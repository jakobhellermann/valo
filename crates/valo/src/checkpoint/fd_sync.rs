//! Rewind the kernel fd table to the checkpoint state.

use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};

use rustix::fs::{self, Mode, OFlags, SeekFrom};
use rustix::io::{self as rio, Errno, FdFlags};

use crate::checkpoint::SnapshotSlot;
use crate::global_state::GlobalState;
use crate::log::structured;
use crate::procfs::fds::{self, FdKind, ProcFds};
use crate::token::{Exclusive, Frozen};
use crate::{Result, error, info};

use self::keepalive::{FdKeepalive, FdKeepaliveRegistry};

/// Base fd number for non-restorable file descriptors
const KEEPALIVE_FD_BASE: RawFd = 1024;

/// File descriptor at checkpoint time
pub struct FdRecord {
    fd: RawFd,
    kind: FdKind,
    fd_flags: FdFlags,
    status_flags: OFlags,
    /// `None` = not seekable
    offset: Option<u64>,
    dev: fs::Dev,
    ino: u64,
    link: String,
}

// Capture current fd table.
pub(crate) fn snapshot(
    exclusive: &mut Exclusive,
    frozen: &Frozen,
    slot: SnapshotSlot,
) -> Result<Vec<FdRecord>> {
    let keepalives = GlobalState::fd_keepalives(exclusive);
    capture(frozen, slot, keepalives)
}

fn capture(
    frozen: &Frozen,
    slot: SnapshotSlot,
    keepalives: &mut FdKeepaliveRegistry,
) -> Result<Vec<FdRecord>> {
    // The previous generation of this slot dies with the snapshot image
    // that is about to replace it.
    keepalives.clear_slot(slot);

    let mut records = Vec::new();
    let mut fds = ProcFds::new_self()?;
    while let Some(entry) = fds.next(frozen)? {
        if keepalives.is_keepalive(entry.fd, entry.dev, entry.ino) {
            continue;
        }
        structured::events::fd("checkpoint.fds", &entry);
        records.push(FdRecord {
            fd: entry.fd,
            kind: entry.kind,
            fd_flags: entry.fd_flags,
            status_flags: entry.status_flags,
            offset: entry.offset,
            dev: entry.dev,
            ino: entry.ino,
            link: entry.link.to_owned(),
        });
    }

    // Stash away keepalives
    // TODO: restore pipes normally
    for record in &records {
        if record.kind == FdKind::Regular {
            continue;
        }

        if keepalives.find(slot, record.dev, record.ino).is_some() {
            continue;
        }

        // SAFETY: all guest threads are suspended and the fd exists
        let borrowed = unsafe { BorrowedFd::borrow_raw(record.fd) };

        // TODO: only dup when not already present
        let keepalive = match rio::fcntl_dupfd_cloexec(borrowed, KEEPALIVE_FD_BASE) {
            Ok(keepalive) => keepalive,
            Err(e) => {
                error!(
                    Checkpoint,
                    "cannot pin fd {} ({}): {}", record.fd, record.link, e
                );
                continue;
            }
        };

        let keepalive_fd = keepalive.as_raw_fd();
        if keepalives
            .register(slot, record.dev, record.ino, keepalive)
            .is_err()
        {
            error!(
                Checkpoint,
                "keepalive registry full ({}): fd {} stays unrestorable after close",
                keepalive::MAX_FDS,
                record.fd
            );
            continue;
        }

        structured::events::fd_pin(
            record.fd,
            keepalive_fd,
            record.dev,
            record.ino,
            &record.link,
        )
    }

    info!(
        Checkpoint,
        "captured {} fds for slot {}",
        records.len(),
        slot.0
    );
    Ok(records)
}

/// Opens the file backing `(dev, ino)` through the given fd table
pub(crate) fn open_by_identity(records: &[FdRecord], dev: fs::Dev, ino: u64) -> Option<OwnedFd> {
    let record = records.iter().find(|r| r.dev == dev && r.ino == ino)?;
    fds::open_fd_path(record.fd, OFlags::RDONLY).ok()
}

