// TODO(ai-review): generated, not yet audited
//! Interposed game-visible time sources — thin entrypoints only. Each
//! override counts the read and delegates to the virtual clock in
//! `determinism::time`, falling through to the real call when that source
//! keeps real time (config gate, tick state and diagnostics live there).

use std::ffi::{c_int, c_void};

use crate::determinism::time;
use crate::override_sym;

override_sym!(clock_gettime(clock_id: libc::clockid_t, tp: *mut libc::timespec) -> c_int |real| {
    match time::clock_gettime(clock_id, tp) {
        Some(rv) => rv,
        None => real(clock_id, tp),
    }
});

override_sym!(gettimeofday(tv: *mut libc::timeval, tz: *mut c_void) -> c_int |real| {
    match time::gettimeofday(tv, tz) {
        Some(rv) => rv,
        None => real(tv, tz),
    }
});

override_sym!(time(t: *mut libc::time_t) -> libc::time_t |real| {
    match time::time(t) {
        Some(rv) => rv,
        None => real(t),
    }
});
