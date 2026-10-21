//! Waits on fds with a timeout.

use std::ffi::c_int;

use crate::determinism::time::timespec_ns;
use crate::determinism::timeout::{self, millis_ns, timeval_ns};
use crate::override_sym;

override_sym!(poll(fds: *mut libc::pollfd, nfds: libc::nfds_t, timeout_ms: c_int) -> c_int |real| {
    let wait_real = || real(fds, nfds, timeout_ms);
    match nfds {
        0 => timeout::wait_nothing(millis_ns(timeout_ms), wait_real),
        _ => timeout::wait_fds(millis_ns(timeout_ms), wait_real),
    }
});

override_sym!(ppoll(fds: *mut libc::pollfd, nfds: libc::nfds_t, timeout: *const libc::timespec, sigmask: *const libc::sigset_t) -> c_int |real| {
    // SAFETY: `timeout` is null or a timespec per the call contract.
    let timeout_ns = unsafe { timeout.as_ref() }.and_then(timespec_ns);
    let wait_real = || real(fds, nfds, timeout, sigmask);
    match nfds {
        0 => timeout::wait_nothing(timeout_ns, wait_real),
        _ => timeout::wait_fds(timeout_ns, wait_real),
    }
});

override_sym!(select(nfds: c_int, readfds: *mut libc::fd_set, writefds: *mut libc::fd_set, exceptfds: *mut libc::fd_set, timeout: *mut libc::timeval) -> c_int |real| {
    // SAFETY: `timeout` is null or a timeval per the call contract.
    let timeout_ns = unsafe { timeout.as_ref() }.and_then(timeval_ns);
    let wait_real = || real(nfds, readfds, writefds, exceptfds, timeout);
    if !waits_on_fds(nfds, readfds, writefds, exceptfds) {
        let ready = timeout::wait_nothing(timeout_ns, wait_real);
        if timeout_ns.is_some() {
            // SAFETY: `timeout` is a valid timeval, as `timeout_ns` is only set for one.
            unsafe { *timeout = libc::timeval { tv_sec: 0, tv_usec: 0 } };
        }
        return ready;
    }
    timeout::wait_fds(timeout_ns, wait_real)
});

override_sym!(pselect(nfds: c_int, readfds: *mut libc::fd_set, writefds: *mut libc::fd_set, exceptfds: *mut libc::fd_set, timeout: *const libc::timespec, sigmask: *const libc::sigset_t) -> c_int |real| {
    // SAFETY: `timeout` is null or a timespec per the call contract.
    let timeout_ns = unsafe { timeout.as_ref() }.and_then(timespec_ns);
    let wait_real = || real(nfds, readfds, writefds, exceptfds, timeout, sigmask);
    match waits_on_fds(nfds, readfds, writefds, exceptfds) {
        true => timeout::wait_fds(timeout_ns, wait_real),
        false => timeout::wait_nothing(timeout_ns, wait_real),
    }
});

override_sym!(epoll_wait(epfd: c_int, events: *mut libc::epoll_event, maxevents: c_int, timeout_ms: c_int) -> c_int |real| {
    timeout::wait_fds(millis_ns(timeout_ms), || real(epfd, events, maxevents, timeout_ms))
});

override_sym!(epoll_pwait(epfd: c_int, events: *mut libc::epoll_event, maxevents: c_int, timeout_ms: c_int, sigmask: *const libc::sigset_t) -> c_int |real| {
    timeout::wait_fds(millis_ns(timeout_ms), || real(epfd, events, maxevents, timeout_ms, sigmask))
});

fn waits_on_fds(
    nfds: c_int,
    readfds: *mut libc::fd_set,
    writefds: *mut libc::fd_set,
    exceptfds: *mut libc::fd_set,
) -> bool {
    nfds != 0 || !readfds.is_null() || !writefds.is_null() || !exceptfds.is_null()
}
