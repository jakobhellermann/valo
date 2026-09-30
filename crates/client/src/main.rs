mod tui;

use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use valo_ipc::messages::{ClientToLib, FrameBoundary, Hello, LibToClient};
use valo_ipc::{ClientIPC, PROTOCOL_VERSION};

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

        /// Target program to execute.
        program: PathBuf,

        /// Arguments forwarded to the target program.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
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
        } => run(lib, gdb, tui, program, args),
    }
}

fn run(
    lib: Option<PathBuf>,
    gdb: bool,
    use_tui: bool,
    program: PathBuf,
    args: Vec<String>,
) -> Result<ExitCode> {
    let lib = resolve_lib(lib)?;
    let lib = std::fs::canonicalize(&lib)
        .with_context(|| format!("canonicalize libvalo.so path {}", lib.display()))?;

    let mut cmd = build_command(gdb, &program, &args);
    cmd.env("LD_PRELOAD", prepend_ld_preload(&lib));
    valo_launch::configure_target(&mut cmd);

    if use_tui {
        // Don't let the child's stdout/stderr clobber the alternate screen.
        // The user can still see lib logs via the events panel.
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
    }

    let sock_path = PathBuf::from(format!("/tmp/valo-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&sock_path);
    cmd.env("VALO_SOCK", &sock_path);

    // Resolve the lib's log path so the TUI can tail it. Same defaulting
    // as in the lib's `log::init`.
    let log_path: Option<PathBuf> = std::env::var_os("VALO_LOG")
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from("/tmp/valo.log")));

    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawning {}", cmd.get_program().to_string_lossy()))?;

    let result = if use_tui {
        tui::run(&sock_path, log_path.as_deref(), child.id())
    } else {
        run_ipc_loop(&sock_path)
    };
    if let Err(e) = result {
        eprintln!("ipc loop ended: {e:#}");
    }
    // Best-effort cleanup of the socket file.
    let _ = std::fs::remove_file(&sock_path);

    let status = wait_or_kill(&mut child)?;

    if let Some(code) = status.code() {
        Ok(ExitCode::from(u8::try_from(code).unwrap_or(1)))
    } else if let Some(sig) = status.signal() {
        Ok(ExitCode::from(u8::try_from(128 + sig).unwrap_or(255)))
    } else {
        Ok(ExitCode::from(1))
    }
}

fn run_ipc_loop(sock: &Path) -> Result<()> {
    let mut conn = ClientIPC::establish(sock).context("ipc establish")?;
    eprintln!("[valo-client] ipc connected on {}", sock.display());

    conn.send(&ClientToLib::Hello(Hello::CLIENT))
        .context("send hello")?;
    let peer = conn
        .recv()
        .context("recv hello")?
        .context("lib closed before hello")?;
    let LibToClient::Hello(peer_hello) = peer else {
        bail!("expected Hello from lib, got {peer:?}");
    };
    if peer_hello.version != PROTOCOL_VERSION {
        bail!(
            "lib protocol version {} does not match client {}",
            peer_hello.version,
            PROTOCOL_VERSION
        );
    }
    eprintln!("[valo-client] handshake complete");

    loop {
        let msg = match conn.recv()? {
            Some(m) => m,
            None => {
                eprintln!("[valo-client] lib hung up");
                return Ok(());
            }
        };
        match msg {
            LibToClient::FrameBoundary(FrameBoundary { frame: _ }) => {
                // std::thread::sleep_ms(200);
                conn.send(&ClientToLib::Advance).context("send advance")?;
            }
            other => eprintln!("[valo-client] ipc <- {other:?}"),
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

fn build_command(gdb: bool, program: &PathBuf, args: &[String]) -> Command {
    if gdb {
        // gdb -ex 'handle SIGSYS nostop noprint' --args <program> [args...]
        let mut cmd = Command::new("gdb");
        cmd.arg("-ex")
            .arg("python gdb.execute('handle SIGSYS nostop noprint', to_string=True)") // suppress gdb signal table result
            .arg("--args")
            .arg(program)
            .args(args);
        cmd
    } else {
        let mut cmd = Command::new(program);
        cmd.args(args);
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

fn resolve_lib(explicit: Option<PathBuf>) -> Result<PathBuf> {
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
