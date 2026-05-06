// TODO(ai-review): generated, not yet audited
//! IPC connection to the valo client.
//!
//! Opt-in via `VALO_SOCK`. When set, [`init`] blocks until the client peer
//! connects, exchanges Hello messages, and stores the connection in a
//! `OnceLock` for the rest of the process. Frame-boundary and hooks call
//! [`with_ipc`] to send/recv on the connection under a `Mutex`.

use std::ffi::CStr;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, OnceLock};

use valo_ipc::messages::{ClientToLib, Hello, LibToClient};
use valo_ipc::{LibIPC, PROTOCOL_VERSION};

use crate::{Result, error, info, warn};

static IPC: OnceLock<Mutex<LibIPC>> = OnceLock::new();

/// If `VALO_SOCK` is set, establish the IPC connection and run the Hello
/// handshake. Returns `Ok(())` whether or not a connection was made — the
/// only error path is one where the env var was set but rendezvous or
/// handshake failed.
pub fn init() -> Result<()> {
    // SAFETY: called from `_init` before any threads or env mutation, doesn't escape function
    let Some(path) = (unsafe { path_from_env() }) else {
        info!(Ipc, "VALO_SOCK not set, IPC disabled");
        return Ok(());
    };
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
