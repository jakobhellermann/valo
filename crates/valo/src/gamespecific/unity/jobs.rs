// TODO(ai-review): generated, not yet audited
//! Deterministic jobs: a job the main thread hands to the workers is done
//! before the main thread continues, as on an infinitely fast machine.
//! Otherwise whether it is done by a later poll depends on the workers' speed.
//!
//! Only the main thread waits: workers waiting for the jobs they enqueue could
//! block each other.

use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::thread_tracker;

/// Jobs the main thread enqueued that did not complete yet.
static PENDING: Mutex<Vec<usize>> = Mutex::new(Vec::new());
/// Bumped whenever a pending job completes; the futex word the main thread waits on.
static EVENTS: AtomicU32 = AtomicU32::new(0);

/// Runs `enqueue` (the original enqueue of `job`) and, on the main thread,
/// waits until `job` completed.
pub(super) fn enqueue(job: *mut c_void, enqueue: impl FnOnce()) {
    if !thread_tracker::is_main() {
        return enqueue();
    }
    let job = job as usize;
    // registered first: a worker may complete it before the enqueue returns
    pending().push(job);
    enqueue();
    loop {
        let seen = EVENTS.load(Ordering::SeqCst);
        if !pending().contains(&job) {
            return;
        }
        if !super::wait_for_change(&EVENTS, seen, || {
            format!("jobs: job {job:#x} not completed")
        }) {
            pending().retain(|&pending| pending != job);
            return;
        }
    }
}

pub(super) fn completed(job: *mut c_void) {
    let mut pending = pending();
    if let Some(i) = pending.iter().position(|&p| p == job as usize) {
        pending.swap_remove(i);
        drop(pending);
        super::signal(&EVENTS);
    }
}

fn pending() -> std::sync::MutexGuard<'static, Vec<usize>> {
    PENDING.lock().expect("jobs: PENDING poisoned")
}