// Restore the fd table.
// - Close newly opened FDs
// - re-open missing fds at original number
// - re-dup path-less fds from their keepalives
// - restore offsets and flags
pub(crate) fn restore(
    exclusive: &mut Exclusive,
    frozen: &Frozen,
    slot: SnapshotSlot,
    records: &[FdRecord],
) -> Result<()> {
    let keepalives = GlobalState::fd_keepalives(exclusive);

    let mut closed = 0;
    {
        let mut current = ProcFds::new_self()?;
        while let Some(entry) = current.next(frozen)? {
            if keepalives.is_keepalive(entry.fd, entry.dev, entry.ino) {
                continue;
            }
            let known = records.iter().any(|record| {
                record.fd == entry.fd && record.dev == entry.dev && record.ino == entry.ino
            });
            if known {
                continue;
            }
            // SAFETY: the descriptor belongs to the discarded timeline
            unsafe { close_fd(entry.fd) };
            closed += 1;
            structured::events::fd_action("restore.fds.close", entry.fd);
        }
    }

    let mut kept = 0;
    let mut reopened = 0;
    let mut missing = 0;
    for record in records {
        // SAFETY: the function contract requires no other thread active.
        if !unsafe { is_open(record) }? {
            match record.kind {
                FdKind::Regular => match reopen(record) {
                    Ok(()) => {
                        reopened += 1;
                        structured::events::fd_action("restore.fds.reopen", record.fd);
                    }
                    Err(e) => {
                        error!(
                            Restore,
                            "cannot reopen fd {} from {}: {}", record.fd, record.link, e
                        );
                        structured::events::fd_action("restore.fds.reopen_failed", record.fd);
                    }
                },
                _ => match keepalives.find(slot, record.dev, record.ino) {
                    Some(entry) => match restore_keepalive(entry, record) {
                        Ok(()) => {
                            reopened += 1;
                            structured::events::fd_action("restore.fds.reopen", record.fd);
                        }
                        Err(e) => {
                            error!(
                                Restore,
                                "cannot restore fd {} ({}): {}", record.fd, record.link, e
                            );
                            structured::events::fd_action("restore.fds.reopen_failed", record.fd);
                        }
                    },
                    None => {
                        missing += 1;
                        error!(
                            Restore,
                            "no keepalive for path-less fd {} ({})", record.fd, record.link
                        );
                        structured::events::fd_action("restore.fds.missing", record.fd);
                    }
                },
            }
            continue;
        }
        kept += 1;
        // SAFETY: the function contract requires no other thread active.
        let borrowed = unsafe { BorrowedFd::borrow_raw(record.fd) };
        rio::fcntl_setfd(borrowed, record.fd_flags)?;
        fs::fcntl_setfl(borrowed, record.status_flags)?;
        if let Some(offset) = record.offset {
            fs::seek(borrowed, SeekFrom::Start(offset))?;
        }
    }
    info!(
        Restore,
        "fd sync: {kept} kept, {closed} closed, {reopened} reopened, {missing} missing"
    );
    Ok(())
}

// Whether the record's original file is still at its number
//
// # Safety
// - no other thread may close or reuse the descriptor number during the call.
unsafe fn is_open(record: &FdRecord) -> Result<bool> {
    // SAFETY: the number is open or absent for the whole call.
    let borrowed = unsafe { BorrowedFd::borrow_raw(record.fd) };
    match fs::fstat(borrowed) {
        Ok(stat) => Ok(stat.st_dev == record.dev && stat.st_ino == record.ino),
        Err(Errno::BADF) => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Closes a descriptor number we do not own.
///
/// # Safety
/// - the descriptor is open, suitable for assuming ownership, and
///   requires no cleanup other than close.
unsafe fn close_fd(fd: RawFd) {
    // SAFETY: the function contract requires an adoptable open descriptor.
    drop(unsafe { OwnedFd::from_raw_fd(fd) });
}

/// Reopens the record's file and places it at the original fd number.
fn reopen(record: &FdRecord) -> Result<()> {
    let file = fs::open(&record.link, record.status_flags, Mode::empty())?;
    let placed = if file.as_raw_fd() == record.fd {
        file
    } else {
        let placed = rio::fcntl_dupfd_cloexec(&file, record.fd)?;
        if placed.as_raw_fd() != record.fd {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("fd {} is occupied", record.fd),
            ));
        }
        drop(file);
        placed
    };
    rio::fcntl_setfd(&placed, record.fd_flags)?;
    if let Some(offset) = record.offset {
        fs::seek(&placed, SeekFrom::Start(offset))?;
    }
    // Transfer ownership to the rewound state
    std::mem::forget(placed);
    Ok(())
}

