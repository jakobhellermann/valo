//! Rewind the kernel fd table to the checkpoint state.

use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};

use rustix::fs::{self, Mode, OFlags, SeekFrom};
use rustix::io::{self as rio, Errno, FdFlags};

use crate::checkpoint::SnapshotSlot;
use crate::log::structured;
use crate::procfs::fds::{self, FdKind, ProcFds};
use crate::token::Frozen;
use crate::{Result, error, info};

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
pub(crate) fn snapshot(frozen: &Frozen, slot: SnapshotSlot) -> Result<Vec<FdRecord>> {
    let mut records = Vec::new();
    let mut fds = ProcFds::new_self()?;
    while let Some(entry) = fds.next(frozen)? {
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
    fds::open_fd_path(record.fd).ok()
}

// Restore the fd table.
// - Close newly opened FDs
// - re-open missing fds at original number
// - restore offsets and flags
pub(crate) fn restore(frozen: &Frozen, records: &[FdRecord]) -> Result<()> {
    let mut closed = 0;
    {
        let mut current = ProcFds::new_self()?;
        while let Some(entry) = current.next(frozen)? {
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
                _ => {
                    missing += 1;
                    structured::events::fd_action("restore.fds.missing", record.fd);
                }
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
        let frozen = unsafe { Frozen::assume() };
        let records = snapshot(&frozen, SnapshotSlot(0)).unwrap();

        let opened = open_by_identity(&records, stat.st_dev, stat.st_ino)
            .expect("snapshot must hold the open memfd");
        let mut buf = [0u8; 3];
        rustix::io::read(&opened, &mut buf).unwrap();
        assert_eq!(&buf, b"abc");

        assert!(open_by_identity(&records, stat.st_dev, stat.st_ino ^ 1).is_none());
    }
}
