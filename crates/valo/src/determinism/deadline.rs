//! Absolute deadlines of timed waits, moved from the virtual clocks to the real ones.

use std::collections::HashMap;
use std::ffi::c_int;
use std::sync::{LazyLock, Mutex};

use crate::determinism::time::CLOCK;
use crate::thread_tracker;

type CondAddress = usize;

/// Condition variables that wait on another clock than `CLOCK_REALTIME`.
static COND_CLOCKS: LazyLock<Mutex<HashMap<CondAddress, libc::clockid_t>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn track_cond(cond: *mut libc::pthread_cond_t, clock: libc::clockid_t) {
    let mut clocks = COND_CLOCKS.lock().unwrap();
    if clock == libc::CLOCK_REALTIME {
        clocks.remove(&(cond as CondAddress));
    } else {
        clocks.insert(cond as CondAddress, clock);
    }
}

pub fn forget_cond(cond: *mut libc::pthread_cond_t) {
    COND_CLOCKS.lock().unwrap().remove(&(cond as CondAddress));
}

pub fn cond_clock(cond: *mut libc::pthread_cond_t) -> libc::clockid_t {
    COND_CLOCKS
        .lock()
        .unwrap()
        .get(&(cond as CondAddress))
        .copied()
        .unwrap_or(libc::CLOCK_REALTIME)
}

/// Runs `wait_real` with the real deadline for `deadline` on `clock`; when that times out on the
/// frame thread, the virtual clock moves on to `deadline`.
///
/// # Safety
/// `deadline` is null or points to a timespec.
pub unsafe fn wait(
    clock: libc::clockid_t,
    deadline: *const libc::timespec,
    wait_real: impl FnOnce(*const libc::timespec) -> c_int,
    timed_out: impl FnOnce(c_int) -> bool,
) -> c_int {
    // SAFETY: null or a timespec per this function's contract.
    let converted =
        unsafe { deadline.as_ref() }.and_then(|deadline| CLOCK.real_deadline(clock, deadline));
    let Some((real_deadline, ahead_ns)) = converted else {
        return wait_real(deadline);
    };
    let ret = wait_real(&real_deadline);
    if timed_out(ret) && thread_tracker::is_main() {
        CLOCK.sleep(ahead_ns);
    }
    ret
}
