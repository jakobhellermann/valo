// TODO(ai-review): generated, not yet audited
//! Deterministic preloading: every operation queued before a frame's
//! `UpdatePreloading` is through its background `Perform` and integrated by
//! the end of that call, as on an infinitely fast machine. Otherwise the
//! frame an operation finishes on depends on the preload thread's speed.
//!
//! Counted from the hooks: queued (`AddToQueue`), running (`Perform` begin
//! to end) and non-parallel operations between their `Perform` and their
//! integration, which keep the preload thread from starting anything else.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use rustix::thread::futex;

use crate::determinism::time::real_monotonic;
use crate::{debug, error};

static THREAD_STARTED: AtomicBool = AtomicBool::new(false);
static QUEUED: AtomicU32 = AtomicU32::new(0);
static RUNNING: AtomicU32 = AtomicU32::new(0);
static NON_PARALLEL_PENDING: AtomicU32 = AtomicU32::new(0);
/// Bumped on every `Perform` begin and end; the futex word the main thread waits on.
static EVENTS: AtomicU32 = AtomicU32::new(0);

/// A wait this long means the preload thread stopped making progress.
const STALL: Duration = Duration::from_secs(5);

pub(super) fn thread_started() {
    THREAD_STARTED.store(true, Ordering::SeqCst);
}

pub(super) fn queued() {
    QUEUED.fetch_add(1, Ordering::SeqCst);
}

pub(super) fn perform_begin() {
    if QUEUED
        .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
        .is_err()
    {
        error!(
            Unity,
            "preload: Perform began for an operation that was never queued"
        );
    }
    RUNNING.fetch_add(1, Ordering::SeqCst);
    signal();
}

pub(super) fn perform_end(non_parallel: bool) {
    RUNNING.fetch_sub(1, Ordering::SeqCst);
    if non_parallel {
        NON_PARALLEL_PENDING.fetch_add(1, Ordering::SeqCst);
    }
    signal();
}

pub(super) fn integrated_non_parallel() {
    NON_PARALLEL_PENDING.fetch_sub(1, Ordering::SeqCst);
}

/// Runs `update` (the original `UpdatePreloading`) until the preload thread
/// has nothing left it can do this frame, integrating whatever it finishes.
pub(super) fn update(mut update: impl FnMut()) {
    // operations queued after the last frame's update finish before this one looks at them
    wait_until_idle();
    let mut rounds = 0;
    loop {
        let before = EVENTS.load(Ordering::SeqCst);
        update();
        rounds += 1;
        let running = RUNNING.load(Ordering::SeqCst);
        let blocked = NON_PARALLEL_PENDING.load(Ordering::SeqCst) > 0;
        let startable =
            QUEUED.load(Ordering::SeqCst) > 0 && !blocked && THREAD_STARTED.load(Ordering::SeqCst);
        let changed = EVENTS.load(Ordering::SeqCst) != before;
        if running == 0 && !startable {
            if changed {
                // something finished during the call: integrate it too
                continue;
            }
            break;
        }
        if !wait_for_event(before) {
            break;
        }
    }
    if rounds > 1 {
        debug!(
            Unity,
            "preload: {rounds} UpdatePreloading rounds this frame"
        );
    }
}

/// Waits until the preload thread has nothing left it can do (nothing running,
/// nothing startable), so a following wait on it either succeeds at once or
/// runs out with nobody left to signal.
pub(super) fn wait_until_idle() {
    loop {
        let seen = EVENTS.load(Ordering::SeqCst);
        let blocked = NON_PARALLEL_PENDING.load(Ordering::SeqCst) > 0;
        let startable =
            QUEUED.load(Ordering::SeqCst) > 0 && !blocked && THREAD_STARTED.load(Ordering::SeqCst);
        if RUNNING.load(Ordering::SeqCst) == 0 && !startable {
            return;
        }
        if !wait_for_event(seen) {
            return;
        }
    }
}

fn signal() {
    EVENTS.fetch_add(1, Ordering::SeqCst);
    if let Err(e) = futex::wake(&EVENTS, futex::Flags::PRIVATE, u32::MAX) {
        error!(Unity, "preload: futex wake failed: {e}");
    }
}

/// Waits until a `Perform` begins or ends after `seen`; false after a stall.
fn wait_for_event(seen: u32) -> bool {
    let start = real_monotonic();
    // relative timeouts are measured by the kernel in real time
    let slice = rustix::time::Timespec {
        tv_sec: 0,
        tv_nsec: 100_000_000,
    };
    while EVENTS.load(Ordering::SeqCst) == seen {
        if real_monotonic() - start > STALL {
            error!(
                Unity,
                "preload: no progress for 5s (queued {}, running {}, non-parallel pending {})",
                QUEUED.load(Ordering::SeqCst),
                RUNNING.load(Ordering::SeqCst),
                NON_PARALLEL_PENDING.load(Ordering::SeqCst)
            );
            return false;
        }
        match futex::wait(&EVENTS, futex::Flags::PRIVATE, seen, Some(&slice)) {
            Ok(()) => {}
            Err(e)
                if e == rustix::io::Errno::AGAIN
                    || e == rustix::io::Errno::TIMEDOUT
                    || e == rustix::io::Errno::INTR => {}
            Err(e) => {
                error!(Unity, "preload: futex wait failed: {e}");
                return false;
            }
        }
    }
    true
}
