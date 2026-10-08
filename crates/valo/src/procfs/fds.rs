// TODO(ai-review): generated, not yet audited
//! Enumerate `/proc/self/fd` and describe each open descriptor.

use std::ffi::CStr;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd, RawFd};
use std::{io, slice};

use rustix::fs::{self, AtFlags, Mode, OFlags, SeekFrom};
use rustix::io::{Errno, FdFlags};

use crate::Result;
use crate::token::Frozen;

/// Kernel PATH_MAX: fd symlink targets cannot be longer.
const LINK_BUF: usize = 4096;
/// One getdents64 batch.
const DIRENT_BUF: usize = 2048;

const DIRENT_RECLEN: usize = std::mem::offset_of!(libc::dirent64, d_reclen);
const DIRENT_NAME: usize = std::mem::offset_of!(libc::dirent64, d_name);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FdKind {
    Regular,
    Pipe,
    Socket,
    MemFd,
    Device,
    Special,
}

impl FdKind {
    fn classify(link: &str) -> Self {
        if link.starts_with("pipe:") {
            Self::Pipe
        } else if link.starts_with("socket:") {
            Self::Socket
        } else if link.starts_with("/dev/") {
            Self::Device
        } else if link.starts_with("/memfd:") {
            Self::MemFd
        } else if link.starts_with("/dmabuf:") {
            Self::Special
        } else if link.starts_with('/') {
            Self::Regular
        } else {
            Self::Special
        }
    }
}

impl std::fmt::Display for FdKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Regular => "regular",
            Self::Pipe => "pipe",
            Self::Socket => "socket",
            Self::MemFd => "memfd",
            Self::Device => "device",
            Self::Special => "special",
        })
    }
}

pub struct OpenFd<'a> {
    pub fd: RawFd,
    pub kind: FdKind,
    pub link: &'a str,
    pub dev: fs::Dev,
    pub ino: u64,
    pub fd_flags: FdFlags,
    pub status_flags: OFlags,
    /// `None` if the descriptor is not seekable.
    pub offset: Option<u64>,
    #[cfg_attr(not(feature = "structured-log"), allow(dead_code))]
    pub size: u64,
}

/// Iterates `/proc/self/fd`.
pub struct ProcFds {
    dir: OwnedFd,
    dirents: [MaybeUninit<u8>; DIRENT_BUF],
    filled: usize,
    pos: usize,
    link_buf: [MaybeUninit<u8>; LINK_BUF],
}

impl ProcFds {
    pub fn new_self() -> Result<Self> {
        let dir = fs::open(
            c"/proc/self/fd",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        Ok(Self {
            dir,
            dirents: [MaybeUninit::uninit(); DIRENT_BUF],
            filled: 0,
            pos: 0,
            link_buf: [MaybeUninit::uninit(); LINK_BUF],
        })
    }

    pub fn next(&mut self, frozen: &Frozen) -> Result<Option<OpenFd<'_>>> {
        loop {
            if self.pos >= self.filled {
                self.pos = 0;
                // SAFETY: getdents64 writes up to DIRENT_BUF bytes into the
                // buffer and leaves the rest untouched.
                let n = unsafe {
                    libc::syscall(
                        libc::SYS_getdents64,
                        self.dir.as_raw_fd(),
                        self.dirents.as_mut_ptr(),
                        DIRENT_BUF,
                    )
                };
                if n < 0 {
                    return Err(io::Error::last_os_error());
                }
                self.filled = n as usize;
                if self.filled == 0 {
                    return Ok(None);
                }
            }

            // SAFETY: getdents64 wrote `filled` bytes as packed records.
            let records = unsafe { self.dirents[..self.filled].assume_init_ref() };
            let reclen = u16::from_ne_bytes([
                records[self.pos + DIRENT_RECLEN],
                records[self.pos + DIRENT_RECLEN + 1],
            ]) as usize;
            debug_assert!(self.pos + reclen <= self.filled);
            let name = &records[self.pos + DIRENT_NAME..self.pos + reclen];
            let nul = name.iter().position(|&b| b == 0).unwrap_or(name.len());
            // ".", ".." and other non-fd entries are skipped here
            let Some(fd) = parse_fd(&name[..nul]) else {
                self.pos += reclen;
                continue;
            };
            // parse_fd caps the name at 10 digits, so it fits with a NUL
            let mut name_buf = [0u8; 12];
            name_buf[..nul].copy_from_slice(&name[..nul]);
            self.pos += reclen;
            if fd == self.dir.as_raw_fd() {
                continue;
            }
            let name = CStr::from_bytes_with_nul(&name_buf[..=nul]).expect("fd name");

            match self.describe(frozen, fd, name) {
                Ok(Some(entry)) => return Ok(Some(entry)),
                Ok(None) => continue,
                Err(e) => return Err(e),
            }
        }
    }

