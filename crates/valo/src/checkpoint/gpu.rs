//! Coordinate a frozen guest with its Kumquat GPU host over Kumquat's control connection, which
//! the client hands over during the IPC init. A request names the slot's snapshot directory;
//! Kumquat answers each one with OK or an error text.

use std::fmt::NumBuffer;
use std::io;
use std::io::IoSliceMut;
use std::mem::MaybeUninit;
use std::os::fd::{IntoRawFd, OwnedFd};
use std::os::unix::ffi::OsStringExt;
use std::sync::OnceLock;
use std::sync::atomic::Ordering;
use std::time::Duration;

use rustix::event::{PollFd, PollFlags};
use rustix::io::Errno;
use rustix::mm::{MapFlags, ProtFlags};
use rustix::net::{RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags, SendFlags};
use rustix::time::Timespec;
use valo_ipc::kumquat;

use crate::checkpoint::SnapshotSlot;
use crate::checkpoint::fd_sync::FdRecord;
use crate::determinism::time::real_monotonic;
use crate::global_state::GlobalState;
use crate::log::structured;
use crate::procfs::maps::{MemoryMap, MemoryMapPermissions, ProcMaps};
use crate::{Result, error, home, info};

const GPU_WAIT_TIMEOUT: Duration = Duration::from_secs(6);
const MAX_MESSAGE_SIZE: usize = 4 + 4096;

/// A GPU host object the guest holds; a restore replaces it with a new one.
#[derive(Clone, Copy, Debug)]
pub(crate) struct GpuObject {
    resource_id: u32,
    dev: u64,
    ino: u64,
}

/// Where the guest maps a GPU host object.
#[derive(Clone, Copy, Debug)]
pub(crate) struct GpuArea {
    start: u64,
    len: usize,
    prot: ProtFlags,
    resource_id: u32,
}

/// The resource the object `(dev, ino)` belongs to, if it is a GPU host object.
pub(crate) fn resource_of(objects: &[GpuObject], dev: u64, ino: u64) -> Option<u32> {
    objects
        .iter()
        .find(|object| object.dev == dev && object.ino == ino)
        .map(|object| object.resource_id)
}

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
    let mut payload = [0u8; MAX_MESSAGE_SIZE];
    let (kind, len, descriptor) = receive(control, &mut payload, operation)?;
    if descriptor.is_some() {
        return Err(io::Error::from_raw_os_error(libc::EPROTO));
    }
    check_response(kind, &payload[..len], operation)
}

fn check_response(kind: u32, payload: &[u8], operation: &str) -> Result<()> {
    match kind {
        kumquat::RESP_OK => Ok(()),
        kumquat::RESP_ERR => {
            let text = std::str::from_utf8(payload).unwrap_or("<not UTF-8>");
            error!("kumquat {operation} failed: {text}");
            Err(io::Error::from_raw_os_error(libc::EIO))
        }
        _ => Err(io::Error::from_raw_os_error(libc::EPROTO)),
    }
}

/// Receives the next message within [`GPU_WAIT_TIMEOUT`]: its type, the payload length and an
/// attached descriptor.
fn receive(
    control: &OwnedFd,
    payload: &mut [u8; MAX_MESSAGE_SIZE],
    operation: &str,
) -> Result<(u32, usize, Option<OwnedFd>)> {
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
        let mut space = [MaybeUninit::<u8>::uninit(); rustix::cmsg_space!(ScmRights(1))];
        let mut ancillary = RecvAncillaryBuffer::new(&mut space);
        let received = match rustix::net::recvmsg(
            control,
            &mut [IoSliceMut::new(&mut message)],
            &mut ancillary,
            RecvFlags::CMSG_CLOEXEC,
        ) {
            Err(e) if e == Errno::INTR => continue,
            received => received?,
        };
        if received
            .flags
            .intersects(ReturnFlags::TRUNC | ReturnFlags::CTRUNC)
        {
            error!("kumquat sent a message that does not fit during the {operation}");
            return Err(io::Error::from_raw_os_error(libc::EMSGSIZE));
        }
        let mut descriptor = None;
        for message in ancillary.drain() {
            if let RecvAncillaryMessage::ScmRights(mut fds) = message {
                descriptor = fds.next();
                if descriptor.is_none() || fds.next().is_some() {
                    return Err(io::Error::from_raw_os_error(libc::EPROTO));
                }
            }
        }
        let len = received.bytes;
        if len == 0 {
            error!("kumquat exited during the {operation}");
            return Err(io::Error::from_raw_os_error(libc::EPIPE));
        }
        if len < 4 {
            return Err(io::Error::from_raw_os_error(libc::EPROTO));
        }
        let kind = u32::from_le_bytes([message[0], message[1], message[2], message[3]]);
        payload[..len - 4].copy_from_slice(&message[4..len]);
        return Ok((kind, len - 4, descriptor));
    }
}

