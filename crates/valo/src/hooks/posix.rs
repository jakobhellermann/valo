//! POSIX file open hooks.

use std::ffi::{CStr, VaList, c_char, c_int, c_uint};
use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd};

use rustix::fs::OFlags;

use crate::determinism::randomness::RANDOM;
use crate::{error, override_sym};

override_sym!(variadic open(path: *const c_char, flags: c_int) -> c_int |real, args| {
    // SAFETY: open(2) requires `path` to be null or a NUL-terminated string.
    if let Some(fd) = device_open(unsafe { c_str(path) }, flags) {
        return fd;
    }
    // SAFETY: `args` are this open call's variadic arguments, forwarded unchanged.
    unsafe { real(path, flags, open_mode(flags, &mut args)) }
});

override_sym!(variadic open64(path: *const c_char, flags: c_int) -> c_int |real, args| {
    // SAFETY: open64(2) requires `path` to be null or a NUL-terminated string.
    if let Some(fd) = device_open(unsafe { c_str(path) }, flags) {
        return fd;
    }
    // SAFETY: `args` are this open64 call's variadic arguments, forwarded unchanged.
    unsafe { real(path, flags, open_mode(flags, &mut args)) }
});

override_sym!(variadic openat(dirfd: c_int, path: *const c_char, flags: c_int) -> c_int |real, args| {
    // SAFETY: openat(2) requires `path` to be null or a NUL-terminated string.
    if let Some(fd) = device_open(unsafe { c_str(path) }, flags) {
        return fd;
    }
    // SAFETY: `args` are this openat call's variadic arguments, forwarded unchanged.
    unsafe { real(dirfd, path, flags, open_mode(flags, &mut args)) }
});

override_sym!(variadic openat64(dirfd: c_int, path: *const c_char, flags: c_int) -> c_int |real, args| {
    // SAFETY: openat64(2) requires `path` to be null or a NUL-terminated string.
    if let Some(fd) = device_open(unsafe { c_str(path) }, flags) {
        return fd;
    }
    // SAFETY: `args` are this openat64 call's variadic arguments, forwarded unchanged.
    unsafe { real(dirfd, path, flags, open_mode(flags, &mut args)) }
});

// _FORTIFY_SOURCE redirects open() calls without a mode argument to these
override_sym!(__open_2(path: *const c_char, flags: c_int) -> c_int |real| {
    // SAFETY: open(2) requires `path` to be null or a NUL-terminated string.
    if let Some(fd) = device_open(unsafe { c_str(path) }, flags) {
        return fd;
    }
    real(path, flags)
});

override_sym!(__open64_2(path: *const c_char, flags: c_int) -> c_int |real| {
    // SAFETY: open64(2) requires `path` to be null or a NUL-terminated string.
    if let Some(fd) = device_open(unsafe { c_str(path) }, flags) {
        return fd;
    }
    real(path, flags)
});

override_sym!(__openat_2(dirfd: c_int, path: *const c_char, flags: c_int) -> c_int |real| {
    // SAFETY: openat(2) requires `path` to be null or a NUL-terminated string.
    if let Some(fd) = device_open(unsafe { c_str(path) }, flags) {
        return fd;
    }
    real(dirfd, path, flags)
});

override_sym!(__openat64_2(dirfd: c_int, path: *const c_char, flags: c_int) -> c_int |real| {
    // SAFETY: openat64(2) requires `path` to be null or a NUL-terminated string.
    if let Some(fd) = device_open(unsafe { c_str(path) }, flags) {
        return fd;
    }
    real(dirfd, path, flags)
});

override_sym!(fopen(path: *const c_char, mode: *const c_char) -> *mut libc::FILE |real| {
    // SAFETY: fopen(3) requires `path` and `mode` to be null or NUL-terminated strings.
    if let Some(file) = device_fopen(unsafe { c_str(path) }, unsafe { c_str(mode) }) {
        return file;
    }
    real(path, mode)
});

override_sym!(fopen64(path: *const c_char, mode: *const c_char) -> *mut libc::FILE |real| {
    // SAFETY: fopen64(3) requires `path` and `mode` to be null or NUL-terminated strings.
    if let Some(file) = device_fopen(unsafe { c_str(path) }, unsafe { c_str(mode) }) {
        return file;
    }
    real(path, mode)
});

/// # Safety
/// `ptr` is null or points to a NUL-terminated string that outlives `'a`.
unsafe fn c_str<'a>(ptr: *const c_char) -> Option<&'a CStr> {
    // SAFETY: non-null per the check; NUL-terminated and alive per this function's contract.
    (!ptr.is_null()).then(|| unsafe { CStr::from_ptr(ptr) })
}

/// `Some(fd)` when `path` is a randomness device, `None` to open it for real.
fn device_open(path: Option<&CStr>, flags: c_int) -> Option<c_int> {
    if !crate::is_target() {
        return None;
    }
    if !path.is_some_and(|path| RANDOM.should_hook_random_device(path)) {
        return None;
    }
    let cloexec = OFlags::from_bits_retain(flags as u32).contains(OFlags::CLOEXEC);
    Some(match RANDOM.open_device(cloexec) {
        Ok(fd) => fd.into_raw_fd(),
        Err(e) => {
            error!(Hooks, "randomness: serving a device open failed: {e}");
            super::fail(e)
        }
    })
}

/// `Some(stream)` when `path` is a randomness device, `None` to open it for real.
fn device_fopen(path: Option<&CStr>, mode: Option<&CStr>) -> Option<*mut libc::FILE> {
    let mode = mode?;
    if !path.is_some_and(|path| RANDOM.should_hook_random_device(path)) {
        return None;
    }
    let cloexec = mode.to_bytes().contains(&b'e');
    let raw = match RANDOM.open_device(cloexec) {
        Ok(fd) => fd.into_raw_fd(),
        Err(e) => {
            error!(Hooks, "randomness: serving a device fopen failed: {e}");
            super::fail(e);
            return Some(std::ptr::null_mut());
        }
    };
    // SAFETY: `raw` was just taken from an OwnedFd and is open; `mode` is a valid C string.
    let file = unsafe { libc::fdopen(raw, mode.as_ptr()) };
    if file.is_null() {
        // SAFETY: fdopen failed and did not take ownership, so `raw` is still exclusively ours.
        drop(unsafe { OwnedFd::from_raw_fd(raw) });
    }
    Some(file)
}

/// # Safety
/// `args` are the variadic arguments of an open-family call with these `flags`.
unsafe fn open_mode(flags: c_int, args: &mut VaList) -> c_uint {
    let flags = OFlags::from_bits_retain(flags as u32);
    if flags.contains(OFlags::CREATE) || flags.contains(OFlags::TMPFILE) {
        // SAFETY: open(2) callers pass a mode exactly when O_CREAT or O_TMPFILE is set.
        unsafe { args.next_arg::<c_uint>() }
    } else {
        0
    }
}
