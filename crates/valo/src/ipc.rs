// TODO(ai-review): generated, not yet audited
//! IPC connection to the valo client.
//!
//! Opt-in via `VALO_SOCK`. Two modes, selected by `VALO_IPC_MODE`:
//!
//! - `block` (default): init blocks until the client connects; the client
//!   gates every frame boundary (used by the harness and the TUI).
//! - `async`: init only binds a listener; frames pump freely and one-shot
//!   `valo msg` clients connect, issue a command, and disconnect.

use std::ffi::CStr;
use std::io::ErrorKind;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use valo_ipc::messages::{ClientToLib, Hello, LibToClient, State};
use valo_ipc::{LibIPC, PROTOCOL_VERSION};

use crate::global_state::GlobalState;
use crate::{Result, error, info, warn};

static IPC: OnceLock<Mutex<LibIPC>> = OnceLock::new();

/// Async IPC mode state (`VALO_IPC_MODE=async`): the listener socket exists
/// from init onward; at most one transient client is connected at a time.
/// In this mode frames pump freely and `valo msg` sessions are one-shot
/// command exchanges.
static ASYNC: Mutex<Option<AsyncState>> = Mutex::new(None);

struct AsyncState {
    listener: UnixListener,
    conn: Option<LibIPC>,
}

/// How long the async boundary handler waits for a command from a connected
/// client before giving up for this boundary.
const ASYNC_READ_TIMEOUT: Duration = Duration::from_millis(100);

/// If `VALO_SOCK` is set, set up IPC. With `VALO_IPC_MODE=async` this only
/// binds the listener socket (the game starts immediately, clients connect
/// later); otherwise (block mode) it blocks until the client peer connects,
/// exchanges Hello messages, and stores the connection in a `OnceLock` for
/// the rest of the process.
pub fn init() -> Result<()> {
    // SAFETY: called from `_init` before any threads or env mutation, doesn't escape function
    let Some(path) = (unsafe { path_from_env() }) else {
        info!(Ipc, "VALO_SOCK not set, IPC disabled");
        return Ok(());
    };
    if async_mode_requested() {
        return init_async(path);
    }
    info!(Ipc, "establishing on {}", path.display());

    let mut ipc = LibIPC::establish(path)?;
    ipc.send(&LibToClient::Hello(Hello::LIB))?;
    let peer = ipc
        .recv()?
        .ok_or_else(|| std::io::Error::other("client closed before Hello"))?;
    let ClientToLib::Hello(peer_hello) = peer else {
        return Err(std::io::Error::other(format!(
            "expected Hello from client, got {peer:?}"
        )));
    };
    if peer_hello.version != PROTOCOL_VERSION {
        return Err(std::io::Error::other(format!(
            "client protocol version {} does not match lib version {}",
            peer_hello.version, PROTOCOL_VERSION
        )));
    }

    info!(Ipc, "handshake complete");
    if IPC.set(Mutex::new(ipc)).is_err() {
        // OnceLock::set fails only if already set — we should be the only
        // caller during init.
        error!(Ipc, "already initialized; ignoring second init call");
    }
    Ok(())
}

/// Lock the IPC connection until the returned guard is dropped. Returns
/// `None` when IPC was not configured (no `VALO_SOCK`); the caller should
/// continue without IPC in that case.
pub fn lock() -> Option<MutexGuard<'static, LibIPC>> {
    let mutex = IPC.get()?;
    Some(mutex.lock().unwrap_or_else(|p| {
        warn!(Ipc, "mutex poisoned, recovering");
        mutex.clear_poison();
        p.into_inner()
    }))
}

pub fn async_mode() -> bool {
    let guard = ASYNC.lock().unwrap_or_else(|p| {
        warn!(Ipc, "mutex poisoned, recovering");
        p.into_inner()
    });
    guard.is_some()
}

pub fn handle_async(frame: u64) {
    let mut guard = ASYNC.lock().unwrap_or_else(|p| {
        warn!(Ipc, "mutex poisoned, recovering");
        p.into_inner()
    });
    let Some(state) = guard.as_mut() else { return };

    if state.conn.is_none() {
        match state.listener.accept() {
            Ok((stream, _)) => match handshake_and_wrap(stream) {
                Ok(conn) => {
                    info!(Ipc, "msg client connected");
                    state.conn = Some(conn);
                }
                Err(e) => warn!(Ipc, "client handshake failed: {e}"),
            },
            Err(e) if e.kind() == ErrorKind::WouldBlock => {}
            Err(e) => warn!(Ipc, "accept: {e}"),
        }
    }

    let Some(conn) = state.conn.as_mut() else {
        return;
    };
    let _ = conn.set_read_timeout(ASYNC_READ_TIMEOUT);
    match conn.recv() {
        Ok(Some(msg)) => handle_async_command(conn, msg, frame),
        Ok(None) => {
            info!(Ipc, "msg client disconnected");
            state.conn = None;
        }
        Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {}
        Err(e) => {
            warn!(Ipc, "async recv: {e}");
            state.conn = None;
        }
    }
}

