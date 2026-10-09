//! Sleep/wait tracking.

use std::ffi::c_int;

use crate::debug::sleep;
use crate::override_sym;

override_sym!(usleep(usec: libc::useconds_t) -> c_int |real| {
    let ret = real(usec);
    sleep::record(sleep::SleepKind::Usleep, usec as u64 * 1000, false);
    ret
});

override_sym!(nanosleep(
    requested_time: *const libc::timespec,
    remaining: *mut libc::timespec
) -> c_int |real| {
    // SAFETY: pointer contract per the call.
    let ns = unsafe {
        requested_time
            .as_ref()
            .map_or(0, |t| t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64)
    };
    let ret = real(requested_time, remaining);
    sleep::record(sleep::SleepKind::Nanosleep, ns, false);
    ret
});

override_sym!(clock_nanosleep(
    clock_id: libc::clockid_t,
    flags: c_int,
    request: *const libc::timespec,
    remain: *mut libc::timespec
) -> c_int |real| {
    let ns = sleep::clock_nanosleep_duration(clock_id, flags, request);
    let ret = real(clock_id, flags, request, remain);
    sleep::record(sleep::SleepKind::ClockNanosleep, ns, false);
    ret
});

override_sym!(poll(fds: *mut libc::pollfd, nfds: libc::nfds_t, timeout: c_int) -> c_int |real| {
    let ret = real(fds, nfds, timeout);
    let ns = if timeout >= 0 { timeout as u64 * 1_000_000 } else { 0 };
    sleep::record(sleep::SleepKind::Poll, ns, ret == 0 && timeout >= 0);
    ret
});

override_sym!(select(
    nfds: c_int,
    readfds: *mut libc::fd_set,
    writefds: *mut libc::fd_set,
    exceptfds: *mut libc::fd_set,
    timeout: *mut libc::timeval
) -> c_int |real| {
    // SAFETY: pointer contract per the call.
    let ns = unsafe {
        timeout
            .as_ref()
            .map_or(0, |t| t.tv_sec as u64 * 1_000_000_000 + t.tv_usec as u64 * 1000)
    };
    let ret = real(nfds, readfds, writefds, exceptfds, timeout);
    sleep::record(sleep::SleepKind::Select, ns, ret == 0 && !timeout.is_null());
    ret
});

override_sym!(pselect(
    nfds: c_int,
    readfds: *mut libc::fd_set,
    writefds: *mut libc::fd_set,
    exceptfds: *mut libc::fd_set,
    timeout: *const libc::timespec,
    sigmask: *const libc::sigset_t
) -> c_int |real| {
    // SAFETY: pointer contract per the call.
    let ns = unsafe {
        timeout
            .as_ref()
            .map_or(0, |t| t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64)
    };
    let ret = real(nfds, readfds, writefds, exceptfds, timeout, sigmask);
    sleep::record(sleep::SleepKind::Pselect, ns, ret == 0 && !timeout.is_null());
    ret
});

override_sym!(epoll_wait(
    epfd: c_int,
    events: *mut libc::epoll_event,
    maxevents: c_int,
    timeout: c_int
) -> c_int |real| {
    let ret = real(epfd, events, maxevents, timeout);
    let ns = if timeout >= 0 { timeout as u64 * 1_000_000 } else { 0 };
    sleep::record(sleep::SleepKind::EpollWait, ns, ret == 0 && timeout >= 0);
    ret
});

override_sym!(sched_yield() -> c_int |real| {
    let ret = real();
    sleep::record(sleep::SleepKind::SchedYield, 0, false);
    ret
});

// _FORTIFY_SOURCE `poll`
override_sym!(__poll_chk(
    fds: *mut libc::pollfd,
    nfds: libc::nfds_t,
    timeout: c_int,
    fdslen: usize
) -> c_int |real| {
    let ret = real(fds, nfds, timeout, fdslen);
    let ns = if timeout >= 0 { timeout as u64 * 1_000_000 } else { 0 };
    sleep::record(sleep::SleepKind::PollChk, ns, ret == 0 && timeout >= 0);
    ret
});

override_sym!(ppoll(
    fds: *mut libc::pollfd,
    nfds: libc::nfds_t,
    timeout: *const libc::timespec,
    sigmask: *const libc::sigset_t
) -> c_int |real| {
    // SAFETY: pointer contract per the call.
    let ns = unsafe {
        timeout
            .as_ref()
            .map_or(0, |t| t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64)
    };
    let ret = real(fds, nfds, timeout, sigmask);
    sleep::record(sleep::SleepKind::Ppoll, ns, ret == 0 && !timeout.is_null());
    ret
});

override_sym!(__ppoll_chk(
    fds: *mut libc::pollfd,
    nfds: libc::nfds_t,
    timeout: *const libc::timespec,
    sigmask: *const libc::sigset_t,
    fdslen: usize
) -> c_int |real| {
    // SAFETY: pointer contract per the call.
    let ns = unsafe {
        timeout
            .as_ref()
            .map_or(0, |t| t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64)
    };
    let ret = real(fds, nfds, timeout, sigmask, fdslen);
    sleep::record(sleep::SleepKind::PpollChk, ns, ret == 0 && !timeout.is_null());
    ret
});

override_sym!(epoll_pwait(
    epfd: c_int,
    events: *mut libc::epoll_event,
    maxevents: c_int,
    timeout: c_int,
    sigmask: *const libc::sigset_t
) -> c_int |real| {
    let ret = real(epfd, events, maxevents, timeout, sigmask);
    let ns = if timeout >= 0 { timeout as u64 * 1_000_000 } else { 0 };
    sleep::record(sleep::SleepKind::EpollPwait, ns, ret == 0 && timeout >= 0);
    ret
});

override_sym!(epoll_pwait2(
    epfd: c_int,
    events: *mut libc::epoll_event,
    maxevents: c_int,
    timeout: *const libc::timespec,
    sigmask: *const libc::sigset_t
) -> c_int |real| {
    // SAFETY: pointer contract per the call.
    let ns = unsafe {
        timeout
            .as_ref()
            .map_or(0, |t| t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64)
    };
    let ret = real(epfd, events, maxevents, timeout, sigmask);
    sleep::record(sleep::SleepKind::EpollPwait2, ns, ret == 0 && !timeout.is_null());
    ret
});