fn restore_keepalive(entry: &FdKeepalive, record: &FdRecord) -> Result<()> {
    let stat = fs::fstat(&entry.keepalive)?;
    if stat.st_dev != record.dev || stat.st_ino != record.ino {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "keepalive no longer holds the recorded file",
        ));
    }

    let placed = rio::fcntl_dupfd_cloexec(&entry.keepalive, record.fd)?;
    if placed.as_raw_fd() != record.fd {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("fd {} is occupied", record.fd),
        ));
    }
    rio::fcntl_setfd(&placed, record.fd_flags)?;
    fs::fcntl_setfl(&placed, record.status_flags)?;

    if let Some(offset) = record.offset {
        fs::seek(&placed, SeekFrom::Start(offset))?;
    }

    // Transfoer ownership to restored state
    std::mem::forget(placed);

    Ok(())
}

pub(crate) mod keepalive {
    use std::os::fd::{AsRawFd, OwnedFd, RawFd};

    use rustix::fs::Dev;
    use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};

    use crate::checkpoint::SnapshotSlot;
    use crate::{info, warn};

    pub const MAX_FDS: usize = 4096;

    const MIN_NOFILE: u64 = 8192;

    pub(crate) fn ensure_fd_space() {
        let current = getrlimit(Resource::Nofile);
        let soft = current.current.unwrap_or(u64::MAX);
        if soft >= MIN_NOFILE {
            return;
        }

        let hard = current.maximum.unwrap_or(u64::MAX);
        let target = MIN_NOFILE.min(hard);
        if target <= soft {
            warn!(
                Init,
                "fd hard limit limit is {hard}, not enough space for keepalive fds at {}",
                super::KEEPALIVE_FD_BASE
            );
            return;
        }
        if let Err(e) = setrlimit(
            Resource::Nofile,
            Rlimit {
                current: Some(target),
                maximum: current.maximum,
            },
        ) {
            warn!(Init, "cannot raise soft RLIMIT_NOFILE to {target}: {e}");
            return;
        }
        info!(Init, "raised soft RLIMIT_NOFILE from {soft} to {target}");
    }

    pub(super) struct FdKeepalive {
        pub slot: SnapshotSlot,
        pub keepalive: OwnedFd,
        pub dev: Dev,
        pub ino: u64,
    }

    pub struct FdKeepaliveRegistry {
        entries: heapless::Vec<FdKeepalive, MAX_FDS>,
    }

    impl FdKeepaliveRegistry {
        pub(super) fn clear_slot(&mut self, slot: SnapshotSlot) {
            let mut i = 0;
            while i < self.entries.len() {
                if self.entries[i].slot == slot {
                    drop(self.entries.swap_remove(i));
                } else {
                    i += 1;
                }
            }
        }

        pub(super) fn register(
            &mut self,
            slot: SnapshotSlot,
            dev: Dev,
            ino: u64,
            keepalive: OwnedFd,
        ) -> Result<(), OwnedFd> {
            if self.find(slot, dev, ino).is_some() {
                drop(keepalive);
                return Ok(());
            }

            self.entries
                .push(FdKeepalive {
                    slot,
                    dev,
                    ino,
                    keepalive,
                })
                .map_err(|rejected| rejected.keepalive)
        }

        pub(super) fn find(&self, slot: SnapshotSlot, dev: Dev, ino: u64) -> Option<&FdKeepalive> {
            self.entries
                .iter()
                .find(|e| e.slot == slot && e.dev == dev && e.ino == ino)
        }

        pub fn is_keepalive(&self, fd: RawFd, dev: Dev, ino: u64) -> bool {
            self.entries
                .iter()
                .any(|e| e.keepalive.as_raw_fd() == fd && e.dev == dev && e.ino == ino)
        }
    }

    #[cfg(test)]
    mod tests {
        use std::os::fd::BorrowedFd;

        use rustix::fs::{MemfdFlags, fstat, memfd_create};

        use super::*;

        #[test]
        fn registry_pins_and_purges_per_slot() {
            let _guard = crate::utils::FD_TABLE_TEST_LOCK.lock().unwrap();
            let mut registry = FdKeepaliveRegistry {
                entries: heapless::Vec::new(),
            };

            let memfd = memfd_create(c"valo-keepalive-test", MemfdFlags::CLOEXEC).unwrap();
            let stat = fstat(&memfd).unwrap();

            let slot0 = SnapshotSlot(0);
            let slot1 = SnapshotSlot(1);

            let pinned0 = memfd.try_clone().unwrap();
            let slot0_fd = pinned0.as_raw_fd();
            registry
                .register(slot0, stat.st_dev, stat.st_ino, pinned0)
                .unwrap();
            assert!(registry.is_keepalive(slot0_fd, stat.st_dev, stat.st_ino));
            assert!(!registry.is_keepalive(slot0_fd, stat.st_dev, stat.st_ino ^ 1));
            assert_eq!(
                registry
                    .find(slot0, stat.st_dev, stat.st_ino)
                    .unwrap()
                    .keepalive
                    .as_raw_fd(),
                slot0_fd
            );
            assert!(registry.find(slot1, stat.st_dev, stat.st_ino).is_none());

            // One pin per fd
            let redundant = memfd.try_clone().unwrap();
            let redundant_fd = redundant.as_raw_fd();
            registry
                .register(slot0, stat.st_dev, stat.st_ino, redundant)
                .unwrap();
            assert!(registry.find(slot0, stat.st_dev, stat.st_ino).is_some());

            // SAFETY: the borrow is only used for an EBADF; nothing can observe it, and the number is not used beyond this call.
            let closed = unsafe { BorrowedFd::borrow_raw(redundant_fd) };
            assert_eq!(fstat(closed).unwrap_err(), rustix::io::Errno::BADF);

            let pinned1 = memfd.try_clone().unwrap();
            registry
                .register(slot1, stat.st_dev, stat.st_ino, pinned1)
                .unwrap();

            registry.clear_slot(slot0);
            assert!(registry.find(slot0, stat.st_dev, stat.st_ino).is_none());
            assert!(registry.find(slot1, stat.st_dev, stat.st_ino).is_some());
            // SAFETY: the borrow is only used for an EBADF; nothing can observe it, and the number is not used beyond this call.
            let closed = unsafe { BorrowedFd::borrow_raw(slot0_fd) };
            assert_eq!(fstat(closed).unwrap_err(), rustix::io::Errno::BADF);

            registry.clear_slot(slot1);
            assert!(registry.find(slot1, stat.st_dev, stat.st_ino).is_none());
        }
    }
}

