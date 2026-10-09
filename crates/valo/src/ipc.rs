// TODO(ai-review): generated, not yet audited
//! IPC connection to the valo client.
//!
//! Opt-in via `VALO_SOCK`.
//! Different modes of operation depending on the init handshake:
//!
//! - the client answers with `AdvanceFrames` and keeps blocking every
//!   boundary (`valo run`).
//! - a `Detach` queued during init drains and frees the session
//!   (`valo spawn`).
//! - a client that closed leaves the session held for the next one
//!   (`valo spawn --paused`).

use std::io::ErrorKind;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use valo_ipc::config::{Config, DeterminismConfig, FrameConfig, InputConfig, LogConfig};
use valo_ipc::messages::{ClientToLib, Error, FrameBoundary, LibHello, LibToClient, State};
use valo_ipc::{LibIPC, PROTOCOL_VERSION};

use crate::checkpoint::SnapshotSlot;
use crate::global_state::GlobalState;
use crate::{Result, checkpoint, debug, error, info, trace, warn};

/// How long a free session waits for a command from a connected client
/// before the frame runs.
const COMMAND_READ_TIMEOUT: Duration = Duration::from_millis(100);

/// How long a handshake on an accepted connection may take, so a client
/// that connects but never completes it cannot freeze the session.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_millis(100);

enum Release {
    /// Every frame boundary waits for a client command.
    Held,
    /// Frames run until this frame number (then held again).
    Until(u64),
    /// Frames run without stopping.
    Free,
}

struct Session {
    listener: UnixListener,
    conn: Option<LibIPC>,
    release: Release,
}

static SESSION: Mutex<Option<Session>> = Mutex::new(None);

pub fn init() -> Result<()> {
    let Some(path) = std::env::var_os("VALO_SOCK").map(PathBuf::from) else {
        info!(Ipc, "VALO_SOCK not set, IPC disabled");
        return Ok(());
    };
    info!(Ipc, "establishing on {}", path.display());

    let mut conn = LibIPC::establish(&path)?;
    handshake(&mut conn)?;

    let msg = conn
        .recv()?
        .ok_or_else(|| std::io::Error::other("client closed before SetConfig"))?;
    let ClientToLib::SetConfig(config) = msg else {
        return Err(std::io::Error::other(format!(
            "expected SetConfig after Hello, got {msg:?}"
        )));
    };
    GlobalState::get().apply_config(&config);

    // The init client's next message (or closing) decides the session: a
    // command keeps the connection and enters held (gated session),
    // Detach enters free, closing enters held without a client (paused
    // session).
    let listener = bind_listener(&path)?;
    *lock_session() = Some(Session {
        listener,
        conn: Some(conn),
        release: Release::Held,
    });
    info!(Ipc, "init complete");
    Ok(())
}

fn bind_listener(path: &Path) -> Result<UnixListener> {
    // The rendezvous leaves the socket file stale on both sides (the
    // listener drops it after accept). Reclaim the path for reconnects and
    // one-shot clients.
    let _ = std::fs::remove_file(path);
    UnixListener::bind(path)
        .map_err(|e| std::io::Error::other(format!("bind {}: {e}", path.display())))
}

/// Called at every frame boundary. No-op without IPC.
pub fn handle_frame_boundary(frame: u64) {
    let mut guard = lock_session();
    let Some(session) = guard.as_mut() else {
        return;
    };

    if let Release::Until(until) = session.release
        && frame >= until
    {
        session.release = Release::Held;
    }

    match session.release {
        Release::Free => free_turn(session, frame),
        Release::Until(_) => {}
        Release::Held => held_turn(session, frame),
    }
}

fn free_turn(session: &mut Session, frame: u64) {
    if session.conn.is_none() {
        // An idle session must never block the game waiting for a client.
        if let Err(e) = session.listener.set_nonblocking(true) {
            warn!(Ipc, "set listener mode: {e}");
            return;
        }
        match session.listener.accept() {
            Ok((stream, _)) => match accept_handshake(stream) {
                Ok(conn) => {
                    info!(Ipc, "client connected");
                    session.conn = Some(conn);
                }
                Err(e) => warn!(Ipc, "client handshake failed: {e}"),
            },
            Err(e) if e.kind() == ErrorKind::WouldBlock => return,
            Err(e) => {
                warn!(Ipc, "accept: {e}");
                return;
            }
        }
    }

    let Some(conn) = session.conn.as_mut() else {
        return;
    };
    if let Err(e) = conn.set_read_timeout(COMMAND_READ_TIMEOUT) {
        warn!(Ipc, "set command timeout: {e}");
        session.conn = None;
        return;
    }
    match conn.recv() {
        Ok(Some(msg)) => {
            let Some(mut conn) = session.conn.take() else {
                return;
            };
            handle_command(session, &mut conn, msg, frame);
            session.conn = Some(conn);
        }
        Ok(None) => {
            info!(Ipc, "client disconnected");
            session.conn = None;
        }
        Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {}
        Err(e) if is_hangup(&e) => {
            info!(Ipc, "client disconnected: {e}");
            session.conn = None;
        }
        Err(e) => {
            warn!(Ipc, "recv: {e}");
            session.conn = None;
        }
    }
}