    /// `Ok(None)` if the descriptor vanished since the directory read.
    fn describe(&mut self, _frozen: &Frozen, fd: RawFd, name: &CStr) -> Result<Option<OpenFd<'_>>> {
        // SAFETY: the buffer has LINK_BUF writable bytes.
        let len = unsafe {
            libc::readlinkat(
                self.dir.as_raw_fd(),
                name.as_ptr(),
                self.link_buf.as_mut_ptr().cast(),
                LINK_BUF,
            )
        };
        if len < 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::ENOENT) {
                return Ok(None);
            }
            return Err(err);
        }
        let len = len as usize;
        if len == LINK_BUF {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "fd symlink target exceeds the buffer",
            ));
        }
        // SAFETY: readlinkat wrote `len` bytes; the buffer is owned by self
        // and only overwritten by the next `next()` call.
        let link = match std::str::from_utf8(unsafe {
            slice::from_raw_parts(self.link_buf.as_ptr().cast::<u8>(), len)
        }) {
            Ok(link) => link,
            Err(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "fd symlink target is not valid UTF-8",
                ));
            }
        };

        let stat = match fs::statat(&self.dir, name, AtFlags::empty()) {
            Ok(stat) => stat,
            Err(Errno::NOENT) => return Ok(None),
            Err(e) => return Err(e.into()),
        };

        // SAFETY: fd was just read from /proc/self/fd; `_frozen` proves no
        // other thread can close or reuse it in between.
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        let fd_flags = rustix::io::fcntl_getfd(borrowed)?;
        let status_flags = fs::fcntl_getfl(borrowed)?;
        // ESPIPE and EINVAL both mean "not seekable" (pipes vs dma-bufs);
        // any other errno is unexpected and propagates.
        let offset = match fs::seek(borrowed, SeekFrom::Current(0)) {
            Ok(offset) => Some(offset),
            Err(Errno::SPIPE | Errno::INVAL) => None,
            Err(e) => return Err(e.into()),
        };

        Ok(Some(OpenFd {
            fd,
            kind: FdKind::classify(link),
            link,
            dev: stat.st_dev,
            ino: stat.st_ino,
            fd_flags,
            status_flags,
            offset,
            size: stat.st_size as u64,
        }))
    }
}

