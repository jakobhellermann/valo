// TODO(ai-review): generated, not yet audited
//! Deterministic preloading: every operation queued before a frame's
//! `UpdatePreloading` is through its background `Perform` and integrated by
//! the end of that call, as on an infinitely fast machine. Otherwise the
//! frame an operation finishes on depends on the preload thread's speed.
//!
//! Counted from the hooks: queued (`AddToQueue`), running (`Perform` begin
//! to end) and non-parallel operations between their `Perform` and their
//! integration, which keep the preload thread from starting anything else.
//! An operation counts as through `Perform` only once `PreloadManager::Run`
//! marked it, which happens after `Perform` returns.

use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};

use crate::determinism::time::real_monotonic;
use crate::{debug, error};

/// Offset of the status in a `PreloadManagerOperation`: 0 until `Run` sets it
/// after `Perform`; the main thread integrates nothing before.
const OPERATION_STATUS_OFFSET: usize = 0x40;

static THREAD_STARTED: AtomicBool = AtomicBool::new(false);
static QUEUED: AtomicU32 = AtomicU32::new(0);
static RUNNING: AtomicU32 = AtomicU32::new(0);
static NON_PARALLEL_PENDING: AtomicU32 = AtomicU32::new(0);
/// Bumped on every `Perform` begin and end; the futex word the main thread waits on.
static EVENTS: AtomicU32 = AtomicU32::new(0);
/// Operations through `Perform` that are neither marked by `Run` nor integrated yet.
static UNMARKED: Mutex<Vec<usize>> = Mutex::new(Vec::new());

pub(super) fn thread_started() {
    THREAD_STARTED.store(true, Ordering::SeqCst);
}

pub(super) fn queued() {
    QUEUED.fetch_add(1, Ordering::SeqCst);
}

pub(super) fn perform_begin(op: *mut c_void) {
    // SAFETY: `op` is the live operation whose `Perform` is about to run.
    let status = unsafe { status(op as usize) };
    if status != 0 {
        error!(
            Unity,
            "preload: operation {op:p} has status {status} before Perform"
        );
    }
    RUNNING.fetch_add(1, Ordering::SeqCst);
    if QUEUED
        .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
        .is_err()
    {
        error!(
            Unity,
            "preload: Perform began for an operation that was never queued"
        );
    }
    signal();
}

pub(super) fn perform_end(op: *mut c_void, non_parallel: bool) {
    // listed first: the main thread may see RUNNING drop before `Run` marks the operation
    unmarked().push(op as usize);
    if non_parallel {
        NON_PARALLEL_PENDING.fetch_add(1, Ordering::SeqCst);
    }
    RUNNING.fetch_sub(1, Ordering::SeqCst);
    signal();
}

pub(super) fn integrated(op: *mut c_void, non_parallel: bool) {
    unmarked().retain(|&unmarked| unmarked != op as usize);
    if non_parallel {
        NON_PARALLEL_PENDING.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Runs `update` (the original `UpdatePreloading`) until the preload thread
/// has nothing left it can do this frame, integrating whatever it finishes.
pub(super) fn update(mut update: impl FnMut()) {
    // operations queued after the last frame's update finish before this one looks at them
    wait_until_idle();
    let mut rounds = 0;
    loop {
        wait_until_marked();
        let before = EVENTS.load(Ordering::SeqCst);
        update();
        rounds += 1;
        if idle() {
            let changed = EVENTS.load(Ordering::SeqCst) != before;
            if changed || !unmarked().is_empty() {
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
        if idle() {
            return wait_until_marked();
        }
        if !wait_for_event(seen) {
            return;
        }
    }
}

/// Whether the preload thread has nothing left it can do. Each counter pair is
/// read in the reverse order of the hooks' writes, so an operation is never
/// missed while it moves from queued to running to performed.
fn idle() -> bool {
    let queued = QUEUED.load(Ordering::SeqCst) > 0;
    let running = RUNNING.load(Ordering::SeqCst) > 0;
    let blocked = NON_PARALLEL_PENDING.load(Ordering::SeqCst) > 0;
    let startable = queued && !blocked && THREAD_STARTED.load(Ordering::SeqCst);
    !running && !startable
}

/// Waits until `Run` marked every operation through `Perform`. Main thread only:
/// an operation leaves the list before its integration can free it.
fn wait_until_marked() {
    let start = real_monotonic();
    let mut yields = 0;
    loop {
        // SAFETY: listed operations are not integrated yet, so still alive.
        unmarked().retain(|&op| unsafe { status(op) } == 0);
        if unmarked().is_empty() {
            if yields > 0 {
                debug!(Unity, "preload: waited {yields} yields for Run to mark");
            }
            return;
        }
        if real_monotonic() - start > super::STALL {
            error!(
                Unity,
                "preload: operations {:x?} not marked after Perform for 5s",
                *unmarked()
            );
            return;
        }
        rustix::thread::sched_yield();
        yields += 1;
    }
}

/// # Safety
/// `op` must be a live `PreloadManagerOperation`.
unsafe fn status(op: usize) -> i32 {
    // SAFETY: the status is an aligned i32 of the live operation, written by `Run`.
    unsafe { &*((op + OPERATION_STATUS_OFFSET) as *const AtomicI32) }.load(Ordering::SeqCst)
}

fn unmarked() -> std::sync::MutexGuard<'static, Vec<usize>> {
    UNMARKED.lock().expect("preload: UNMARKED poisoned")
}

fn signal() {
    super::signal(&EVENTS);
}

/// Waits until a `Perform` begins or ends after `seen`; false after a stall.
fn wait_for_event(seen: u32) -> bool {
    super::wait_for_change(&EVENTS, seen, || {
        format!(
            "preload: no progress (queued {}, running {}, non-parallel pending {})",
            QUEUED.load(Ordering::SeqCst),
            RUNNING.load(Ordering::SeqCst),
            NON_PARALLEL_PENDING.load(Ordering::SeqCst)
        )
    })
}
