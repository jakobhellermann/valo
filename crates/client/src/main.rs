mod config;
mod input;
mod msg;
mod screenshot;
mod tui;

use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use config::ConfigArgs;
use valo_client_core::Session;
use valo_ipc::config::Config;
use valo_ipc::messages::{ClientToLib, FrameBoundary, LibToClient};
use valo_launch::bind::bind_mounts;
use valo_launch::{paths, resolve_lib};

/// How long to wait for the next FrameBoundary after sending Advance before assuming the target hung.
const HANG_TIMEOUT: Duration = Duration::from_secs(5);
/// How long to wait for the first frame boundary
const FIRST_HANG_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Parser)]
#[command(
    name = "valo",
    about = "Run programs with checkpoint/restore support and forced determinism",
    disable_help_flag = true,
    disable_help_subcommand = true
)]
struct Cli {
    /// Print help
    #[arg(short = 'h', long, action = clap::ArgAction::HelpShort, hide = true, global = true)]
    help: Option<bool>,

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
#[command(verbatim_doc_comment)]
enum Cmd {
    /// Run a binary through valo.
    Run {
        /// Path to libvalo.so. Defaults to libvalo.so next to the valo binary.
        #[arg(long, help_heading = "Runtime", value_name = "PATH")]
        lib: Option<PathBuf>,

        #[command(flatten)]
        config: ConfigArgs,

        /// Run the target inside gdb.
        #[arg(long, conflicts_with = "tui", help_heading = "Runtime")]
        gdb: bool,

        /// Show a TUI for play/pause/snapshot/restore
        #[arg(long, help_heading = "Runtime")]
        tui: bool,

        /// Target program and arguments.
        #[arg(required = true, trailing_var_arg = true, help_heading = None)]
        target: Vec<String>,
    },
    /// Spawn a detached target.
    Spawn {
        /// Path to libvalo.so. Defaults to libvalo.so next to the valo binary.
        #[arg(long, help_heading = "Runtime")]
        lib: Option<PathBuf>,

        /// Socket path shared with `valo msg`.
        #[arg(long)]
        sock: Option<PathBuf>,

        /// Stop before drawing the first frame; use `valo msg advance N`.
        #[arg(long)]
        paused: bool,

        #[command(flatten)]
        config: ConfigArgs,

        /// Target program and arguments.
        #[arg(trailing_var_arg = true, help_heading = None)]
        target: Vec<String>,
    },

