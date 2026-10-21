use std::ffi::c_int;

use crate::determinism::time::CLOCK;
use crate::override_sym;

const NS_PER_SEC: u64 = 1_000_000_000;

override_sym!(clock_gettime(clock: libc::clockid_t, tp: *mut libc::timespec) -> c_int |real| {
    match CLOCK.now_ns(clock) {
        Some(ns) if !tp.is_null() => {
            // SAFETY: `tp` is non-null and points to a timespec per the call contract.
            unsafe {
                tp.write(libc::timespec {
                    tv_sec: (ns / NS_PER_SEC) as libc::time_t,
                    tv_nsec: (ns % NS_PER_SEC) as libc::c_long,
                })
            };
            0
        }
        _ => real(clock, tp),
    }
});

override_sym!(gettimeofday(tv: *mut libc::timeval, tz: *mut libc::timezone) -> c_int |real| {
    match CLOCK.now_ns(libc::CLOCK_REALTIME) {
        Some(ns) if !tv.is_null() => {
            // SAFETY: `tv` is non-null and points to a timeval per the call contract.
            unsafe {
                tv.write(libc::timeval {
                    tv_sec: (ns / NS_PER_SEC) as libc::time_t,
                    tv_usec: (ns % NS_PER_SEC / 1000) as libc::suseconds_t,
                })
            };
            if tz.is_null() { 0 } else { real(std::ptr::null_mut(), tz) }
        }
        _ => real(tv, tz),
    }
});

override_sym!(time(t: *mut libc::time_t) -> libc::time_t |real| {
    match CLOCK.now_ns(libc::CLOCK_REALTIME) {
        Some(ns) => {
            let secs = (ns / NS_PER_SEC) as libc::time_t;
            if !t.is_null() {
                // SAFETY: `t` is non-null and points to a time_t per the call contract.
                unsafe { t.write(secs) };
            }
            secs
        }
        None => real(t),
    }
});
