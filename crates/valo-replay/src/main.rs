//! Frame-loop replay driver: runs a target under libvalo and drives it from
//! a timeline script, Celeste-TAS style.
//!
//! Script lines: `<frames>,<KEY>[,<KEY>...]` holds the keys for that many
//! frames (whole-state per block, `5,R` = five frames of Right and nothing
//! else), command lines run at the current boundary — `checkpoint <slot>`,
//! `restore <slot>`, `quit`. The script expands into a frame-indexed
//! timeline, and the driver is stateless per boundary, so a restore (which
//! rewinds the lib's frame) resynchronizes on its own.
//!
//! Example:
//!
//! ```text
//! 30              # 30 frames of nothing
//! 60,R            # hold Right
//! checkpoint 0
//! 60,R
//! restore 0
//! 5,U
//! quit
//! ```

use std::collections::BTreeMap;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
use valo_ipc::ClientIPC;
use valo_ipc::config::Config;
use valo_ipc::messages::{
    ClientToLib, FrameBoundary, InputState, KeyBitset, LibToClient, RestoreSucceeded,
};
use valo_launch::gpu_session::GpuSession;
use valo_launch::{paths, resolve_lib};

#[derive(Parser)]
#[command(
    name = "valo-replay",
    about = "scripted frame-loop driver for libvalo targets"
)]
struct Args {
    /// Path to libvalo.so. Defaults to libvalo.so next to this binary.
    #[arg(long)]
    lib: Option<PathBuf>,

    /// GPU checkpoint orchestration (kumquat host, renderer selection);
    /// Unity targets get -force-vulkan.
    #[arg(long)]
    use_gpu: bool,

    /// Timeline script.
    script: PathBuf,

    /// Target program.
    program: PathBuf,

    /// Arguments forwarded to the target program.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

impl Timeline {
    /// Frames until the next boundary the driver must attend: a key-state
    /// change, a command, or the end of the script.
    fn next_event(&self, frame: u64) -> u64 {
        let start = self.keys.get(&frame);
        let mut f = frame + 1;
        while f < self.end_frame {
            if self.commands.contains_key(&f) || self.keys.get(&f) != start {
                return f;
            }
            f += 1;
        }
        self.end_frame
    }
}

enum Cmd {
    Checkpoint(u32),
    Restore(u32),
    Quit,
}

/// Expanded script, indexed by frame number.
struct Timeline {
    /// Keyboard state held during frame f; absent = nothing held.
    keys: BTreeMap<u64, InputState>,
    /// Commands to run at boundary f, before frame f executes.
    commands: BTreeMap<u64, Vec<Cmd>>,
    /// Script lines that become active at boundary f, for progress logging.
    lines: BTreeMap<u64, Vec<String>>,
    /// First frame after the script.
    end_frame: u64,
}

/// Commands run once per session: a restore rewinds the lib, not this
/// driver, so an executed restore must not re-fire on the replayed range.
#[derive(Default)]
struct Executed(BTreeMap<u64, usize>);

fn main() -> Result<()> {
    let args = Args::parse();
    let timeline = parse_script(
        &std::fs::read_to_string(&args.script)
            .with_context(|| format!("reading {}", args.script.display()))?,
    )?;
    let lib = resolve_lib(args.lib)?;
    eprintln!("[valo-replay] lib {}", lib.display());
    let mut gpu = if args.use_gpu {
        let session = GpuSession::start().context("start kumquat session")?;
        Some(session)
    } else {
        None
    };
    let mut child = spawn(&lib, &args.program, &args.args, gpu.as_mut())?;
    if let Some(gpu) = &mut gpu {
        gpu.game_started();
    }
    let result = drive(&mut child, &timeline, args.use_gpu);
    if let Err(e) = &result {
        let _ = child.kill();
        let _ = child.wait();
        valo_launch::cleanup::remove_savestate_data();
        return Err(anyhow!("{e:#}"));
    }
    Ok(())
}

fn parse_script(source: &str) -> Result<Timeline> {
    let mut keys = BTreeMap::new();
    let mut commands: BTreeMap<u64, Vec<Cmd>> = BTreeMap::new();
    let mut lines: BTreeMap<u64, Vec<String>> = BTreeMap::new();
    let mut cursor = 0u64;
    for (no, line) in source.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let Some((count, held)) = line.split_once(',') else {
            if let Ok(count) = line.parse::<u64>() {
                if count == 0 {
                    bail!("line {}: frame count must be > 0", no + 1);
                }
                lines
                    .entry(cursor)
                    .or_default()
                    .push(format!("line {}: {line}", no + 1));
                cursor += count;
                continue;
            }
            commands
                .entry(cursor)
                .or_default()
                .push(parse_command(line, no)?);
            lines
                .entry(cursor)
                .or_default()
                .push(format!("line {}: {line}", no + 1));
            continue;
        };
        let count: u64 = count
            .trim()
            .parse()
            .with_context(|| format!("line {}: frame count '{count}'", no + 1))?;
        if count == 0 {
            bail!("line {}: frame count must be > 0", no + 1);
        }
        let state = parse_keys(held, no)?;
        if state.keys != KeyBitset::default() {
            for f in cursor..cursor + count {
                keys.insert(f, state);
            }
        }
        lines
            .entry(cursor)
            .or_default()
            .push(format!("line {}: {line}", no + 1));
        cursor += count;
    }
    Ok(Timeline {
        keys,
        commands,
        lines,
        end_frame: cursor,
    })
}

