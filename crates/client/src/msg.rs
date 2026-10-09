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
use valo_ipc::ClientIPC;
use valo_ipc::messages::{ClientToLib, LibToClient};
use valo_launch::gpu_session::GpuSession;

use crate::config::ConfigArgs;

/// How long a one-shot msg client waits for a reply before reporting a hang.
const HANG_READ_TIMEOUT: Duration = Duration::from_secs(1);
const HELLO_TIMEOUT: Duration = Duration::from_secs(4);

/// Default socket path shared between `spawn` and `msg`.
pub const DEFAULT_SOCK: &str = "/tmp/valo-session.sock";

#[derive(Subcommand)]
pub enum MsgCmd {
    /// Query the current frame counter.
    State,
    /// Draw frames, then pause at the next frame boundary (default: 1).
    Advance {
        #[arg(default_value_t = 1)]
        frames: u64,
    },
    /// Checkpoint at the next frame boundary.
    Checkpoint,
    /// Restore the last checkpoint. Async mode never answers this: execution
    /// resumes inside the earlier checkpoint's continuation, where the stored
    /// connection is the checkpoint-time one (long gone). Block mode does
    /// reply with `RestoreSucceeded`. Verify with `msg state`.
    Restore,
    /// Stop pausing at frame boundaries
    Detach,
    /// Tell the game to quit.
    Quit,
}

pub fn spawn(
    lib: Option<PathBuf>,
    sock: &Path,
    paused: bool,
    config: &ConfigArgs,
    target: &[String],
) -> Result<()> {
    let use_gpu = config.use_gpu;
    let Some(program) = target.first() else {
        bail!("missing program");
    };
    let lib = crate::resolve_lib(lib)?;
    let lib = std::fs::canonicalize(&lib)
        .with_context(|| format!("canonicalize libvalo.so path {}", lib.display()))?;

    let mut cmd = crate::build_command(false, &PathBuf::from(program), &target[1..], &lib);
    valo_launch::configure_target(&mut cmd, use_gpu);
    cmd.env("VALO_SOCK", sock);

    use std::os::unix::process::CommandExt;
    // closing the spawning terminal must not take the session down with it.
    cmd.process_group(0);

    if !use_gpu {
        let child = cmd
            .spawn()
            .with_context(|| format!("spawning {}", cmd.get_program().to_string_lossy()))?;
        init_session(sock, config, paused)?;
        print_spawned(child.id(), sock, paused);
        return Ok(());
    }

    // The GPU session needs an owner that outlives this process, and the
    // owner must be the game's parent to reap it. Fork the session daemon
    // before spawning the game; the daemon reports the game pid (or a
    // startup error) back through a pipe.
    let (status_read, status_write) =
        rustix::pipe::pipe_with(PipeFlags::CLOEXEC).map_err(std::io::Error::from)?;
    // SAFETY: single-threaded here, no locks held across the fork.
    let daemon = unsafe { libc::fork() };
    if daemon < 0 {
        bail!(std::io::Error::last_os_error());
    }

    if daemon > 0 {
        // The parent keeps only the read end.
        drop(status_write);
        let mut status = String::new();
        {
            use std::io::Read;
            std::fs::File::from(status_read).read_to_string(&mut status)?;
        }
        if let Some(pid) = status.strip_prefix("pid ") {
            let pid: u32 = pid.trim().parse().context("daemon status message")?;
            print_spawned(pid, sock, paused);
            return Ok(());
        }
        bail!("session daemon: {}", status.trim());
    }

    // Detached daemon with no controlling terminal.
    drop(status_read);
    rustix::process::setsid().map_err(std::io::Error::from)?;
    let report = |status: &str| {
        use std::io::Write;
        let mut writer = std::fs::File::from(status_write);
        let _ = writer.write_all(status.as_bytes());
    };
    let mut gpu = match GpuSession::start() {
        Ok(session) => session,
        Err(e) => {
            report(&format!("error {e:#}"));
            std::process::exit(1);
        }
    };
    gpu.configure_game(&mut cmd);
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            report(&format!("error {:#}", anyhow::Error::new(e)));
            drop(gpu);
            std::process::exit(1);
        }
    };
    gpu.game_started();
    if let Err(e) = init_session(sock, config, paused) {
        report(&format!("error {e:#}"));
        drop(gpu);
        std::process::exit(1);
    }
    report(&format!("pid {}\n", child.id()));
    let status = child.wait().context("wait for game")?;
    let host_died = crate::report_kumquat_exit(&mut gpu)?;
    // std::process::exit skips destructors; the drop kills kumquat.
    drop(gpu);
    if host_died && !status.success() {
        std::process::exit(1);
    }
    std::process::exit(0);
}

