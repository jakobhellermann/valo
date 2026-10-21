//! Timed waits whose absolute deadline lies on a virtual clock.

use std::ffi::c_int;

use crate::determinism::deadline;
use crate::override_sym;

override_sym!(pthread_cond_init(cond: *mut libc::pthread_cond_t, attr: *const libc::pthread_condattr_t) -> c_int |real| {
    let ret = real(cond, attr);
    if ret == 0 {
        let mut clock = libc::CLOCK_REALTIME;
        if !attr.is_null() {
            // SAFETY: a non-null `attr` is an initialized condattr per the call contract.
            unsafe { libc::pthread_condattr_getclock(attr, &mut clock) };
        }
        deadline::track_cond(cond, clock);
    }
    ret
});

override_sym!(pthread_cond_destroy(cond: *mut libc::pthread_cond_t) -> c_int |real| {
    deadline::forget_cond(cond);
    real(cond)
});

override_sym!(pthread_cond_timedwait(cond: *mut libc::pthread_cond_t, mutex: *mut libc::pthread_mutex_t, abstime: *const libc::timespec) -> c_int |real| {
    // SAFETY: `abstime` is null or a timespec per the call contract.
    unsafe { deadline::wait(deadline::cond_clock(cond), abstime, |abstime| real(cond, mutex, abstime), is_timeout) }
});

override_sym!(pthread_cond_clockwait(cond: *mut libc::pthread_cond_t, mutex: *mut libc::pthread_mutex_t, clock: libc::clockid_t, abstime: *const libc::timespec) -> c_int |real| {
    // SAFETY: `abstime` is null or a timespec per the call contract.
    unsafe { deadline::wait(clock, abstime, |abstime| real(cond, mutex, clock, abstime), is_timeout) }
});

override_sym!(sem_timedwait(sem: *mut libc::sem_t, abstime: *const libc::timespec) -> c_int |real| {
    // SAFETY: `abstime` is null or a timespec per the call contract.
    unsafe { deadline::wait(libc::CLOCK_REALTIME, abstime, |abstime| real(sem, abstime), failed_with_timeout) }
});

override_sym!(sem_clockwait(sem: *mut libc::sem_t, clock: libc::clockid_t, abstime: *const libc::timespec) -> c_int |real| {
    // SAFETY: `abstime` is null or a timespec per the call contract.
    unsafe { deadline::wait(clock, abstime, |abstime| real(sem, clock, abstime), failed_with_timeout) }
});

fn is_timeout(ret: c_int) -> bool {
    ret == libc::ETIMEDOUT
}

fn failed_with_timeout(ret: c_int) -> bool {
    ret == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ETIMEDOUT)
}