/// Lists the GPU host objects the guest holds and where it maps them.
pub(crate) fn capture_objects(
    objects: &mut Vec<GpuObject>,
    areas: &mut Vec<GpuArea>,
) -> Result<()> {
    objects.clear();
    areas.clear();
    let Some(control) = CONTROL.get() else {
        return Ok(());
    };
    let request = kumquat::LIST.to_le_bytes();
    if rustix::net::send(control, &request, SendFlags::NOSIGNAL)? != request.len() {
        return Err(io::Error::from_raw_os_error(libc::EMSGSIZE));
    }
    let mut payload = [0u8; MAX_MESSAGE_SIZE];
    let (kind, len, _) = receive(control, &mut payload, "list")?;
    check_response(kind, &payload[..len], "list")?;
    loop {
        let (kind, len, _) = receive(control, &mut payload, "list")?;
        match (kind, len) {
            (kumquat::END, 0) => break,
            (kumquat::RESOURCE, 20) => objects.push(GpuObject {
                resource_id: u32::from_le_bytes(payload[0..4].try_into().unwrap()),
                dev: u64::from_le_bytes(payload[4..12].try_into().unwrap()),
                ino: u64::from_le_bytes(payload[12..20].try_into().unwrap()),
            }),
            _ => return Err(io::Error::from_raw_os_error(libc::EPROTO)),
        }
    }

    let mut maps = ProcMaps::new_self_copy()?;
    while let Some(area) = maps.next()? {
        if let Some(resource_id) = resource_of(objects, area.dev, area.inode) {
            areas.push(GpuArea {
                start: area.address.start,
                len: (area.address.end - area.address.start) as usize,
                prot: area.perms.into(),
                resource_id,
            });
        }
    }
    Ok(())
}

/// Puts the GPU host's new objects where the guest had the replaced ones: every mapping at its
/// address, every descriptor at its number. Kumquat sends them after the restore's answer.
pub(crate) fn apply_remaps(
    objects: &[GpuObject],
    areas: &[GpuArea],
    records: &[FdRecord],
) -> Result<()> {
    let Some(control) = CONTROL.get() else {
        return Ok(());
    };
    let mut payload = [0u8; MAX_MESSAGE_SIZE];
    let mut remapped = 0;
    loop {
        let (kind, len, descriptor) = receive(control, &mut payload, "restore")?;
        match (kind, len, descriptor) {
            (kumquat::END, 0, None) => break,
            (kumquat::REMAP, 4, Some(memory)) => {
                let resource_id = u32::from_le_bytes(payload[0..4].try_into().unwrap());
                if !objects
                    .iter()
                    .any(|object| object.resource_id == resource_id)
                {
                    error!("kumquat replaced resource {resource_id}, which the guest did not hold");
                    return Err(io::Error::from_raw_os_error(libc::EPROTO));
                }
                for area in areas.iter().filter(|area| area.resource_id == resource_id) {
                    // Kumquat clients map whole objects; /proc shows DRM fake offsets for
                    // dma-bufs, not object offsets.
                    // SAFETY: replaces exactly the range the replaced object was mapped at,
                    // with its protection; the guest is frozen.
                    unsafe {
                        rustix::mm::mmap(
                            area.start as *mut _,
                            area.len,
                            area.prot,
                            MapFlags::SHARED | MapFlags::FIXED,
                            &memory,
                            0,
                        )?;
                    }
                }
                // The received descriptor may already sit at a number fd_sync freed.
                let memory = memory.into_raw_fd();
                let mut placed = false;
                for record in records {
                    let (dev, ino) = record.identity();
                    if resource_of(objects, dev, ino) != Some(resource_id) {
                        continue;
                    }
                    // SAFETY: places the new object at the guest's descriptor number, with the
                    // guest's descriptor flags.
                    let ret = unsafe {
                        if record.fd() == memory {
                            placed = true;
                            libc::fcntl(
                                memory,
                                libc::F_SETFD,
                                if record.cloexec() {
                                    libc::FD_CLOEXEC
                                } else {
                                    0
                                },
                            )
                        } else {
                            libc::dup3(
                                memory,
                                record.fd(),
                                if record.cloexec() { libc::O_CLOEXEC } else { 0 },
                            )
                        }
                    };
                    if ret < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    structured::events::fd_action("restore.fds.remap", record.fd());
                }
                if !placed {
                    // SAFETY: the received descriptor is not one of the guest's numbers.
                    unsafe { libc::close(memory) };
                }
                remapped += 1;
            }
            _ => return Err(io::Error::from_raw_os_error(libc::EPROTO)),
        }
    }
    if remapped != objects.len() {
        error!(
            "kumquat replaced {remapped} of the {} GPU host objects the guest holds",
            objects.len()
        );
        return Err(io::Error::from_raw_os_error(libc::EPROTO));
    }
    info!(Restore, "remapped {remapped} GPU host objects");
    Ok(())
}

/// Fails for a shared mapping of a DRM device: a GPU driver in the guest mapped memory that a
/// restore cannot replace.
pub(crate) fn check_area(area: &MemoryMap) -> Result<()> {
    if use_gpu()
        && area.perms.contains(MemoryMapPermissions::SHARED)
        && area.pathname.starts_with("/dev/dri/")
    {
        error!(Checkpoint, "a GPU driver in the guest maps {area}");
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    Ok(())
}

/// Whether GPU orchestration is enabled. The client sets this via the init
/// SetConfig and must match what it set up for the target (kumquat host,
/// renderer selection).
fn use_gpu() -> bool {
    GlobalState::get().use_gpu.load(Ordering::Relaxed)
}
