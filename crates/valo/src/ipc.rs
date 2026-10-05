// TODO(ai-review): generated, not yet audited
//! IPC connection to the valo client.
//!
//! Opt-in via `VALO_SOCK`. Two modes, selected by `VALO_IPC_MODE`:
//!
//! - `block` (default): init blocks until the client connects; the client
//!   gates every frame boundary (used by the harness and the TUI).
//! - `async`: init binds a listener for one-shot `valo msg` commands. With
//!   `VALO_IPC_PAUSED=1`, the game waits at frame boundaries for commands.

use std::io::ErrorKind;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use valo_ipc::messages::{ClientToLib, Error, Hello, LibToClient, State};
use valo_ipc::{LibIPC, PROTOCOL_VERSION};

use crate::global_state::GlobalState;
use crate::{Result, error, info, warn};

static IPC: OnceLock<Mutex<LibIPC>> = OnceLock::new();

/// Async IPC mode state (`VALO_IPC_MODE=async`): the listener socket exists
/// from init onward; at most one transient client is connected at a time.
static ASYNC: Mutex<Option<AsyncState>> = Mutex::new(None);

struct AsyncState {
    listener: UnixListener,
    conn: Option<LibIPC>,
    paused: bool,
    advance_until: Option<u64>,
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
    let Some(path) = std::env::var_os("VALO_SOCK").map(PathBuf::from) else {
        info!(Ipc, "VALO_SOCK not set, IPC disabled");
        return Ok(());
    };
    if async_mode_requested() {
        return init_async(&path);
    }
    info!(Ipc, "establishing on {}", path.display());

    let mut ipc = LibIPC::establish(&path)?;
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

    if let Some(target) = state.advance_until {
        if frame < target {
            return;
        }
        state.advance_until = None;
        state.paused = true;
        if let Some(mut conn) = state.conn.take()
            && let Err(e) = conn.send(&LibToClient::State(State { frame }))
        {
            warn!(Ipc, "send advance result: {e}");
        }
    }

    loop {
        if state.conn.is_none() {
            if let Err(e) = state.listener.set_nonblocking(!state.paused) {
                error!(Ipc, "set listener mode: {e}");
                return;
            }
            match state.listener.accept() {
                Ok((stream, _)) => match handshake_and_wrap(stream) {
                    Ok(conn) => {
                        info!(Ipc, "msg client connected");
                        state.conn = Some(conn);
                    }
                    Err(e) => {
                        warn!(Ipc, "client handshake failed: {e}");
                        if state.paused {
                            continue;
                        } else {
                            return;
                        }
                    }
                },
                Err(e) if e.kind() == ErrorKind::WouldBlock => return,
                Err(e) => {
                    warn!(Ipc, "accept: {e}");
                    return;
                }
            }
        }

        let Some(conn) = state.conn.as_mut() else {
            continue;
        };
        let timeout = if state.paused {
            conn.clear_read_timeout()
        } else {
            conn.set_read_timeout(ASYNC_READ_TIMEOUT)
        };
        if let Err(e) = timeout {
            warn!(Ipc, "set command timeout: {e}");
            state.conn = None;
            if state.paused {
                continue;
            } else {
                return;
            }
        }
        let msg = match conn.recv() {
            Ok(Some(msg)) => msg,
            Ok(None) => {
                info!(Ipc, "msg client disconnected");
                state.conn = None;
                if state.paused {
                    continue;
                } else {
                    return;
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                return;
            }
            Err(e) => {
                warn!(Ipc, "async recv: {e}");
                state.conn = None;
                if state.paused {
                    continue;
                } else {
                    return;
                }
            }
        };
        if handle_async_command(state, msg, frame) {
            return;
        }
    }
}

// TODO: don't duplicate this
/// `true` lets the current frame draw; `false` keeps the game paused here.
fn handle_async_command(state: &mut AsyncState, msg: ClientToLib, frame: u64) -> bool {
    let Some(mut conn) = state.conn.take() else {
        return !state.paused;
    };
    match msg {
        ClientToLib::Hello(_) => error!(Ipc, "unexpected Hello mid-session"),
        ClientToLib::AdvanceFrames(count) => {
            if count == 0 {
                let _ = conn.send(&LibToClient::State(State { frame }));
            } else if !state.paused {
                let _ = conn.send(&LibToClient::Error(Error {
                    message: "advance requires spawn --paused".into(),
                }));
            } else if frame.checked_add(count).is_none() {
                let _ = conn.send(&LibToClient::Error(Error {
                    message: "invalid advance frame count".into(),
                }));
            } else {
                state.advance_until = frame.checked_add(count);
                state.paused = false;
                state.conn = Some(conn);
                return true;
            }
        }
        ClientToLib::Checkpoint => {
            info!(Ipc, "checkpoint requested at frame {frame}");
            crate::valo_checkpoint();
            // valo_checkpoint returns twice: once after the save, once when a
            // later restore jumps back into this call. On the second return
            // the connection that issued the checkpoint is long gone (one-shot
            // msg client), so only reply on the first.
            if GlobalState::get().restore_in_progress() {
                return !state.paused;
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
    !state.paused
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
        paused: paused_requested(),
        advance_until: None,
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
    std::env::var_os("VALO_IPC_MODE").as_deref() == Some(std::ffi::OsStr::new("async"))
}

fn paused_requested() -> bool {
    std::env::var_os("VALO_IPC_PAUSED").as_deref() == Some(std::ffi::OsStr::new("1"))
}
