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
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
use valo_client_core::Session;
use valo_ipc::config::Config;
use valo_ipc::messages::{
    ClientToLib, FrameBoundary, InputState, KeyBitset, LibToClient, RestoreSucceeded,
};
use valo_launch::resolve_lib;

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
    let cmd = valo_client_core::target_command(&args.program, &args.args, &lib, &[]);
    let mut session = Session::spawn(cmd, valo_launch::paths::ipc_sock(), args.use_gpu)?;
    if session.connect(&Config::default())?.is_none() {
        bail!("target exited before the ipc connection");
    }
    drive(session, &timeline).map_err(|e| anyhow!("{e:#}"))
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

fn drive(mut session: Session, timeline: &Timeline) -> Result<()> {
    eprintln!("[valo-replay] driving until frame {}", timeline.end_frame);
    let mut executed = Executed::default();

    'boundary: loop {
        match session.recv()? {
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
                                if let Err(e) = session.checkpoint(*slot)? {
                                    bail!("checkpoint {slot}: {}", e.0);
                                }
                            }
                            Cmd::Restore(slot) => {
                                // Last command of this turn: the rewind
                                // resumes mid-turn, so anything sent now
                                // would run into the rewound session.
                                session.send(&ClientToLib::Restore(*slot))?;
                                continue 'boundary;
                            }
                            Cmd::Quit => {
                                eprintln!("[valo-replay] quit at frame {frame}");
                                session.quit()?;
                                session.wait(Duration::from_secs(1))?;
                                return Ok(());
                            }
                        }
                    }
                }
                if frame >= timeline.end_frame {
                    eprintln!(
                        "[valo-replay] script done at frame {frame}, detached (pid {})",
                        session.child_pid()
                    );
                    session.detach()?;
                    session.release();
                    return Ok(());
                }
                let state = timeline.keys.get(&frame).cloned().unwrap_or(InputState {
                    mods: 0,
                    keys: KeyBitset::default(),
                    pointer: [0, 0],
                });
                let next = timeline.next_event(frame);
                session.send(&ClientToLib::SetInputs(state))?;
                session.send(&ClientToLib::AdvanceFrames(next - frame))?;
            }
            Some(LibToClient::Error(e)) => bail!("lib error: {}", e.message),
            Some(other) => bail!("unexpected message: {other:?}"),
            None => bail!("lib hung up at the frame boundary"),
        }
    }
}
