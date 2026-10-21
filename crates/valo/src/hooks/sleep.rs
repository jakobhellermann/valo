use std::ffi::c_int;

use crate::determinism::deadline;
use crate::determinism::time::{CLOCK, timespec_ns};
use crate::{override_sym, thread_tracker};

/// How much virtual time a sleep passes, or `None` when it sleeps for real.
fn virtual_sleep_ns(clock: libc::clockid_t, flags: c_int, request: &libc::timespec) -> Option<u64> {
    if !thread_tracker::is_main() {
        return None;
    }

    let ns = timespec_ns(request)?;
    let now = CLOCK.now_ns(clock)?;

    Some(if flags & libc::TIMER_ABSTIME != 0 {
        ns.saturating_sub(now)
    } else {
        ns
    })
}

override_sym!(nanosleep(request: *const libc::timespec, remaining: *mut libc::timespec) -> c_int |real| {
    // SAFETY: a non-null `request` points to a timespec per the call contract.
    match unsafe { request.as_ref() }.and_then(|request| virtual_sleep_ns(libc::CLOCK_MONOTONIC, 0, request)) {
        Some(ns) => {
            CLOCK.sleep(ns);
            0
        }
        None => real(request, remaining),
    }
});

override_sym!(clock_nanosleep(
    clock: libc::clockid_t,
    flags: c_int,
    request: *const libc::timespec,
    remaining: *mut libc::timespec
) -> c_int |real| {
    // SAFETY: a non-null `request` points to a timespec per the call contract.
    match unsafe { request.as_ref() }.and_then(|request| virtual_sleep_ns(clock, flags, request)) {
        Some(ns) => {
            CLOCK.sleep(ns);
            0
        }
        // SAFETY: `request` is null or a timespec per the call contract.
        None if flags & libc::TIMER_ABSTIME != 0 => unsafe {
            deadline::wait(clock, request, |request| real(clock, flags, request, remaining), |ret| ret == 0)
        },
        None => real(clock, flags, request, remaining),
    }
});

override_sym!(usleep(usec: libc::useconds_t) -> c_int |real| {
    let request = libc::timespec {
        tv_sec: (usec / 1_000_000) as libc::time_t,
        tv_nsec: (usec % 1_000_000 * 1000) as libc::c_long,
    };
    match virtual_sleep_ns(libc::CLOCK_MONOTONIC, 0, &request) {
        Some(ns) => {
            CLOCK.sleep(ns);
            0
        }
        None => real(usec),
    }
});