/// Holds this frame boundary until a client releases it. A freshly
/// accepted client gets no notification — it initiates with a command —
/// while a client that is already connected gets a `FrameBoundary`
/// notification per held boundary.
fn held_turn(session: &mut Session, frame: u64) {
    let mut in_dialogue = false;
    loop {
        if session.conn.is_none() {
            if let Err(e) = session.listener.set_nonblocking(false) {
                warn!(Ipc, "set listener mode: {e}");
                return;
            }
            match session.listener.accept() {
                Ok((stream, _)) => match accept_handshake(stream) {
                    Ok(conn) => {
                        info!(Ipc, "client connected");
                        session.conn = Some(conn);
                        in_dialogue = true;
                    }
                    Err(e) => {
                        warn!(Ipc, "client handshake failed: {e}");
                        continue;
                    }
                },
                Err(e) => {
                    warn!(Ipc, "accept: {e}");
                    return;
                }
            }
        }

        if !in_dialogue {
            let Some(conn) = session.conn.as_mut() else {
                continue;
            };
            // The client may have queued commands and closed (a spawner's
            // Detach): an EPIPE here must not drop the connection before
            // its pending input is drained — recv returns the queued data
            // before the EOF.
            match conn.send(&LibToClient::FrameBoundary(FrameBoundary { frame })) {
                Ok(()) => {}
                Err(e) if is_hangup(&e) => info!(Ipc, "client closed before FrameBoundary"),
                Err(e) => warn!(Ipc, "send FrameBoundary: {e}"),
            }
            in_dialogue = true;
        }

        let Some(mut conn) = session.conn.take() else {
            continue;
        };
        if let Err(e) = conn.clear_read_timeout() {
            warn!(Ipc, "clear read timeout: {e}");
            session.conn = None;
            continue;
        }
        match conn.recv() {
            Ok(Some(msg)) => match handle_command(session, &mut conn, msg, frame) {
                Action::Run => {
                    session.conn = Some(conn);
                    return;
                }
                Action::Hold => {
                    session.conn = Some(conn);
                }
                Action::Resync => {
                    debug!(Ipc, "resync at frame={frame}");
                    session.conn = Some(conn);
                    in_dialogue = false;
                }
            },
            Ok(None) => {
                info!(Ipc, "client disconnected");
                session.conn = None;
            }
            Err(e) if is_hangup(&e) => {
                info!(Ipc, "client disconnected: {e}");
                session.conn = None;
            }
            Err(e) => {
                warn!(Ipc, "recv: {e}");
                session.conn = None;
            }
        }
    }
}

fn is_hangup(e: &std::io::Error) -> bool {
    matches!(e.kind(), ErrorKind::BrokenPipe | ErrorKind::ConnectionReset)
}

enum Action {
    /// The current frame may run.
    Run,
    /// The session stays held; keep waiting for commands.
    Hold,
    /// Stays held, but re-sends the FrameBoundary notification first: a
    /// restore resumed inside the checkpoint's held-turn continuation,
    /// past the point where the original notification was sent — the
    /// client is not rewound and still waits for it.
    Resync,
}

