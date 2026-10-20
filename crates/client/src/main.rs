mod config;
mod hud;
mod input;
mod msg;
mod overlay;
mod tui;

use std::fs::File;
use std::io::{ErrorKind, Read};
use std::os::fd::BorrowedFd;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode};
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use config::ConfigArgs;
use valo_ipc::ClientIPC;
use valo_ipc::config::Config;
use valo_ipc::messages::{ClientToLib, FrameBoundary, LibToClient};
use valo_launch::gpu_session::GpuSession;
use valo_launch::{paths, resolve_lib};

/// How long to wait for the next FrameBoundary after sending Advance before assuming the target hung.
const HANG_TIMEOUT: Duration = Duration::from_secs(2);
/// How long to wait for the first frame boundary
const FIRST_HANG_TIMEOUT: Duration = Duration::from_secs(5);

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

        /// Target program to execute.
        #[arg(help_heading = None)]
        program: PathBuf,

        /// Arguments forwarded to the target program.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, help_heading = None)]
        args: Vec<String>,
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
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, help_heading = None)]
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
            program,
            args,
            config,
        } => run(lib, gdb, tui, program, args, config),
        Cmd::Spawn {
            lib,
            sock,
            paused,
            config,
            target,
        } => msg::spawn(
            lib,
            sock.as_deref().unwrap_or(paths::ipc_sock()),
            paused,
            &config,
            &target,
        )
        .map(|_| ExitCode::SUCCESS),
        Cmd::Msg { sock, command } => {
            msg::msg(sock.as_deref().unwrap_or(paths::ipc_sock()), &command)
                .map(|_| ExitCode::SUCCESS)
        }
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
    program: PathBuf,
    args: Vec<String>,
    config: ConfigArgs,
) -> Result<ExitCode> {
    let lib = resolve_lib(lib)?;
    let lib = std::fs::canonicalize(&lib)
        .with_context(|| format!("canonicalize libvalo.so path {}", lib.display()))?;

    if std::env::var_os("VALO_TARGET").is_none() {
        ensure_elf(&program)?;
    }

    let mut cmd = build_command(gdb, &program, &args, &lib);
    let use_gpu = !config.software_rendering;
    valo_launch::command::configure_target(&mut cmd, use_gpu)?;
    let config = config.config();

    if use_tui {
        // Don't let the child's stdout/stderr clobber the alternate screen.
        // The user can still see lib logs via the events panel.
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
    }

    let sock_path = paths::ipc_sock();
    let _ = std::fs::remove_file(sock_path);
    cmd.env("VALO_SOCK", sock_path);

    let log_path = paths::log_path();

    let mut gpu = if use_gpu {
        let session = GpuSession::start()?;
        session.configure_game(&mut cmd);
        Some(session)
    } else {
        None
    };

    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawning {}", cmd.get_program().to_string_lossy()))?;
    let gpu_control = gpu.as_ref().map(GpuSession::control_for_lib);
    let result = if use_tui {
        let tui_result = tui::run(sock_path, Some(log_path.as_path()), config, gpu_control);
        // The alternate screen is gone; report where this session's files landed
        eprintln!(
            "[valo-client] session ended: target pid {}, home {}",
            child.id(),
            paths::valo_home().display()
        );
        eprintln!("[valo-client] log: {}", log_path.display());
        if let Some(gpu) = &gpu {
            eprintln!(
                "[valo-client] kumquat log: {} (host pid {})",
                gpu.log_path().display(),
                gpu.pid()
            );
        }
        tui_result.map(|_| SessionEnd::Done)
    } else {
        run_ipc_loop(sock_path, &mut child, gdb, config, gpu_control)
    };
    if let Err(e) = &result {
        eprintln!("ipc loop ended: {e:#}");
    }

    if let Ok(SessionEnd::Hung { pid: game_pid }) = result
        && !gdb
    {
        eprintln!("[valo-client] core dump: coredumpctl debug {game_pid}");
        eprintln!("[valo-client] valo log: {}", log_path.display());

        abort_game(game_pid);

        if let Some(gpu) = gpu.as_mut() {
            if gpu.exit_status()?.is_none() {
                gpu.abort()?;
            }

            report_kumquat_exit(gpu)?;
        }
        wait_or_kill(&mut child, Duration::from_secs(10))?;
        let _ = std::fs::remove_file(sock_path);
        remove_savestates(&mut child);

        return Ok(ExitCode::from(125));
    }

    // In gdb, don't kill after the inferior process stops.
    let status = if gdb {
        child.wait().context("wait for gdb")?
    } else {
        wait_or_kill(&mut child, Duration::from_millis(500))?
    };
    eprintln!("[valo-client] target (pid {}) exited: {status}", child.id());
    let _ = std::fs::remove_file(sock_path);
    let host_died = gpu
        .as_mut()
        .map(report_kumquat_exit)
        .transpose()?
        .unwrap_or(false);
    if host_died && status.success() {
        remove_savestates(&mut child);
        return Ok(ExitCode::from(125));
    }
    remove_savestates(&mut child);

    if let Some(code) = status.code() {
        Ok(ExitCode::from(u8::try_from(code).unwrap_or(1)))
    } else if let Some(sig) = status.signal() {
        Ok(ExitCode::from(u8::try_from(128 + sig).unwrap_or(255)))
    } else {
        Ok(ExitCode::from(1))
    }
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

pub(crate) fn report_kumquat_exit(session: &mut GpuSession) -> Result<bool> {
    let Some(status) = session.exit_status()? else {
        return Ok(false);
    };

    let pid = session.pid();
    let aborted = status.signal() == Some(rustix::process::Signal::ABORT.as_raw());
    if aborted {
        eprintln!("[valo-client] kumquat core dump: coredumpctl debug {pid}");
    } else {
        eprintln!("[valo-client] kumquat (pid {pid}) exited: {status}");
    }

    eprintln!(
        "[valo-client] kumquat log: {}",
        session.log_path().display()
    );

    Ok(true)
}

fn run_ipc_loop(
    sock: &Path,
    child: &mut Child,
    gdb: bool,
    config: Config,
    gpu_control: Option<BorrowedFd<'_>>,
) -> Result<SessionEnd> {
    // Bound to the first attached game; a gdb rerun keeps it.
    let mut game_ui = None;
    let game_window = input::GameWindow::default();
    loop {
        let Some(mut conn) = establish_or_child_exit(sock, child)? else {
            eprintln!("[valo-client] child exited before ipc connection");
            return Ok(SessionEnd::Done);
        };
        eprintln!("[valo-client] ipc connected on {}", sock.display());

        let pid = valo_launch::init_game(&mut conn, &config, gpu_control)?;
        conn.set_read_timeout(FIRST_HANG_TIMEOUT)
            .context("set hang watchdog timeout")?;
        let (hotkeys, hud) = game_ui.get_or_insert_with(|| spawn_game_ui(game_window.clone()));
        let mut first_boundary = true;
        loop {
            let msg = match conn.recv() {
                Ok(Some(m)) => m,
                Ok(None) => {
                    eprintln!("[valo-client] lib hung up");
                    break;
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    report_hang(pid)?;
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
                        hud.sent(&cmd);
                    }
                    conn.send(&ClientToLib::AdvanceFrames(1))
                        .context("send advance")?;
                }
                other => {
                    hud.received(&other);
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

fn spawn_game_ui(game_window: input::GameWindow) -> (mpsc::Receiver<input::HotkeyEvent>, hud::Hud) {
    let (hotkey_tx, hotkey_rx) = mpsc::channel::<input::HotkeyEvent>();
    input::spawn_thread(game_window.clone(), move |ev| {
        let _ = hotkey_tx.send(ev);
    });
    let overlay = overlay::spawn_thread(game_window, |ev| match ev {
        overlay::OverlayEvent::Status(msg) => eprintln!("[valo-client] overlay: {msg}"),
        overlay::OverlayEvent::Fatal(msg) => eprintln!("[valo-client] overlay failed: {msg}"),
    });
    (hotkey_rx, hud::Hud::new(overlay))
}

/// SIGABRT the game, which is not the direct child behind a wrapper.
fn abort_game(pid: u32) {
    let pid = rustix::process::Pid::from_raw(pid.cast_signed()).expect("game pid is nonzero");
    if let Err(e) = rustix::process::kill_process(pid, rustix::process::Signal::ABORT) {
        eprintln!("[valo-client] abort game pid {pid}: {e}");
    }
}

pub(crate) fn report_hang(pid: u32) -> Result<()> {
    eprintln!(
        "[valo-client] hang: no frame boundary for {:?} (pid {pid})",
        HANG_TIMEOUT
    );

    for entry in std::fs::read_dir(format!("/proc/{pid}/task"))? {
        let tid: u32 = entry?.file_name().to_string_lossy().parse()?;
        let wchan = std::fs::read_to_string(format!("/proc/{pid}/task/{tid}/wchan"))?;
        let comm = std::fs::read_to_string(format!("/proc/{pid}/task/{tid}/comm"))?;
        eprintln!(
            "[valo-client]   tid {tid} ({}) wchan={}",
            comm.trim(),
            wchan.trim()
        );
    }
    Ok(())
}

/// Like [`ClientIPC::establish`], but gives up when the child exits first
/// (e.g. the user quit gdb without ever starting the inferior).
/// Returns `Ok(None)` in that case.
fn establish_or_child_exit(sock: &Path, child: &mut Child) -> Result<Option<ClientIPC>> {
    let sock = sock.to_owned();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(ClientIPC::establish(&sock));
    });
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(conn) => return Ok(Some(conn.context("ipc establish")?)),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if child.try_wait().context("try_wait child")?.is_some() {
                    return Ok(None);
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                bail!("ipc establish thread died without result")
            }
        }
    }
}

/// Wait for the child to exit. If it doesn't exit within the grace
/// period, SIGKILL it — covers the case where a failed restore left the
/// child deadlocked and `wait()` would otherwise block forever.
fn wait_or_kill(child: &mut Child, grace: Duration) -> Result<std::process::ExitStatus> {
    let poll = Duration::from_millis(50);
    let start = std::time::Instant::now();
    loop {
        match child.try_wait().context("try_wait child")? {
            Some(status) => return Ok(status),
            None => {
                if start.elapsed() >= grace {
                    eprintln!("[valo] child did not exit within {grace:?}, killing");
                    let _ = child.kill();
                    return child.wait().context("wait after kill");
                }
                std::thread::sleep(poll);
            }
        }
    }
}

/// Remove the run's savestate data when the target ist gone
fn remove_savestates(child: &mut Child) {
    if child.try_wait().is_ok_and(|s| s.is_some()) {
        valo_launch::cleanup::remove_savestate_data();
    }
}

pub(crate) fn build_command(gdb: bool, program: &PathBuf, args: &[String], lib: &Path) -> Command {
    let ld_preload = prepend_ld_preload(lib);

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

fn prepend_ld_preload(lib: &Path) -> std::ffi::OsString {
    match std::env::var_os("LD_PRELOAD") {
        Some(existing) if !existing.is_empty() => {
            let mut v = lib.as_os_str().to_owned();
            v.push(":");
            v.push(&existing);
            v
        }
        _ => lib.as_os_str().to_owned(),
    }
}
