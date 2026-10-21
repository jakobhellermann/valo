//! Timeouts of poll, select and epoll on the frame thread, counted on the virtual clock.

use std::ffi::c_int;

use crate::determinism::time::CLOCK;
use crate::thread_tracker;

/// Runs `wait_real`; if that times out on the frame thread, also adds `timeout_ns` to the virtual
/// clock.
pub fn wait_fds(timeout_ns: Option<u64>, wait_real: impl FnOnce() -> c_int) -> c_int {
    let ready = wait_real();
    if ready == 0
        && let Some(timeout_ns) = frame_thread_timeout(timeout_ns)
    {
        CLOCK.sleep(timeout_ns);
    }
    ready
}

/// On the frame thread, adds `timeout_ns` to the virtual clock and returns 0 without waiting;
/// elsewhere runs `wait_real`.
pub fn wait_nothing(timeout_ns: Option<u64>, wait_real: impl FnOnce() -> c_int) -> c_int {
    match frame_thread_timeout(timeout_ns) {
        Some(timeout_ns) => {
            CLOCK.sleep(timeout_ns);
            0
        }
        None => wait_real(),
    }
}

fn frame_thread_timeout(timeout_ns: Option<u64>) -> Option<u64> {
    timeout_ns.filter(|&ns| ns > 0 && thread_tracker::is_main() && CLOCK.is_controlled())
}

/// `None` for a negative (infinite) timeout.
pub fn millis_ns(timeout_ms: c_int) -> Option<u64> {
    u64::try_from(timeout_ms).ok().map(|ms| ms * 1_000_000)
}

/// `None` for an invalid timeval.
pub fn timeval_ns(tv: &libc::timeval) -> Option<u64> {
    let secs = u64::try_from(tv.tv_sec).ok()?;
    let micros = u64::try_from(tv.tv_usec)
        .ok()
        .filter(|&micros| micros < 1_000_000)?;
    secs.checked_mul(1_000_000_000)?.checked_add(micros * 1000)
}
