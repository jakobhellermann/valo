// TODO(ai-review): generated, not yet audited
//! `valo spawn` / `valo msg`: detached sessions with one-shot control.
//!
//! `spawn` starts the target as a free or held session; the lib's always
//! bound listener accepts the one-shot `valo msg` clients at frame
//! boundaries.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use rustix::pipe::PipeFlags;
use valo_client_core::Session;
use valo_ipc::ClientIPC;
use valo_ipc::messages::{ClientToLib, InputState, LibToClient};

use crate::config::ConfigArgs;

/// How long a one-shot msg client waits for a reply before reporting a hang.
const HANG_READ_TIMEOUT: Duration = Duration::from_secs(1);
const HELLO_TIMEOUT: Duration = Duration::from_secs(4);

#[derive(Subcommand)]
pub enum MsgCmd {
    /// Get the current state
    State,
    /// Save the game window's contents as a PNG; exact to the frame while held.
    Screenshot {
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Draw frames, then pause at the next frame boundary (default: 1).
    Advance {
        #[arg(default_value_t = 1)]
        frames: u64,
    },
    /// Checkpoint into a savestate slot at the next frame boundary.
    Checkpoint {
        /// Savestate slot to write.
        #[arg(long, default_value_t = 0)]
        slot: u32,
    },
    /// Restore the snapshot in a slot.
    Restore {
        /// Savestate slot to rewind to.
        #[arg(long, default_value_t = 0)]
        slot: u32,
    },
    /// Hold these X keycodes from the next frame on; none releases all.
    Input { keycodes: Vec<u8> },
    /// Stop pausing at frame boundaries
    Detach,
    /// Tell the game to quit.
    Quit,
}

pub fn spawn(
    lib: Option<PathBuf>,
    sock: &Path,
    paused: bool,
    config_args: &ConfigArgs,
    target: &[String],
) -> Result<()> {
    let use_gpu = !config_args.software_rendering;
    let Some(program) = target.first() else {
        bail!("missing program");
    };
    let config = config_args.config();
    let lib = valo_launch::resolve_lib(lib)?;
    let lib = std::fs::canonicalize(&lib)
        .with_context(|| format!("canonicalize libvalo.so path {}", lib.display()))?;

    let mut cmd = crate::build_command(false, &PathBuf::from(program), &target[1..], &lib);
    config_args.apply(&mut cmd)?;
    use std::os::unix::process::CommandExt;
    // closing the spawning terminal must not take the session down with it.
    cmd.process_group(0);

    // The session needs an owner that outlives this process to keep the GPU
    // host alive and clean up after the game. The daemon reports the game
    // pid (or a startup error) back through a pipe.
    let (status_read, status_write) =
        rustix::pipe::pipe_with(PipeFlags::CLOEXEC).map_err(std::io::Error::from)?;
    // SAFETY: single-threaded here, no locks held across the fork.
    let daemon = unsafe { libc::fork() };
    if daemon < 0 {
        bail!(std::io::Error::last_os_error());
    }

    if daemon > 0 {
        drop(status_write);
        let mut status = String::new();
        {
            use std::io::Read;
            std::fs::File::from(status_read).read_to_string(&mut status)?;
        }
        let Some(spawned) = status.strip_prefix("spawned ") else {
            bail!("session daemon: {}", status.trim());
        };
        println!(
            "[valo] spawned {}{}",
            spawned.trim(),
            if paused {
                " (paused before frame 0)"
            } else {
                ""
            }
        );
        return Ok(());
    }

    drop(status_read);
    rustix::process::setsid().map_err(std::io::Error::from)?;
    let report = |status: &str| {
        use std::io::Write;
        let _ = std::fs::File::from(status_write).write_all(status.as_bytes());
    };
    let started = (|| -> Result<(Session, u32)> {
        let mut session = Session::spawn(cmd, sock, use_gpu)?;
        let pid = session
            .connect(&config)?
            .context("target exited before the ipc connection")?;
        // Paused: closing the connection leaves the game held at its first boundary.
        if paused {
            session.disconnect();
        } else {
            session.detach()?;
            // The lib accepts `valo msg` clients only while no client is connected.
            session.disconnect();
        }
        Ok((session, pid))
    })();
    let (mut session, pid) = match started {
        Ok(started) => started,
        Err(e) => {
            report(&format!("error {e:#}"));
            std::process::exit(1);
        }
    };
    let kumquat = session
        .gpu()
        .map(|gpu| format!(" kumquat={}", gpu.pid()))
        .unwrap_or_default();
    report(&format!(
        "spawned pid={pid}{kumquat} sock={}\n",
        sock.display()
    ));
    // Nobody waits for the daemon, so the target's exit code goes nowhere.
    let _ = session.finish(Duration::MAX)?;
    // Dropping the session stops the GPU host; exiting would skip that.
    drop(session);
    std::process::exit(0)
}

pub fn msg(sock: &Path, command: &MsgCmd) -> Result<()> {
    // Connect-only: the lib owns the listener socket
    let mut conn = ClientIPC::connect(sock, Duration::from_secs(3))
        .with_context(|| format!("connect {}: no session listening", sock.display()))?;
    let pid = conn.peer_pid().context("game pid from socket peer")?;
    conn.set_read_timeout(HELLO_TIMEOUT)
        .context("set read timeout")?;
    match conn.hello() {
        Ok(config) => config,
        Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
            valo_client_core::report_hang(pid)?;
            bail!("game did not answer the handshake");
        }
        Err(e) => return Err(e).context("hello"),
    };
    conn.set_read_timeout(HANG_READ_TIMEOUT)
        .context("set command timeout")?;

