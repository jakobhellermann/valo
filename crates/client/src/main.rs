mod config;
mod msg;
mod tui;

use std::io::ErrorKind;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use config::ConfigArgs;
use valo_ipc::ClientIPC;
use valo_ipc::config::Config;
use valo_ipc::messages::{ClientToLib, FrameBoundary, LibToClient};
use valo_launch::gpu_session::GpuSession;
use valo_launch::paths;

/// How long to wait for the next FrameBoundary after sending Advance
/// before assuming the target hung.
/// On expiry the client prints a hang report to stderr, and keeps waiting.
const HANG_TIMEOUT: Duration = Duration::from_secs(2);
const GPU_HANG_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Parser)]
#[command(name = "valo", about = "valo client")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run a binary with libvalo.so injected via LD_PRELOAD.
    Run {
        /// Path to libvalo.so. Defaults to libvalo.so next to the valo binary.
        #[arg(long)]
        lib: Option<PathBuf>,

        /// Run the target inside gdb (gdb --args \<program\> [args...]).
        #[arg(long, conflicts_with = "tui")]
        gdb: bool,

        /// Show a TUI for play/pause/snapshot/restore instead of
        /// auto-advancing every frame.
        #[arg(long)]
        tui: bool,

        #[command(flatten)]
        config: ConfigArgs,

        /// Target program to execute.
        program: PathBuf,

        /// Arguments forwarded to the target program.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Spawn a detached target. Control via `valo msg`.
    Spawn {
        /// Path to libvalo.so. Defaults to libvalo.so next to the valo binary.
        #[arg(long)]
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
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        target: Vec<String>,
    },

    /// Send a command to a `valo spawn` session.
    Msg {
        /// Socket path of the spawned session.
        #[arg(long)]
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
    Hung,
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

    let mut cmd = build_command(gdb, &program, &args, &lib);
    let use_gpu = config.use_gpu;
    valo_launch::command::configure_target(&mut cmd, use_gpu);

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
    if let Some(gpu) = &mut gpu {
        gpu.game_started();
    }

    let result = if use_tui {
        let tui_result = tui::run(
            sock_path,
            Some(log_path.as_path()),
            child.id(),
            config.config(),
        );
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
        run_ipc_loop(sock_path, &mut child, gdb, config.config())
    };
    if let Err(e) = &result {
        eprintln!("ipc loop ended: {e:#}");
    }
    // Best-effort cleanup of the socket file.
    let _ = std::fs::remove_file(sock_path);

    if matches!(result, Ok(SessionEnd::Hung)) && !gdb {
        let host_died = gpu
            .as_mut()
            .map(report_kumquat_exit)
            .transpose()?
            .unwrap_or(false);
        if host_died {
            eprintln!("[valo-client] terminating the stalled game");
            let _ = child.kill();
            let _ = child.wait();
        }
        return Ok(ExitCode::from(125));
    }

    // In gdb, don't kill after the inferior process stops.
    let status = if gdb {
        child.wait().context("wait for gdb")?
    } else {
        wait_or_kill(&mut child)?
    };
    let host_died = gpu
        .as_mut()
        .map(report_kumquat_exit)
        .transpose()?
        .unwrap_or(false);
    if host_died && status.success() {
        return Ok(ExitCode::from(125));
    }

    if let Some(code) = status.code() {
        Ok(ExitCode::from(u8::try_from(code).unwrap_or(1)))
    } else if let Some(sig) = status.signal() {
        Ok(ExitCode::from(u8::try_from(128 + sig).unwrap_or(255)))
    } else {
        Ok(ExitCode::from(1))
    }
}

pub(crate) fn report_kumquat_exit(session: &mut GpuSession) -> Result<bool> {
    let Some(status) = session.exit_status()? else {
        return Ok(false);
    };
    let pid = session.pid();
    eprintln!("[valo-client] kumquat (pid {pid}) exited: {status}");
    if status.signal().is_some() {
        eprintln!("[valo-client] inspect core dump: coredumpctl debug {pid}");
    }
    eprintln!(
        "[valo-client] kumquat log: {}",
        session.log_path().display()
    );
    Ok(true)
}

fn run_ipc_loop(sock: &Path, child: &mut Child, gdb: bool, config: Config) -> Result<SessionEnd> {
    loop {
        let Some(mut conn) = establish_or_child_exit(sock, child)? else {
            eprintln!("[valo-client] child exited before ipc connection");
            return Ok(SessionEnd::Done);
        };
        eprintln!("[valo-client] ipc connected on {}", sock.display());

        conn.init(&config)?;
        conn.set_read_timeout(hang_timeout(config.use_gpu))
            .context("set hang watchdog timeout")?;

        let pid = child.id();
        loop {
            let msg = match conn.recv() {
                Ok(Some(m)) => m,
                Ok(None) => {
                    eprintln!("[valo-client] lib hung up");
                    break;
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    report_hang(pid, config.use_gpu)?;
                    return Ok(SessionEnd::Hung);
                }
                Err(e) => return Err(e).context("ipc recv"),
            };
            match msg {
                LibToClient::FrameBoundary(FrameBoundary { frame: _ }) => {
                    // std::thread::sleep_ms(200);
                    conn.send(&ClientToLib::AdvanceFrames(1))
                        .context("send advance")?;
                }
                other => eprintln!("[valo-client] ipc <- {other:?}"),
            }
        }

        // gdb session may run the inferior multiple times
        if !gdb {
            return Ok(SessionEnd::Done);
        }
    }
}

pub(crate) fn report_hang(pid: u32, use_gpu: bool) -> Result<()> {
    eprintln!(
        "[valo-client] hang: no frame boundary for {:?} (pid {pid})",
        hang_timeout(use_gpu)
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
    eprintln!("[valo-client] attach with:");
    eprintln!("  gdb -p {pid} -ex 'thread apply all bt'");
    Ok(())
}

fn hang_timeout(use_gpu: bool) -> Duration {
    if use_gpu {
        GPU_HANG_TIMEOUT
    } else {
        HANG_TIMEOUT
    }
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

/// Wait for the child to exit. If it doesn't exit within a short grace
/// period, SIGKILL it — covers the case where a failed restore left the
/// child deadlocked and `wait()` would otherwise block forever.
fn wait_or_kill(child: &mut std::process::Child) -> Result<std::process::ExitStatus> {
    use std::time::Duration;
    let grace = Duration::from_millis(500);
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

pub(crate) fn resolve_lib(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        if !p.exists() {
            bail!("--lib path does not exist: {}", p.display());
        }
        return Ok(p);
    }

    let exe = std::env::current_exe().context("current_exe failed")?;
    let dir = exe
        .parent()
        .with_context(|| format!("valo binary {} has no parent dir", exe.display()))?;
    let candidate = dir.join("libvalo.so");
    if candidate.exists() {
        return Ok(candidate);
    }
    bail!(
        "libvalo.so not found next to valo binary at {}. Pass --lib explicitly.",
        candidate.display()
    );
}