    /// Send a command to a `valo spawn` session.
    Msg {
        /// Socket path of the spawned session.
        #[arg(long, global = true)]
        sock: Option<PathBuf>,

        #[command(subcommand)]
        command: msg::MsgCmd,
    },
}

fn main() -> Result<ExitCode> {
    let cli = Cli::parse();
    match cli.command {
        Cmd::Run {
            lib,
            gdb,
            tui,
            target,
            config,
        } => run(lib, gdb, tui, &target, config),
        Cmd::Spawn {
            lib,
            sock,
            paused,
            config,
            target,
        } => msg::spawn(
            lib,
            sock.as_deref().unwrap_or_else(|| paths::ipc_sock()),
            paused,
            &config,
            &target,
        )
        .map(|_| ExitCode::SUCCESS),
        Cmd::Msg { sock, command } => msg::msg(
            sock.as_deref().unwrap_or_else(|| paths::ipc_sock()),
            &command,
        )
        .map(|_| ExitCode::SUCCESS),
    }
}

#[derive(PartialEq)]
enum SessionEnd {
    /// Finished normally
    Done,
    /// Frame watchdog expired
    Hung { pid: u32 },
}

fn run(
    lib: Option<PathBuf>,
    gdb: bool,
    use_tui: bool,
    target: &[String],
    config: ConfigArgs,
) -> Result<ExitCode> {
    let (program, args) = target.split_first().expect("clap requires a target");
    let program = PathBuf::from(program);
    let lib = resolve_lib(lib)?;
    let lib = std::fs::canonicalize(&lib)
        .with_context(|| format!("canonicalize libvalo.so path {}", lib.display()))?;

    if std::env::var_os("VALO_TARGET").is_none() {
        ensure_elf(&program)?;
    }

    let mut cmd = build_command(gdb, &program, args, &lib);
    bind_mounts(&mut cmd, &config.bind)?;
    let use_gpu = !config.software_rendering;
    let config = config.config();

    if use_tui {
        // Don't let the child's stdout/stderr clobber the alternate screen.
        // The user can still see lib logs via the events panel.
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
    }

    let log_path = paths::log_path();
    let mut session = Session::spawn(cmd, paths::ipc_sock(), use_gpu)?;
    let result = if use_tui {
        let tui_result = tui::run(&mut session, Some(log_path.as_path()), config);
        // The alternate screen is gone; report where this session's files landed
        eprintln!(
            "[valo-client] session ended: target pid {}, home {}",
            session.child_pid(),
            paths::valo_home().display()
        );
        eprintln!("[valo-client] log: {}", log_path.display());
        if let Some(gpu) = session.gpu() {
            eprintln!(
                "[valo-client] kumquat log: {} (host pid {})",
                gpu.log_path().display(),
                gpu.pid()
            );
        }
        tui_result.map(|_| SessionEnd::Done)
    } else {
        run_ipc_loop(&mut session, gdb, config)
    };
    if let Err(e) = &result {
        eprintln!("ipc loop ended: {e:#}");
    }

    if let Ok(SessionEnd::Hung { pid: game_pid }) = result
        && !gdb
    {
        return session.abort_hung(game_pid);
    }

    // In gdb, don't kill after the inferior process stops.
    let grace = if gdb {
        Duration::MAX
    } else {
        Duration::from_millis(500)
    };
    session.finish(grace)
}

/// Without `VALO_TARGET`, libvalo attaches to the first process it is preloaded into, so a wrapper script would capture it.
fn ensure_elf(program: &Path) -> Result<()> {
    let path = resolve_program(program)?;
    let mut magic = [0u8; 4];
    File::open(&path)
        .and_then(|mut f| f.read_exact(&mut magic))
        .with_context(|| format!("read ELF header of {}", path.display()))?;
    if &magic != b"\x7fELF" {
        bail!("{} is not an ELF executable", path.display());
    }
    Ok(())
}

fn resolve_program(program: &Path) -> Result<PathBuf> {
    if program.components().count() > 1 {
        return Ok(program.to_owned());
    }
    std::env::var_os("PATH")
        .iter()
        .flat_map(std::env::split_paths)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
        .with_context(|| format!("{} not found in PATH", program.display()))
}

fn run_ipc_loop(session: &mut Session, gdb: bool, config: Config) -> Result<SessionEnd> {
    // Bound to the first attached game; a gdb rerun keeps it.
    let mut game_ui = None;
    let game_window = input::GameWindow::default();
    loop {
        let Some(pid) = session.connect(&config)? else {
            eprintln!("[valo-client] child exited before ipc connection");
            return Ok(SessionEnd::Done);
        };
        eprintln!(
            "[valo-client] ipc connected on {}",
            paths::ipc_sock().display()
        );
        let conn = session.conn();
        conn.set_read_timeout(FIRST_HANG_TIMEOUT)
            .context("set hang watchdog timeout")?;
        let hotkeys = game_ui.get_or_insert_with(|| spawn_hotkeys(game_window.clone()));
        let mut first_boundary = true;
        loop {
            let msg = match conn.recv() {
                Ok(Some(m)) => m,
                Ok(None) => {
                    eprintln!("[valo-client] lib hung up");
                    break;
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    valo_client_core::report_hang(pid)?;
                    return Ok(SessionEnd::Hung { pid });
                }
                Err(e) => return Err(e).context("ipc recv"),
            };
            match msg {
                LibToClient::FrameBoundary(FrameBoundary {
                    frame: _,
                    game_window: window,
                }) => {
                    game_window.store(window.unwrap_or(0), Ordering::Relaxed);
                    if first_boundary {
                        first_boundary = false;
                        conn.set_read_timeout(HANG_TIMEOUT)
                            .context("set hang timeout")?;
                    }
                    // Send hotkey-triggered commands before advancing; the
                    // lib executes Checkpoint/Restore synchronously and the
                    // response arrives as the next message.
                    while let Ok(event) = hotkeys.try_recv() {
                        let cmd = match event {
                            input::HotkeyEvent::Attached(window) => {
                                eprintln!(
                                    "[valo-client] game hotkeys: attached to window {window:#x}"
                                );
                                continue;
                            }
                            input::HotkeyEvent::Fatal(msg) => {
                                eprintln!("[valo-client] game hotkeys: {msg}");
                                continue;
                            }
                            input::HotkeyEvent::Action(input::HotkeyAction::Save(slot)) => {
                                eprintln!("[valo-client] hotkey: checkpoint slot {slot}");
                                ClientToLib::Checkpoint(slot)
                            }
                            input::HotkeyEvent::Action(input::HotkeyAction::Load(slot)) => {
                                eprintln!("[valo-client] hotkey: restore slot {slot}");
                                ClientToLib::Restore(slot)
                            }
                        };
                        conn.send(&cmd).context("send hotkey command")?;
                    }
                    conn.send(&ClientToLib::AdvanceFrames(1))
                        .context("send advance")?;
                }
                other => {
                    eprintln!("[valo-client] ipc <- {other:?}");
                }
            }
        }

        // gdb session may run the inferior multiple times
        if !gdb {
            return Ok(SessionEnd::Done);
        }
    }
}

fn spawn_hotkeys(game_window: input::GameWindow) -> mpsc::Receiver<input::HotkeyEvent> {
    let (hotkey_tx, hotkey_rx) = mpsc::channel::<input::HotkeyEvent>();
    input::spawn_thread(game_window, move |ev| {
        let _ = hotkey_tx.send(ev);
    });
    hotkey_rx
}

pub(crate) fn build_command(gdb: bool, program: &PathBuf, args: &[String], lib: &Path) -> Command {
    let ld_preload = valo_client_core::ld_preload(lib, &[]);

    if gdb {
        // gdb -ex 'handle SIGSYS nostop noprint' --args <program> [args...]
        let mut cmd = Command::new("gdb");
        cmd.arg("-ex")
            .arg("python gdb.execute('handle SIGSYS nostop noprint', to_string=True)") // suppress gdb signal table result
            .arg("-ex")
            .arg("set startup-with-shell off")
            .arg("-ex")
            .arg(format!(
                "set environment LD_PRELOAD={}",
                ld_preload.to_str().expect("valid utf8")
            ))
            .arg("--args")
            .arg(program)
            .args(args);
        cmd
    } else {
        let mut cmd = Command::new(program);
        cmd.args(args);
        cmd.env("LD_PRELOAD", ld_preload);
        cmd
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STEAM_COMMAND: [&str; 5] = ["steam-launch-wrapper", "--", "reaper", "--", "game"];

    #[test]
    fn run_forwards_double_dash_to_target() {
        let cli = Cli::try_parse_from(
            ["valo", "run", "--uncontrolled"]
                .into_iter()
                .chain(STEAM_COMMAND),
        )
        .unwrap();
        let Cmd::Run { target, .. } = cli.command else {
            panic!("expected run");
        };
        assert_eq!(target, STEAM_COMMAND);
    }

    #[test]
    fn run_forwards_target_flags() {
        let cli = Cli::try_parse_from(["valo", "run", "vkcube", "--present_mode", "0"]).unwrap();
        let Cmd::Run { target, .. } = cli.command else {
            panic!("expected run");
        };
        assert_eq!(target, ["vkcube", "--present_mode", "0"]);
    }

    #[test]
    fn run_rejects_unknown_flag_before_target() {
        let err = Cli::try_parse_from(["valo", "run", "--typo", "vkcube"])
            .err()
            .expect("--typo must not become the target");
        assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument);
    }

    #[test]
    fn spawn_forwards_double_dash_to_target() {
        let cli = Cli::try_parse_from(["valo", "spawn"].into_iter().chain(STEAM_COMMAND)).unwrap();
        let Cmd::Spawn { target, .. } = cli.command else {
            panic!("expected spawn");
        };
        assert_eq!(target, STEAM_COMMAND);
    }

    #[test]
    fn spawn_rejects_unknown_flag_before_target() {
        let err = Cli::try_parse_from(["valo", "spawn", "--typo", "vkcube"])
            .err()
            .expect("--typo must not become the target");
        assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument);
    }
}