fn handle_command(
    session: &mut Session,
    conn: &mut LibIPC,
    msg: ClientToLib,
    frame: u64,
) -> Action {
    match msg {
        ClientToLib::Hello(_) => {
            error!(Ipc, "unexpected Hello mid-session");
            Action::Hold
        }
        ClientToLib::SetConfig(config) => {
            info!(Ipc, "received SetConfig");
            if config.use_gpu != GlobalState::get().use_gpu.load(Ordering::Relaxed) {
                if let Err(e) = conn.send(&LibToClient::Error(Error {
                    message: "use_gpu is fixed at init".into(),
                })) {
                    warn!(Ipc, "send SetConfig error: {e}");
                }
                return Action::Hold;
            }
            GlobalState::get().apply_config(&config);
            if let Err(e) = conn.send(&LibToClient::SetConfigDone) {
                warn!(Ipc, "send SetConfigDone: {e}");
            }
            Action::Hold
        }
        ClientToLib::GetState => {
            info!(Ipc, "received GetState");
            if let Err(e) = conn.send(&LibToClient::State(State { frame })) {
                warn!(Ipc, "send State: {e}");
            }
            Action::Hold
        }
        ClientToLib::Quit => {
            info!(Ipc, "quit requested, exiting");
            std::process::exit(0);
        }
        ClientToLib::AdvanceFrames(count) => {
            if !matches!(session.release, Release::Held) {
                if let Err(e) = conn.send(&LibToClient::Error(Error {
                    message: "session is not held".into(),
                })) {
                    warn!(Ipc, "send advance error: {e}");
                }
                return Action::Hold;
            }
            if count == 0 {
                if let Err(e) = conn.send(&LibToClient::State(State { frame })) {
                    warn!(Ipc, "send State: {e}");
                }
                return Action::Hold;
            }
            let Some(target) = frame.checked_add(count) else {
                if let Err(e) = conn.send(&LibToClient::Error(Error {
                    message: "advance frame count overflow".into(),
                })) {
                    warn!(Ipc, "send advance error: {e}");
                }
                return Action::Hold;
            };
            trace!(Ipc, "received AdvanceFrames({count})");
            session.release = Release::Until(target);
            Action::Run
        }
        ClientToLib::Detach => {
            info!(Ipc, "detached, session runs free");
            session.release = Release::Free;
            Action::Run
        }
        ClientToLib::Checkpoint(slot) => {
            info!(Ipc, "checkpoint slot {slot} requested at frame {frame}");
            crate::valo_checkpoint_slot(slot);
            // valo_checkpoint returns a second time when a later restore jumps back into it
            let response = match GlobalState::get().restore_in_progress() {
                true => LibToClient::RestoreSucceeded,
                false => LibToClient::CheckpointDone,
            };
            if let Err(e) = conn.send(&response) {
                warn!(Ipc, "send {response:?}: {e}");
            }
            match GlobalState::get().restore_in_progress() {
                true => Action::Resync,
                false => Action::Hold,
            }
        }
        ClientToLib::Restore(slot) => {
            info!(Ipc, "restore slot {slot} requested at frame {frame}");
            // a failed restore past the preflight aborts the target
            if let Err(message) = checkpoint::preflight_restore(SnapshotSlot(slot)) {
                error!(Ipc, "restore preflight failed: {message}");
                if let Err(e) = conn.send(&LibToClient::Error(Error { message })) {
                    warn!(Ipc, "send restore error: {e}");
                }
                return Action::Hold;
            }
            // On success valo_restore jumps into the saved ucontext and never
            // returns here; on failure it aborts. The client observes EOF.
            crate::valo_restore_slot(slot);
            unreachable!("valo_restore never returns")
        }
        ClientToLib::SetInputs(state) => {
            trace!(Ipc, "received SetInputs mods={:#x}", state.mods);
            crate::input_feed::set_state(state);
            Action::Hold
        }
    }
}

/// Hello exchange on a fresh connection: version check, and an echo of the
/// config the lib currently has.
fn handshake(conn: &mut LibIPC) -> Result<()> {
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
    conn.send(&LibToClient::Hello(LibHello::new(effective_config())))
}

fn accept_handshake(stream: std::os::unix::net::UnixStream) -> Result<LibIPC> {
    // On Linux accepted sockets inherit O_NONBLOCK from the listener; the
    // blocking framed reads below need a blocking stream.
    stream.set_nonblocking(false)?;
    let mut conn = LibIPC::from_accepted(stream);
    conn.set_read_timeout(HANDSHAKE_TIMEOUT)?;
    handshake(&mut conn)?;
    Ok(conn)
}

pub fn effective_config() -> Config {
    let state = GlobalState::get();
    Config {
        frames: FrameConfig {
            checkpoint_frame: state.checkpoint_frame.load(Ordering::Relaxed),
            restore_frame: state.restore_frame.load(Ordering::Relaxed),
            max_restores: state.max_restores.load(Ordering::Relaxed),
        },
        log: LogConfig {
            level: crate::log::current_level().into(),
        },
        use_gpu: state.use_gpu.load(Ordering::Relaxed),
        determinism: DeterminismConfig {
            time: crate::determinism::time::effective(),
            randomness: crate::determinism::randomness::effective(),
        },
        skip_draw: state.skip_draw_mode(),
        input: InputConfig {
            uncontrolled_input: state.uncontrolled_input.load(Ordering::Relaxed),
        },
    }
}

fn lock_session() -> MutexGuard<'static, Option<Session>> {
    SESSION.lock().unwrap_or_else(|p| {
        warn!(Ipc, "mutex poisoned, recovering");
        SESSION.clear_poison();
        p.into_inner()
    })
}