/// Execute one client command in async mode.
// TODO: don't duplicate this
fn handle_async_command(conn: &mut LibIPC, msg: ClientToLib, frame: u64) {
    match msg {
        ClientToLib::Hello(_) => error!(Ipc, "unexpected Hello mid-session"),
        // Async mode pumps frames itself; there is no gate to open.
        ClientToLib::Advance => {}
        ClientToLib::Checkpoint => {
            info!(Ipc, "checkpoint requested at frame {frame}");
            crate::valo_checkpoint();
            // valo_checkpoint returns twice: once after the save, once when a
            // later restore jumps back into this call. On the second return
            // the connection that issued the checkpoint is long gone (one-shot
            // msg client), so only reply on the first.
            if GlobalState::get().restore_in_progress() {
                return;
            }
            if let Err(e) = conn.send(&LibToClient::CheckpointDone) {
                error!(Ipc, "send CheckpointDone: {e}");
            }
        }
        ClientToLib::Restore => {
            info!(Ipc, "restore requested at frame {frame}");
            // On success valo_restore jumps into the saved ucontext and never
            // returns here; the msg client observes EOF instead of a reply.
            crate::valo_restore();
            error!(Ipc, "did not restore snapshot");
        }
        ClientToLib::GetState => {
            if let Err(e) = conn.send(&LibToClient::State(State { frame })) {
                error!(Ipc, "send State: {e}");
            }
        }
        ClientToLib::Quit => {
            info!(Ipc, "quit requested, exiting");
            std::process::exit(0);
        }
    }
}

/// Wrap an accepted stream and run the Hello handshake. A client that
/// connects but never completes the handshake is dropped after
/// `ASYNC_READ_TIMEOUT` so it cannot freeze the game.
fn handshake_and_wrap(stream: std::os::unix::net::UnixStream) -> Result<LibIPC> {
    // On Linux accepted sockets inherit O_NONBLOCK from the listener; the
    // blocking framed reads below need a blocking stream.
    stream.set_nonblocking(false)?;
    let mut conn = LibIPC::from_accepted(stream);
    conn.set_read_timeout(ASYNC_READ_TIMEOUT)?;
    conn.send(&LibToClient::Hello(Hello::LIB))?;
    let peer = conn
        .recv()?
        .ok_or_else(|| std::io::Error::other("client closed before Hello"))?;
    let ClientToLib::Hello(peer_hello) = peer else {
        return Err(std::io::Error::other(format!(
            "expected Hello from client, got {peer:?}"
        )));
    };
    if peer_hello.version != PROTOCOL_VERSION {
        return Err(std::io::Error::other(format!(
            "client protocol version {} does not match lib version {}",
            peer_hello.version, PROTOCOL_VERSION
        )));
    }
    Ok(conn)
}

fn init_async(path: &Path) -> Result<()> {
    // A stale socket file from a dead previous session would make bind fail.
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)
        .map_err(|e| std::io::Error::other(format!("bind {}: {e}", path.display())))?;
    // Non-blocking accept at frame boundaries: an idle session must never
    // block the game waiting for a client.
    listener.set_nonblocking(true)?;
    let mut guard = ASYNC.lock().unwrap_or_else(|p| {
        warn!(Ipc, "mutex poisoned, recovering");
        p.into_inner()
    });
    *guard = Some(AsyncState {
        listener,
        conn: None,
    });
    // One-shot clients use this to attribute hang reports to the game pid.
    if let Err(e) = std::fs::write(pid_file(path), format!("{}\n", std::process::id())) {
        warn!(Ipc, "write pid file: {e}");
    }
    info!(Ipc, "async IPC mode listening on {}", path.display());
    Ok(())
}

fn pid_file(sock: &Path) -> PathBuf {
    let mut p = sock.as_os_str().to_owned();
    p.push(".pid");
    PathBuf::from(p)
}

/// Whether `VALO_IPC_MODE=async` was set (default: block).
fn async_mode_requested() -> bool {
    // SAFETY: getenv has no preconditions of its own.
    let raw = unsafe { libc::getenv(c"VALO_IPC_MODE".as_ptr()) };
    if raw.is_null() {
        return false;
    }
    // SAFETY: getenv returns a NUL-terminated C string.
    let value: &CStr = unsafe { CStr::from_ptr(raw) };
    value.to_bytes() == b"async"
}

/// Read `VALO_SOCK` environment variable.
///
/// # Safety
///
/// For the duration of the lifetime, `getenv` must not be invalidated
/// by `setenv/putenv/unsetenv`.
unsafe fn path_from_env<'a>() -> Option<&'a Path> {
    use std::os::unix::ffi::OsStrExt;
    // SAFETY: getenv has no preconditions of its own.
    let raw = unsafe { libc::getenv(c"VALO_SOCK".as_ptr()) };
    if raw.is_null() {
        return None;
    }
    // SAFETY: getenv returns a NUL-terminated C string.
    // The function-level contract justifies the lifetime.
    let cstr: &'a CStr = unsafe { CStr::from_ptr(raw) };
    let bytes = cstr.to_bytes();
    if bytes.is_empty() {
        return None;
    }
    Some(Path::new(std::ffi::OsStr::from_bytes(bytes)))
}