fn parse_command(line: &str, no: usize) -> Result<Cmd> {
    let mut parts = line.split_whitespace();
    match parts.next() {
        Some("checkpoint") => parts
            .next()
            .and_then(|s| s.parse::<u32>().ok().map(Cmd::Checkpoint))
            .ok_or_else(|| anyhow!("line {}: 'checkpoint <slot>'", no + 1)),
        Some("restore") => parts
            .next()
            .and_then(|s| s.parse::<u32>().ok().map(Cmd::Restore))
            .ok_or_else(|| anyhow!("line {}: 'restore <slot>'", no + 1)),
        Some("quit") => Ok(Cmd::Quit),
        other => bail!("line {}: unknown command '{other:?}'", no + 1),
    }
}

/// Key names → X keycodes (evdev); `kc<N>` addresses a raw keycode.
fn parse_keys(held: &str, no: usize) -> Result<InputState> {
    let mut state = InputState {
        mods: 0,
        keys: KeyBitset::default(),
        pointer: [0, 0],
    };
    for key in held.split(',').filter(|k| !k.trim().is_empty()) {
        let key = key.trim();
        let keycode: u8 = match key {
            "U" => 111,
            "D" => 116,
            "L" => 113,
            "R" => 114,
            "ENTER" => 36,
            "ESC" => 9,
            _ if key.starts_with("kc") => key[2..]
                .parse()
                .with_context(|| format!("line {}: keycode '{key}'", no + 1))?,
            _ => bail!("line {}: unknown key '{key}'", no + 1),
        };
        state.keys.set(keycode, true);
    }
    Ok(state)
}

/// Refuse to reuse a run home that a live session still owns; a socket
/// file without a listener is a leftover and gets removed.
fn claim_sock(sock: &Path) -> Result<()> {
    if !sock.exists() {
        return Ok(());
    }
    match ClientIPC::connect(sock, Duration::from_millis(200)) {
        Ok(_) => bail!(
            "a live session owns {}; stop it or point VALO_HOME elsewhere",
            sock.display()
        ),
        Err(_) => {
            let _ = std::fs::remove_file(sock);
        }
    }
    Ok(())
}

fn spawn(
    lib: &Path,
    program: &Path,
    args: &[String],
    gpu: Option<&mut GpuSession>,
) -> Result<Child> {
    claim_sock(paths::ipc_sock())?;
    let mut cmd = Command::new(program);
    cmd.args(args);
    cmd.env("LD_PRELOAD", lib);
    // Kill the target if this driver dies, so a killed/timeouted run
    // never leaves an orphaned game behind.
    // SAFETY: single-threaded driver, no state to corrupt in the child.
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            Ok(())
        });
    }
    if let Some(gpu) = gpu {
        gpu.configure_game(&mut cmd);
        valo_launch::command::configure_target(&mut cmd, true)?;
    } else {
        valo_launch::command::configure_target(&mut cmd, false)?;
    }
    cmd.env("VALO_SOCK", paths::ipc_sock());
    cmd.stdout(Stdio::inherit());
    cmd.stderr(Stdio::inherit());
    cmd.spawn()
        .with_context(|| format!("spawning {}", program.display()))
}

