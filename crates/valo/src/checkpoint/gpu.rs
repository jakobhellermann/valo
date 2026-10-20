//! Coordinate a frozen guest with its Kumquat GPU host over Kumquat's control connection, which
//! the client hands over during the IPC init. A request names the slot's snapshot directory;
//! Kumquat answers each one with OK or an error text.

use std::fmt::NumBuffer;
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStringExt;
use std::sync::OnceLock;
use std::sync::atomic::Ordering;
use std::time::Duration;

use rustix::event::{PollFd, PollFlags};
use rustix::io::Errno;
use rustix::net::{RecvFlags, SendFlags};
use rustix::time::Timespec;
use valo_ipc::kumquat;

use crate::checkpoint::SnapshotSlot;
use crate::determinism::time::real_monotonic;
use crate::global_state::GlobalState;
use crate::{Result, error, home, info};

const GPU_WAIT_TIMEOUT: Duration = Duration::from_secs(6);
const MAX_MESSAGE_SIZE: usize = 4 + 4096;

static CONTROL: OnceLock<OwnedFd> = OnceLock::new();
/// `<home>/slots`, laid out at init so that requests need no allocation.
static SLOTS_DIR: OnceLock<Vec<u8>> = OnceLock::new();

pub fn set_control(control: OwnedFd) -> Result<()> {
    CONTROL
        .set(control)
        .map_err(|_| io::Error::other("GPU control socket received twice"))
}

pub fn init() -> Result<()> {
    if !use_gpu() {
        if std::env::var_os("VIRTGPU_KUMQUAT").is_some() {
            return Err(io::Error::from_raw_os_error(libc::EINVAL));
        }
        info!(Init, "gpu orchestration disabled");
        return Ok(());
    }
    if CONTROL.get().is_none() {
        return Err(io::Error::other("use_gpu without a GPU control socket"));
    }
    let slots = home::get().join("slots").into_os_string().into_vec();
    SLOTS_DIR
        .set(slots)
        .map_err(|_| io::Error::other("gpu orchestration initialized twice"))?;
    info!(
        Init,
        "gpu orchestration over the kumquat control connection"
    );
    Ok(())
}

/// Start the GPU snapshot.
pub fn trigger_snapshot(slot: SnapshotSlot) -> Result<()> {
    send_request(kumquat::SNAPSHOT, slot)
}

pub fn wait_snapshot_complete() -> Result<()> {
    wait_response("snapshot")
}

/// Ask kumquat to start restoring its host state.
/// Returns immediately, the restore runs in the kumquat process.
/// Complete it with [`wait_restore_complete`].
pub fn trigger_restore(slot: SnapshotSlot) -> Result<()> {
    send_request(kumquat::RESTORE, slot)
}

/// Wait for the kumquat restore triggered by [`trigger_restore`] to finish.
pub fn wait_restore_complete() -> Result<()> {
    wait_response("restore")
}

/// Sends `kind` with the slot's snapshot directory `<home>/slots/<slot>/gpu`.
fn send_request(kind: u32, slot: SnapshotSlot) -> Result<()> {
    let Some(control) = CONTROL.get() else {
        return Ok(());
    };
    let slots = SLOTS_DIR
        .get()
        .ok_or_else(|| io::Error::other("gpu orchestration not initialized"))?;
    let mut message = [0u8; MAX_MESSAGE_SIZE];
    let mut len = 0;
    let mut push = |bytes: &[u8]| -> Result<()> {
        let end = len + bytes.len();
        let target = message
            .get_mut(len..end)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::ENAMETOOLONG))?;
        target.copy_from_slice(bytes);
        len = end;
        Ok(())
    };
    push(&kind.to_le_bytes())?;
    push(slots)?;
    push(b"/")?;
    push(slot.0.format_into(&mut NumBuffer::new()).as_bytes())?;
    push(b"/gpu")?;
    let sent = rustix::net::send(control, &message[..len], SendFlags::NOSIGNAL)?;
    if sent != len {
        return Err(io::Error::from_raw_os_error(libc::EMSGSIZE));
    }
    Ok(())
}

fn wait_response(operation: &str) -> Result<()> {
    let Some(control) = CONTROL.get() else {
        return Ok(());
    };

    let deadline = real_monotonic() + GPU_WAIT_TIMEOUT;
    loop {
        let remaining = deadline.saturating_sub(real_monotonic());
        if remaining.is_zero() {
            return Err(io::Error::from_raw_os_error(libc::ETIMEDOUT));
        }

        let mut poll_fds = [PollFd::new(control, PollFlags::IN | PollFlags::HUP)];
        let timeout = Timespec {
            tv_sec: remaining.as_secs() as i64,
            tv_nsec: remaining.subsec_nanos() as i64,
        };
        let ready = match rustix::event::poll(&mut poll_fds, Some(&timeout)) {
            Err(e) if e == Errno::INTR => continue,
            ready => ready?,
        };
        if ready == 0 {
            return Err(io::Error::from_raw_os_error(libc::ETIMEDOUT));
        }

        let mut message = [0u8; MAX_MESSAGE_SIZE];
        let len = match rustix::net::recv(control, &mut message, RecvFlags::empty()) {
            Err(e) if e == Errno::INTR => continue,
            received => received?.0,
        };
        if len == 0 {
            error!("kumquat exited during the {operation}");
            return Err(io::Error::from_raw_os_error(libc::EPIPE));
        }
        if len < 4 {
            return Err(io::Error::from_raw_os_error(libc::EPROTO));
        }
        let kind = u32::from_le_bytes([message[0], message[1], message[2], message[3]]);
        return match kind {
            kumquat::RESP_OK => Ok(()),
            kumquat::RESP_ERR => {
                let text = std::str::from_utf8(&message[4..len]).unwrap_or("<not UTF-8>");
                error!("kumquat {operation} failed: {text}");
                Err(io::Error::from_raw_os_error(libc::EIO))
            }
            _ => Err(io::Error::from_raw_os_error(libc::EPROTO)),
        };
    }
}

/// Whether GPU orchestration is enabled. The client sets this via the init
/// SetConfig and must match what it set up for the target (kumquat host,
/// renderer selection).
fn use_gpu() -> bool {
    GlobalState::get().use_gpu.load(Ordering::Relaxed)
}