    let request = match command {
        MsgCmd::State | MsgCmd::Screenshot { .. } => ClientToLib::GetState,
        MsgCmd::Advance { frames } => ClientToLib::AdvanceFrames(*frames),
        MsgCmd::Checkpoint { slot } => ClientToLib::Checkpoint(*slot),
        MsgCmd::Restore { slot } => ClientToLib::Restore(*slot),
        MsgCmd::Input { keycodes } => ClientToLib::SetInputs(held(keycodes)),
        MsgCmd::Detach => ClientToLib::Detach,
        MsgCmd::Quit => ClientToLib::Quit,
    };
    conn.send(&request).context("send command")?;

    if matches!(
        request,
        ClientToLib::AdvanceFrames(_) | ClientToLib::Checkpoint(_)
    ) {
        conn.clear_read_timeout().context("clear command timeout")?;
    }

    // No reply in async mode, by construction: on success valo_restore
    // rewinds and resumes inside the earlier checkpoint's continuation, where
    // the stored connection is the checkpoint-time one-shot stream (dead).
    // Block mode replies with RestoreSucceeded on its persistent connection;
    // this restore client's stream becomes an orphaned fd on the lib side.
    // TODO: check for failed preflight
    if matches!(request, ClientToLib::Restore(_)) {
        println!("[valo] restore sent (verify with: valo msg state)");
        return Ok(());
    }
    if matches!(request, ClientToLib::Detach) {
        println!("[valo] detached, session runs free");
        return Ok(());
    }
    if matches!(request, ClientToLib::SetInputs(_)) {
        println!("[valo] inputs set");
        return Ok(());
    }

    match recv_reply(&mut conn, pid, HANG_READ_TIMEOUT).context("recv reply")? {
        Some(LibToClient::State(state)) => match command {
            // A held session stays at this boundary until the connection closes; a free one runs on.
            MsgCmd::Screenshot { output } => {
                let window = state.game_window.context("the game has no window")?;
                crate::screenshot::capture(window, output)?;
                println!("[valo] frame {}: wrote {}", state.frame, output.display());
            }
            _ => println!("[valo] frame {}", state.frame),
        },
        Some(LibToClient::FrameBoundary(b)) => println!("[valo] frame {}", b.frame),
        Some(LibToClient::Error(error)) => bail!("{}", error.message),
        Some(LibToClient::CheckpointDone) => println!("[valo] checkpoint done"),
        Some(other) => println!("[valo] <- {other:?}"),
        None => {
            if matches!(request, ClientToLib::Quit) {
                println!("[valo] quit sent");
            } else {
                bail!("lib closed without reply");
            }
        }
    }
    Ok(())
}

fn held(keycodes: &[u8]) -> InputState {
    InputState {
        mods: 0,
        keys: keycodes.iter().copied().collect(),
        pointer: [0, 0],
    }
}

fn recv_reply(conn: &mut ClientIPC, pid: u32, timeout: Duration) -> Result<Option<LibToClient>> {
    match conn.recv() {
        Ok(reply) => Ok(reply),
        Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
            valo_client_core::report_hang(pid)?;
            bail!("no reply within {timeout:?}");
        }
        Err(e) => Err(e.into()),
    }
}
