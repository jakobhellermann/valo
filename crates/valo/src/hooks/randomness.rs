// TODO(ai-review): generated, not yet audited
//! Interposed randomness sources — thin entrypoints over the deterministic
//! stream in `determinism::randomness`.

use std::ffi::{c_int, c_uint, c_void};

use rustix::io::Errno;

use crate::determinism::randomness;
use crate::override_sym;

override_sym!(getrandom(buf: *mut c_void, len: libc::size_t, _flags: c_uint) -> libc::ssize_t |real| {
    if !randomness::deterministic() {
        return real(buf, len, _flags);
    }
    if len == 0 {
        return 0;
    }
    if buf.is_null() {
        return super::fail(Errno::FAULT) as libc::ssize_t;
    }
    // SAFETY: getrandom(2) requires `buf` to point to `len` writable bytes; non-null was checked above.
    randomness::fill(unsafe { std::slice::from_raw_parts_mut(buf.cast::<u8>(), len) });
    len as libc::ssize_t
});

override_sym!(getentropy(buf: *mut c_void, len: libc::size_t) -> c_int |real| {
    if !randomness::deterministic() {
        return real(buf, len);
    }
    // getentropy(3): at most 256 bytes per call
    if len > 256 {
        return super::fail(Errno::IO);
    }
    if len == 0 {
        return 0;
    }
    if buf.is_null() {
        return super::fail(Errno::FAULT);
    }
    // SAFETY: getentropy(3) requires `buf` to point to `len` writable bytes; non-null was checked above.
    randomness::fill(unsafe { std::slice::from_raw_parts_mut(buf.cast::<u8>(), len) });
    0
});