#[cfg(test)]
mod tests {
    use rustix::fs::{self, MemfdFlags};

    use super::*;

    #[test]
    fn snapshot_lookup_opens_unlinked_file() {
        let _guard = crate::utils::FD_TABLE_TEST_LOCK.lock().unwrap();
        let memfd = fs::memfd_create(c"valo-fds-snapshot", MemfdFlags::empty()).unwrap();
        rustix::io::write(&memfd, b"abc").unwrap();
        fs::seek(&memfd, SeekFrom::Start(0)).unwrap();
        let stat = fs::fstat(&memfd).unwrap();

        // SAFETY: the fd-table test lock serializes the only fd-churning tests.
        let frozen = &unsafe { Frozen::assume() };
        // SAFETY: the fd-table test lock serializes the only fd-churning tests.
        let exclusive = &mut unsafe { Exclusive::assume() };
        let records = snapshot(exclusive, frozen, SnapshotSlot(0)).unwrap();

        let opened = open_by_identity(&records, stat.st_dev, stat.st_ino)
            .expect("snapshot must hold the open memfd");
        let mut buf = [0u8; 3];
        rustix::io::read(&opened, &mut buf).unwrap();
        assert_eq!(&buf, b"abc");

        assert!(open_by_identity(&records, stat.st_dev, stat.st_ino ^ 1).is_none());
    }
}
