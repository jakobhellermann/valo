// TODO(ai-review): generated, not yet audited
//! Stack-allocated structured logger usable from signal handlers.
//!
//! Writes to a log file (default `/tmp/valo.log`, override with `VALO_LOG`)
//! using no allocations or stdio buffering.

use core::ffi::CStr;
use core::fmt::{self, Write};
use core::sync::atomic::{AtomicI32, Ordering};
use std::io::IoSlice;

use rustix::fd::BorrowedFd;
use rustix::fs::{Mode, OFlags};
use rustix::io;

const BUF_SIZE: usize = 1024;
const DEFAULT_PATH: &CStr = c"/tmp/valo.log";

/// Descriptor for log destination.
/// -1 = uninitialized
/// -2 = failed
static LOG_FD: AtomicI32 = AtomicI32::new(-1);

#[cfg(feature = "structured-log")]
pub(crate) mod structured;

#[cfg(not(feature = "structured-log"))]
pub(crate) mod structured {
    pub mod events {
        use crate::checkpoint::{PageFlag, SnapshotSlot};
        use crate::procfs_maps::MemoryMap;

        pub fn init() {}
        pub fn save_area(_area: &MemoryMap, _skip: bool) {}
        pub fn save_pages(_start: u64, _index: u64, _count: u64, _flag: PageFlag) {}
        pub fn restore_area(_area: &MemoryMap, _skip: bool) {}
        pub fn checkpoint_begin(_slot: SnapshotSlot) {}
        pub fn restore_begin(_slot: SnapshotSlot) {}
    }
}

#[derive(Clone, Copy)]
pub enum Category {
    Checkpoint,
    Restore,
    Hooks,
    Frame,
    Init,
    Ipc,
    Steam,
    /// Default category for debug prints without a category (`debug!("foo")).
    Adhoc,
}

impl Category {
    fn as_str(self) -> &'static str {
        match self {
            Category::Checkpoint => "checkpoint",
            Category::Restore => "restore",
            Category::Hooks => "hooks",
            Category::Frame => "frame",
            Category::Init => "init",
            Category::Ipc => "ipc",
            Category::Steam => "steam",
            Category::Adhoc => "adhoc",
        }
    }
}

#[derive(Clone, Copy)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Level::Error => "ERROR",
            Level::Warn => "WARN",
            Level::Info => "INFO",
            Level::Debug => "DEBUG",
            Level::Trace => "TRACE",
        }
    }
}

/// Open the log file. Call once at init.
///
/// Idempotent.
pub fn init() {
    if LOG_FD.load(Ordering::Acquire) >= 0 {
        return;
    }
    let path = path_from_env().unwrap_or(DEFAULT_PATH);
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC | OFlags::CLOEXEC;
    // 0644
    let mode = Mode::RUSR | Mode::WUSR | Mode::RGRP | Mode::ROTH;
    match rustix::fs::open(path, flags, mode) {
        Ok(fd) => {
            let raw = rustix::fd::IntoRawFd::into_raw_fd(fd);
            LOG_FD.store(raw, Ordering::Release);
        }
        Err(_) => {
            LOG_FD.store(-2, Ordering::Release);
        }
    }
    init_structured();
}

#[cfg(feature = "structured-log")]
fn init_structured() {
    structured::init();
}

fn path_from_env() -> Option<&'static CStr> {
    // SAFETY: Environment is not mutated?
    let raw = unsafe { libc::getenv(c"VALO_LOG".as_ptr()) };
    if raw.is_null() {
        return None;
    }
    // SAFETY: getenv returns a NUL-terminated C string.
    Some(unsafe { CStr::from_ptr(raw) })
}

#[cfg(not(feature = "structured-log"))]
fn init_structured() {}

pub(super) struct StackWriter {
    buf: [u8; BUF_SIZE],
    len: usize,
}

impl StackWriter {
    pub(super) fn new() -> Self {
        Self {
            buf: [0; BUF_SIZE],
            len: 0,
        }
    }

    pub(super) fn as_slice(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

impl Write for StackWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let bytes = s.as_bytes();
        let remaining = BUF_SIZE - self.len;
        let n = bytes.len().min(remaining);
        self.buf[self.len..self.len + n].copy_from_slice(&bytes[..n]);
        self.len += n;
        // TODO: currently truncates rather than erroring of buf is exceeded
        Ok(())
    }
}

#[doc(hidden)]
pub fn _write(category: Category, level: Level, args: fmt::Arguments<'_>) {
    let raw_fd = LOG_FD.load(Ordering::Acquire);
    if raw_fd < 0 {
        return;
    }

    // SAFETY: getpid is async-signal-safe and never fails.
    let pid = unsafe { libc::getpid() };

    let mut w = StackWriter::new();
    let _ = write!(w, "[pid={} {}:{}] ", pid, category.as_str(), level.as_str());
    let _ = w.write_fmt(args);
    if w.len < BUF_SIZE {
        w.buf[w.len] = b'\n';
        w.len += 1;
    } else {
        w.buf[BUF_SIZE - 1] = b'\n';
    }

    // SAFETY: raw_fd is a file descriptor we opened in init() and never
    // close. Writing to it from any thread/context is safe.
    let fd = unsafe { BorrowedFd::borrow_raw(raw_fd) };
    let _ = io::writev(fd, &[IoSlice::new(w.as_slice())]);
}

/// Usage: `log!(Restore, Info, "msg {}", arg)`.
///
/// `$category` and `$level` are bare variant names (e.g. `Restore`, `Info`),
/// not full paths — the macro prepends `Category::` / `Level::` itself.
#[macro_export]
macro_rules! log {
    ($category:ident, $level:ident, $($arg:tt)*) => {
        $crate::log::_write(
            $crate::log::Category::$category,
            $crate::log::Level::$level,
            format_args!($($arg)*),
        )
    };
}

#[macro_export]
macro_rules! error {
    ($category:ident, $($arg:tt)*) => { $crate::log!($category, Error, $($arg)*) };
    ($($arg:tt)*) => { $crate::log!(Adhoc, Error, $($arg)*) };
}
#[macro_export]
macro_rules! warn {
    ($category:ident, $($arg:tt)*) => { $crate::log!($category, Warn, $($arg)*) };
    ($($arg:tt)*) => { $crate::log!(Adhoc, Warn, $($arg)*) };
}
#[macro_export]
macro_rules! info {
    ($category:ident, $($arg:tt)*) => { $crate::log!($category, Info, $($arg)*) };
    ($($arg:tt)*) => { $crate::log!(Adhoc, Info, $($arg)*) };
}
#[macro_export]
macro_rules! debug {
    ($category:ident, $($arg:tt)*) => { $crate::log!($category, Debug, $($arg)*) };
    ($($arg:tt)*) => { $crate::log!(Adhoc, Debug, $($arg)*) };
}
// TODO: level-based filtering. For now `trace!` is a no-op
#[macro_export]
macro_rules! trace {
    ($category:ident, $($arg:tt)*) => {
        if false {
            $crate::log!($category, Trace, $($arg)*);
        }
    };
    ($($arg:tt)*) => {
        if false {
            $crate::log!(Adhoc, Trace, $($arg)*);
        }
    };
}

#[macro_export]
macro_rules! abort {
    ($category:ident, $($arg:tt)*) => {
        $crate::log!($category, Error, $($arg)*);
        std::process::abort();
    };
    ($($arg:tt)*) => { $crate::abort!(Adhoc, $($arg)*) };
}