fn drive(child: &mut Child, timeline: &Timeline, use_gpu: bool) -> Result<()> {
    let sock = paths::ipc_sock();
    let mut conn = connect_or_child_exit(sock, child)?;
    let config = Config {
        use_gpu,
        ..Config::default()
    };
    conn.init(&config)?;
    eprintln!("[valo-replay] driving until frame {}", timeline.end_frame);
    let mut executed = Executed::default();

    'boundary: loop {
        match conn.recv()? {
            Some(
                LibToClient::FrameBoundary(FrameBoundary { frame, .. })
                | LibToClient::RestoreSucceeded(RestoreSucceeded { frame }),
            ) => {
                for entry in timeline.lines.get(&frame).into_iter().flatten() {
                    eprintln!("[valo-replay] frame {frame}: {entry}");
                }
                if let Some(cmds) = timeline.commands.get(&frame) {
                    let done = executed.0.entry(frame).or_default();
                    for (idx, cmd) in cmds.iter().enumerate() {
                        if *done > idx {
                            continue;
                        }
                        *done = idx + 1;
                        match cmd {
                            Cmd::Checkpoint(slot) => {
                                conn.send(&ClientToLib::Checkpoint(*slot))?;
                                expect(&mut conn, |m| matches!(m, LibToClient::CheckpointDone))?;
                            }
                            Cmd::Restore(slot) => {
                                // Last command of this turn: the rewind
                                // resumes mid-turn, so anything sent now
                                // would run into the rewound session.
                                conn.send(&ClientToLib::Restore(*slot))?;
                                continue 'boundary;
                            }
                            Cmd::Quit => {
                                conn.send(&ClientToLib::Quit)?;
                                eprintln!("[valo-replay] quit at frame {frame}");
                                remove_savestates(child);
                                return Ok(());
                            }
                        }
                    }
                }
                if frame >= timeline.end_frame {
                    conn.send(&ClientToLib::Detach)?;
                    eprintln!(
                        "[valo-replay] script done at frame {frame}, detached (pid {})",
                        child.id()
                    );
                    return Ok(());
                }
                let state = timeline.keys.get(&frame).cloned().unwrap_or(InputState {
                    mods: 0,
                    keys: KeyBitset::default(),
                    pointer: [0, 0],
                });
                let next = timeline.next_event(frame);
                conn.send(&ClientToLib::SetInputs(state))?;
                conn.send(&ClientToLib::AdvanceFrames(next - frame))?;
            }
            Some(LibToClient::Error(e)) => bail!("lib error: {}", e.message),
            Some(other) => bail!("unexpected message: {other:?}"),
            None => bail!("lib hung up at the frame boundary"),
        }
    }
}

/// Remove the run's savestate bulk data once the target exited. A Quit
/// target exits on its own at the next boundary; if it does not within a
/// short window, the home stays and a later run sweeps it after the exit.
fn remove_savestates(child: &mut Child) {
    for _ in 0..100 {
        if child.try_wait().is_ok_and(|status| status.is_some()) {
            valo_launch::cleanup::remove_savestate_data();
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn expect(conn: &mut ClientIPC, is: impl Fn(&LibToClient) -> bool) -> Result<()> {
    match conn.recv()? {
        Some(m) if is(&m) => Ok(()),
        Some(other) => bail!("unexpected reply: {other:?}"),
        None => bail!("lib hung up waiting for the command reply"),
    }
}

fn connect_or_child_exit(sock: &Path, child: &mut Child) -> Result<ClientIPC> {
    loop {
        match ClientIPC::connect(sock, Duration::from_millis(100)) {
            Ok(conn) => return Ok(conn),
            Err(_) => {
                if let Ok(Some(status)) = child.try_wait() {
                    bail!("target exited (status {status}) before the ipc connection");
                }
            }
        }
    }
}