fn print_spawned(pid: u32, sock: &Path, paused: bool) {
    println!(
        "[valo] spawned pid={} sock={}{}",
        pid,
        sock.display(),
        if paused {
            " (paused before frame 0)"
        } else {
            ""
        }
    );
}

/// The spawn process is the session's init client: it delivers the config
/// and either detaches (session runs) or closes (session starts paused).
fn init_session(sock: &Path, config: &ConfigArgs, paused: bool) -> Result<()> {
    let mut conn =
        ClientIPC::establish(sock).with_context(|| format!("connect {}", sock.display()))?;
    conn.init(&config.config())
        .with_context(|| format!("init {}", sock.display()))?;
    if !paused {
        conn.send(&ClientToLib::Detach)?;
    }
    Ok(())
}

pub fn msg(sock: &Path, command: &MsgCmd) -> Result<()> {
    // Connect-only: the lib owns the listener socket
    let mut conn = ClientIPC::connect(sock, Duration::from_secs(3))
        .with_context(|| format!("connect {}: no session listening", sock.display()))?;
    let pid = conn.peer_pid().context("game pid from socket peer")?;
    conn.set_read_timeout(HELLO_TIMEOUT)
        .context("set read timeout")?;
    let current_config = match conn.hello() {
        Ok(config) => config,
        Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
            hang_report(pid, false)?;
            bail!("game did not answer the handshake");
        }
        Err(e) => return Err(e).context("hello"),
    };
    conn.set_read_timeout(HANG_READ_TIMEOUT)
        .context("set command timeout")?;

    let request = match command {
        MsgCmd::State => ClientToLib::GetState,
        MsgCmd::Advance { frames } => ClientToLib::AdvanceFrames(*frames),
        MsgCmd::Checkpoint => ClientToLib::Checkpoint,
        MsgCmd::Restore => ClientToLib::Restore,
        MsgCmd::Detach => ClientToLib::Detach,
        MsgCmd::Quit => ClientToLib::Quit,
    };
    conn.send(&request).context("send command")?;

    if matches!(
        request,
        ClientToLib::AdvanceFrames(_) | ClientToLib::Checkpoint
    ) {
        conn.clear_read_timeout().context("clear command timeout")?;
    }

    // No reply in async mode, by construction: on success valo_restore
    // rewinds and resumes inside the earlier checkpoint's continuation, where
    // the stored connection is the checkpoint-time one-shot stream (dead).
    // Block mode replies with RestoreSucceeded on its persistent connection;
    // this restore client's stream becomes an orphaned fd on the lib side.
    if matches!(request, ClientToLib::Restore) {
        println!("[valo] restore sent (verify with: valo msg state)");
        return Ok(());
    }
    if matches!(request, ClientToLib::Detach) {
        println!("[valo] detached, session runs free");
        return Ok(());
    }

    match recv_reply(&mut conn, pid, HANG_READ_TIMEOUT, current_config.use_gpu)
        .context("recv reply")?
    {
        Some(LibToClient::State(state)) => println!("[valo] frame {}", state.frame),
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

/// A read timeout here usually means the game is hung: report the
/// per-thread wchan state.
fn hang_report(pid: u32, use_gpu: bool) -> Result<()> {
    if std::path::Path::new(&format!("/proc/{pid}")).exists() {
        crate::report_hang(pid, use_gpu)
    } else {
        eprintln!("[valo] game pid {pid} is gone");
        Ok(())
    }
}

fn recv_reply(
    conn: &mut ClientIPC,
    pid: u32,
    timeout: Duration,
    use_gpu: bool,
) -> Result<Option<LibToClient>> {
    match conn.recv() {
        Ok(reply) => Ok(reply),
        Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
            hang_report(pid, use_gpu)?;
            bail!("no reply within {timeout:?}");
        }
        Err(e) => Err(e.into()),
    }
}
