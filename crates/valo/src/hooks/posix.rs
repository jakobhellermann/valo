// TODO(ai-review): generated, not yet audited
//! Interposed POSIX file opens — thin entrypoints; randomness devices are
//! served by `determinism::randomness`, everything else goes to libc.

use std::ffi::{CStr, VaList, c_char, c_int, c_uint};
use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd};

use rustix::fs::OFlags;

use crate::determinism::randomness;
use crate::{error, override_sym};

override_sym!(variadic open(path: *const c_char, flags: c_int) -> c_int |real, args| {
    // SAFETY: open(2) requires `path` to be null or a NUL-terminated string.
    if let Some(fd) = unsafe { device_open(path, flags) } {
        return fd;
    }
    // SAFETY: `args` are this open call's variadic arguments, forwarded unchanged.
    unsafe { real(path, flags, open_mode(flags, &mut args)) }
});

override_sym!(variadic open64(path: *const c_char, flags: c_int) -> c_int |real, args| {
    // SAFETY: open64(2) requires `path` to be null or a NUL-terminated string.
    if let Some(fd) = unsafe { device_open(path, flags) } {
        return fd;
    }
    // SAFETY: `args` are this open64 call's variadic arguments, forwarded unchanged.
    unsafe { real(path, flags, open_mode(flags, &mut args)) }
});

override_sym!(variadic openat(dirfd: c_int, path: *const c_char, flags: c_int) -> c_int |real, args| {
    // SAFETY: openat(2) requires `path` to be null or a NUL-terminated string.
    if let Some(fd) = unsafe { device_open(path, flags) } {
        return fd;
    }
    // SAFETY: `args` are this openat call's variadic arguments, forwarded unchanged.
    unsafe { real(dirfd, path, flags, open_mode(flags, &mut args)) }
});

override_sym!(variadic openat64(dirfd: c_int, path: *const c_char, flags: c_int) -> c_int |real, args| {
    // SAFETY: openat64(2) requires `path` to be null or a NUL-terminated string.
    if let Some(fd) = unsafe { device_open(path, flags) } {
        return fd;
    }
    // SAFETY: `args` are this openat64 call's variadic arguments, forwarded unchanged.
    unsafe { real(dirfd, path, flags, open_mode(flags, &mut args)) }
});

override_sym!(fopen(path: *const c_char, mode: *const c_char) -> *mut libc::FILE |real| {
    // SAFETY: fopen(3) requires `path` and `mode` to be null or NUL-terminated strings.
    if let Some(file) = unsafe { device_fopen(path, mode) } {
        return file;
    }
    real(path, mode)
});

override_sym!(fopen64(path: *const c_char, mode: *const c_char) -> *mut libc::FILE |real| {
    // SAFETY: fopen64(3) requires `path` and `mode` to be null or NUL-terminated strings.
    if let Some(file) = unsafe { device_fopen(path, mode) } {
        return file;
    }
    real(path, mode)
});

/// `Some(fd)` (`Some(-1)` with errno on failure) when `path` is a randomness
/// device, `None` to open it for real.
///
/// # Safety
/// `path` is null or points to a NUL-terminated string.
unsafe fn device_open(path: *const c_char, flags: c_int) -> Option<c_int> {
    // SAFETY: forwarded from this function's contract.
    if !unsafe { is_device(path) } {
        return None;
    }
    let cloexec = OFlags::from_bits_retain(flags as u32).contains(OFlags::CLOEXEC);
    Some(match randomness::open_device(cloexec) {
        Ok(fd) => fd.into_raw_fd(),
        Err(e) => {
            error!(Hooks, "randomness: serving a device open failed: {e}");
            super::fail(e)
        }
    })
}

/// `Some(stream)` (null with errno on failure) when `path` is a randomness
/// device, `None` to open it for real.
///
/// # Safety
/// `path` and `mode` are null or point to NUL-terminated strings.
unsafe fn device_fopen(path: *const c_char, mode: *const c_char) -> Option<*mut libc::FILE> {
    // SAFETY: forwarded from this function's contract.
    if !unsafe { is_device(path) } {
        return None;
    }
    // SAFETY: forwarded from this function's contract; null was checked first.
    let cloexec = !mode.is_null() && unsafe { CStr::from_ptr(mode) }.to_bytes().contains(&b'e');
    let raw = match randomness::open_device(cloexec) {
        Ok(fd) => fd.into_raw_fd(),
        Err(e) => {
            error!(Hooks, "randomness: serving a device fopen failed: {e}");
            super::fail(e);
            return Some(std::ptr::null_mut());
        }
    };
    // SAFETY: `raw` was just taken from an OwnedFd and is open; `mode` is the caller's per this function's contract.
    let file = unsafe { libc::fdopen(raw, mode) };
    if file.is_null() {
        // SAFETY: fdopen failed and did not take ownership, so `raw` is still exclusively ours.
        drop(unsafe { OwnedFd::from_raw_fd(raw) });
    }
    Some(file)
}

/// # Safety
/// `path` is null or points to a NUL-terminated string.
unsafe fn is_device(path: *const c_char) -> bool {
    // SAFETY: forwarded from this function's contract; null was checked first.
    !path.is_null() && randomness::is_device(unsafe { CStr::from_ptr(path) })
}

/// The open family's optional mode argument, present with O_CREAT/O_TMPFILE.
///
/// # Safety
/// `args` are the variadic arguments of an open-family call with these `flags`.
unsafe fn open_mode(flags: c_int, args: &mut VaList) -> c_uint {
    let flags = OFlags::from_bits_retain(flags as u32);
    if flags.intersects(OFlags::CREATE | OFlags::TMPFILE) {
        // SAFETY: open(2) callers pass a mode exactly when O_CREAT or O_TMPFILE is set.
        unsafe { args.next_arg::<c_uint>() }
    } else {
        0
    }
}
