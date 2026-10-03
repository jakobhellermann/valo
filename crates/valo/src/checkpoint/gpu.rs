// TODO(ai-review): generated, not yet audited
//! Coordinate a frozen guest with its Kumquat GPU host. SIGUSR1/2 start an
//! operation; one byte on the result pipe reports its completion. The pipe is
//! read through an init-time duplicate owned by valo, independent of the
//! guest's fd numbers and of fd_table checkpoint state.

use std::io;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags};
use rustix::io::Errno;
use rustix::time::Timespec;

use crate::checkpoint::SnapshotSlot;
use crate::{Result, error, info};

const GPU_WAIT_TIMEOUT: Duration = Duration::from_secs(6);
static KUMQUAT_PID: AtomicI32 = AtomicI32::new(0);

/// Kumquat's end of the result pipe, duplicated at init and owned by valo
/// from then on. `None` means GPU orchestration is off.
static RESULT_FD: OnceLock<Option<OwnedFd>> = OnceLock::new();

pub fn init() -> Result<()> {
    if !use_gpu() {
        if std::env::var_os("VIRTGPU_KUMQUAT").is_some() {
            return Err(io::Error::from_raw_os_error(libc::EINVAL));
        }
        info!(Init, "gpu orchestration disabled");
        return Ok(());
    }

    let pid = env_i32("KUMQUAT_PID")?;
    let result_fd = env_i32("VALO_GPU_RESULT_FD")?;

    // SAFETY: signal 0 only checks whether the launcher's child still exists.
    if unsafe { libc::kill(pid, 0) } < 0 {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: the launcher set the fd number, and nothing has closed it yet.
    let inherited = unsafe { BorrowedFd::borrow_raw(result_fd) };
    if rustix::io::fcntl_getfd(inherited).is_err() {
        return Err(io::Error::last_os_error());
    }

    // The duplicate is owned by valo and never closed, so the result pipe
    // stays readable even if the guest closes or reuses its inherited copy.
    let owned = rustix::io::fcntl_dupfd_cloexec(inherited, 3)?;
    KUMQUAT_PID.store(pid, Ordering::SeqCst);

    let _ = RESULT_FD.set(Some(owned));
    info!(Init, "gpu orchestration: kumquat pid {pid}");

    Ok(())
}

/// Start the GPU snapshot.
pub fn trigger_snapshot(slot: SnapshotSlot) -> Result<()> {
    ensure_supported_slot(slot)?;
    signal_kumquat(libc::SIGUSR1)
}

pub fn wait_snapshot_complete() -> Result<()> {
    wait_result(b'S')
}

/// Signal kumquat to start restoring its host state.
/// Returns immediately, the restore runs in the kumquat process.
/// Complete it with [`wait_restore_complete`].
pub fn trigger_restore(slot: SnapshotSlot) -> Result<()> {
    ensure_supported_slot(slot)?;
    signal_kumquat(libc::SIGUSR2)
}

/// Wait for the kumquat restore triggered by [`trigger_restore`] to finish.
pub fn wait_restore_complete() -> Result<()> {
    wait_result(b'R')
}

fn ensure_supported_slot(slot: SnapshotSlot) -> Result<()> {
    // TODO: support multiple slots
    if KUMQUAT_PID.load(Ordering::SeqCst) != 0 && slot.0 != 0 {
        return Err(io::Error::from_raw_os_error(libc::EOPNOTSUPP));
    }
    Ok(())
}

fn wait_result(expected: u8) -> Result<()> {
    let Some(fd) = RESULT_FD.get().and_then(Option::as_ref) else {
        return Ok(());
    };

    let deadline = Instant::now() + GPU_WAIT_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::from_raw_os_error(libc::ETIMEDOUT));
        }

        let mut poll_fds = [PollFd::new(fd, PollFlags::IN | PollFlags::HUP)];
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

        let mut status = [0_u8];
        let count = match rustix::io::read(fd, &mut status) {
            Err(e) if e == Errno::INTR => continue,
            count => count?,
        };

        return match (count, status[0]) {
            (0, _) => {
                error!(
                    "kumquat exited during the {}",
                    match expected {
                        b'R' => "restore",
                        _ => "snapshot",
                    }
                );
                Err(io::Error::from_raw_os_error(libc::EPIPE))
            }
            (1, value) if value == expected => Ok(()),
            (1, b's' | b'r') => {
                error!(
                    "kumquat reported the {} as failed",
                    match expected {
                        b'R' => "restore",
                        _ => "snapshot",
                    }
                );
                Err(io::Error::from_raw_os_error(libc::EIO))
            }
            _ => Err(io::Error::from_raw_os_error(libc::EPROTO)),
        };
    }
}

fn signal_kumquat(signal: i32) -> Result<()> {
    let pid = KUMQUAT_PID.load(Ordering::SeqCst);
    if pid == 0 {
        return Ok(());
    }
    // SAFETY: pid was validated during init; kill reports races with exit.
    if unsafe { libc::kill(pid, signal) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn env_i32(name: &str) -> Result<i32> {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse::<i32>().ok())
        .filter(|v| *v > 0)
        .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))
}

/// Whether GPU orchestration is enabled: `VALO_USE_GPU` set to anything but an
/// explicit off value. Must agree with `valo-launch`'s parser, which selects
/// the matching renderer environment for the child.
// TODO: use valo-launch in valo?
fn use_gpu() -> bool {
    let Some(value) = std::env::var_os("VALO_USE_GPU") else {
        return false;
    };
    !matches!(value.to_string_lossy().trim(), "" | "0" | "false" | "no")
}
