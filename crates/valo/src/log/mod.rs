// TODO(ai-review): generated, not yet audited
//! Stack-allocated structured logger usable from signal handlers.
//!
//! Writes to a log file (default `/tmp/valo.log`, override with `VALO_LOG`)
//! using no allocations or stdio buffering.

use core::fmt::{self, Write};
use core::sync::atomic::{AtomicI32, AtomicU8, Ordering};
use std::io::IoSlice;
use std::path::PathBuf;

use rustix::fd::BorrowedFd;
use rustix::fs::{Mode, OFlags};
use rustix::io;
use valo_ipc::config::LogLevel;

use crate::home;

const BUF_SIZE: usize = 1024;

/// Descriptor for log destination.
/// -1 = uninitialized
/// -2 = failed
static LOG_FD: AtomicI32 = AtomicI32::new(-1);

#[cfg(feature = "structured-log")]
pub(crate) mod structured;

#[cfg(not(feature = "structured-log"))]
pub(crate) mod structured {
    pub mod events {
        use std::os::fd::{OwnedFd, RawFd};

        use rustix::fs::Dev;

        use crate::checkpoint::SnapshotSlot;
        use crate::procfs::maps::MemoryMap;

        pub fn init() {}
        pub fn save_area(_area: &MemoryMap, _skip: bool) {}
        pub fn shared_fd_initial(_start: u64, _shared_fd: Option<&(OwnedFd, u64, u64)>) {}
        pub fn restore_area(_area: &MemoryMap, _skip: bool) {}
        pub fn restore_area_done(_area: &MemoryMap) {}
        pub fn checkpoint_begin(_slot: SnapshotSlot) {}
        pub fn checkpoint_end(_slot: SnapshotSlot) {}
        pub fn restore_begin(_slot: SnapshotSlot) {}
        pub fn restore_end(_slot: SnapshotSlot) {}
        pub fn trace_step(_event: &'static str) {}
        pub fn reallocate_areas_begin() {}
        pub fn reallocate_areas_end() {}
        pub fn log_fd_list(
            _exclusive: &mut crate::token::Exclusive,
            _frozen: &crate::token::Frozen,
            _event: &'static str,
        ) -> crate::Result<()> {
            Ok(())
        }
        pub fn fd(_event: &'static str, _entry: &crate::procfs::fds::OpenFd) {}
        pub fn fd_action(_event: &'static str, _fd: i32) {}
        pub fn fd_pin(_fd: RawFd, _keepalive: RawFd, _dev: Dev, _ino: u64, _link: &str) {}
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

    fn index(self) -> u8 {
        match self {
            Level::Error => 0,
            Level::Warn => 1,
            Level::Info => 2,
            Level::Debug => 3,
            Level::Trace => 4,
        }
    }

    fn from_index(index: u8) -> Self {
        match index {
            0 => Level::Error,
            1 => Level::Warn,
            2 => Level::Info,
            3 => Level::Debug,
            _ => Level::Trace,
        }
    }
}

static MAX_LEVEL: AtomicU8 = AtomicU8::new(4);

pub fn set_level(level: Level) {
    MAX_LEVEL.store(level.index(), Ordering::Release);
}

impl From<LogLevel> for Level {
    fn from(level: LogLevel) -> Self {
        match level {
            LogLevel::Error => Level::Error,
            LogLevel::Warn => Level::Warn,
            LogLevel::Info => Level::Info,
            LogLevel::Debug => Level::Debug,
            LogLevel::Trace => Level::Trace,
        }
    }
}

impl From<Level> for LogLevel {
    fn from(level: Level) -> Self {
        match level {
            Level::Error => LogLevel::Error,
            Level::Warn => LogLevel::Warn,
            Level::Info => LogLevel::Info,
            Level::Debug => LogLevel::Debug,
            Level::Trace => LogLevel::Trace,
        }
    }
}

pub fn current_level() -> Level {
    Level::from_index(MAX_LEVEL.load(Ordering::Acquire))
}

/// Open the log file. Call once at init.
///
/// Idempotent.
pub fn init() {
    if LOG_FD.load(Ordering::Acquire) >= 0 {
        return;
    }
    let path = path_from_env().unwrap_or_else(|| home::get().join("valo.log"));
    // APPEND: the fd sync rewinds the log fd's offset on restore.
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC | OFlags::APPEND | OFlags::CLOEXEC;
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

#[cfg(not(feature = "structured-log"))]
fn init_structured() {}

fn path_from_env() -> Option<PathBuf> {
    std::env::var_os("VALO_LOG").map(PathBuf::from)
}

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
    if level.index() > MAX_LEVEL.load(Ordering::Relaxed) {
        return;
    }
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