/// Opens `/proc/self/fd/<fd>`: a separate description of the same file, so
/// seeking does not move the offset of the passed descriptor.
pub fn open_fd_path(fd: RawFd) -> Result<OwnedFd> {
    let dir = fs::open(
        c"/proc/self/fd",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    Ok(fs::openat(
        &dir,
        rustix::path::DecInt::new(fd),
        OFlags::RDONLY,
        Mode::empty(),
    )?)
}

/// Opens the file with identity `(dev, ino)` via its entry in the current fd table.
// TODO: Don't re-scan here again
pub fn open_by_identity(frozen: &Frozen, dev: fs::Dev, ino: u64) -> Result<Option<OwnedFd>> {
    let mut fds = ProcFds::new_self()?;
    let mut found = None;
    while let Some(entry) = fds.next(frozen)? {
        if entry.dev == dev && entry.ino == ino {
            found = Some(entry.fd);
            break;
        }
    }
    let Some(fd) = found else {
        return Ok(None);
    };
    Ok(Some(open_fd_path(fd)?))
}

fn parse_fd(name: &[u8]) -> Option<RawFd> {
    std::str::from_utf8(name).ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::os::fd::AsRawFd;

    use rustix::fs::{MemfdFlags, Mode, OFlags};

    use super::*;

    #[test]
    fn enumerates_kinds_flags_offset_and_skips_non_fds() {
        // Hold the fd-table lock for the whole test: opening the fixtures and
        // enumerating /proc/self/fd must not interleave with another test
        // closing or reusing a descriptor in between.
        let _guard = crate::utils::FD_TABLE_TEST_LOCK.lock().unwrap();
        let (pipe_read, pipe_write) = rustix::pipe::pipe_with(
            rustix::pipe::PipeFlags::CLOEXEC | rustix::pipe::PipeFlags::NONBLOCK,
        )
        .unwrap();
        let (pipe_read, pipe_write) = (pipe_read.as_raw_fd(), pipe_write.as_raw_fd());

        let memfd = rustix::fs::memfd_create(c"valo-fds-test", MemfdFlags::CLOEXEC).unwrap();
        let devnull = fs::open(
            c"/dev/null",
            OFlags::RDONLY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .unwrap();

        let mut file = std::fs::File::create("/tmp/valo-fds-test-regular").unwrap();
        std::io::Write::write_all(&mut file, b"abc").unwrap();
        std::io::Seek::seek(&mut file, std::io::SeekFrom::Start(1)).unwrap();
        let file_fd = file.as_raw_fd();

        let mut kinds = HashMap::new();
        let mut offset_of = HashMap::new();
        let mut nonblock = HashMap::new();
        {
            // SAFETY: FD_TABLE_TEST_LOCK serializes this test against every
            // other test that opens or closes a descriptor, so no other
            // thread can touch the fd table while it is enumerated.
            let frozen = unsafe { crate::token::Frozen::assume() };
            let mut fds = ProcFds::new_self().unwrap();
            while let Some(entry) = fds.next(&frozen).unwrap() {
                kinds.insert(entry.fd, entry.kind);
                offset_of.insert(entry.fd, entry.offset);
                nonblock.insert(entry.fd, entry.status_flags.contains(OFlags::NONBLOCK));
            }
        }
        std::fs::remove_file("/tmp/valo-fds-test-regular").unwrap();

        assert_eq!(kinds[&pipe_read], FdKind::Pipe);
        assert_eq!(kinds[&memfd.as_raw_fd()], FdKind::MemFd);
        assert_eq!(kinds[&devnull.as_raw_fd()], FdKind::Device);
        assert_eq!(kinds[&file_fd], FdKind::Regular);
        assert_eq!(offset_of[&file_fd], Some(1));
        assert!(nonblock[&pipe_write]);
    }

    #[test]
    fn opens_unlinked_file_by_identity() {
        let _guard = crate::utils::FD_TABLE_TEST_LOCK.lock().unwrap();
        // SAFETY: the fd-table test lock serializes the only fd-churning tests.
        let frozen = unsafe { crate::token::Frozen::assume() };
        let memfd = rustix::fs::memfd_create(c"valo-fds-identity", MemfdFlags::empty()).unwrap();
        rustix::io::write(&memfd, b"abc").unwrap();
        fs::seek(&memfd, SeekFrom::Start(0)).unwrap();
        let stat = fs::fstat(&memfd).unwrap();

        let opened = open_by_identity(&frozen, stat.st_dev, stat.st_ino)
            .unwrap()
            .expect("identity of an open memfd must be found");

        // Separate description: reading does not move the memfd's offset.
        let mut buf = [0u8; 3];
        rustix::io::read(&opened, &mut buf).unwrap();
        assert_eq!(&buf, b"abc");
        assert_eq!(fs::seek(&memfd, SeekFrom::Current(0)).unwrap(), 0);

        assert!(
            open_by_identity(&frozen, stat.st_dev, stat.st_ino ^ 1)
                .unwrap()
                .is_none()
        );
    }
}
