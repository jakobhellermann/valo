// TODO(ai-review): generated, not yet audited
//! Track pthread lifecycle in a table that checkpoints rewind like any other
//! process memory.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU64, AtomicUsize, Ordering};

use libc::{pid_t, pthread_t};

use crate::abort;

pub(crate) const MAX_THREADS: usize = 256;

#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ThreadId(u64);

impl ThreadId {
    pub(crate) const UNUSED: Self = Self(0);
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    /// Zero so the zero-initialized table starts all-free.
    Free = 0,
    Starting = 1,
    Running = 2,
    Exited = 3,
}

#[repr(u8)]
#[derive(Clone, Copy)]
pub(crate) enum ParkState {
    /// Not part of a suspension; the handler ignores foreign signals.
    Running = 0,
    /// suspend_others signaled the thread.
    Signaled = 1,
    /// Parked at the deterministic stack position.
    Parked = 2,
}

pub(crate) struct ThreadSlot {
    phase: AtomicU8, // Phase
    id: AtomicU64,
    pthread: AtomicUsize,
    tid: AtomicI32,
    detached: AtomicBool,
    /// Stores `ParkState`: the suspend gate; the handler only parks threads
    /// signaled by suspend_others. `ParkState::Running == 0` matches the
    /// zero-initialized table.
    park: AtomicU8,
    pub(crate) park_address: AtomicUsize,
    pub(crate) stack_low: AtomicUsize,
}

impl ThreadSlot {
    const fn new() -> Self {
        Self {
            phase: AtomicU8::new(Phase::Free as u8),
            id: AtomicU64::new(0),
            pthread: AtomicUsize::new(0),
            tid: AtomicI32::new(0),
            detached: AtomicBool::new(false),
            park: AtomicU8::new(ParkState::Running as u8),
            park_address: AtomicUsize::new(0),
            stack_low: AtomicUsize::new(0),
        }
    }

    /// CAS transition of the park state; argument order follows libTAS
    /// updateState: new state first, expected state second.
    pub(crate) fn update_state(&self, to: ParkState, from: ParkState) -> bool {
        self.park
            .compare_exchange(from as u8, to as u8, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    fn phase(&self) -> Phase {
        match self.phase.load(Ordering::Acquire) {
            bits if bits == Phase::Free as u8 => Phase::Free,
            bits if bits == Phase::Starting as u8 => Phase::Starting,
            bits if bits == Phase::Running as u8 => Phase::Running,
            bits if bits == Phase::Exited as u8 => Phase::Exited,
            _ => {
                abort!("thread slot phase corrupted");
            }
        }
    }

    fn clear(&self) {
        self.pthread.store(0, Ordering::Relaxed);
        self.tid.store(0, Ordering::Relaxed);
        self.id.store(0, Ordering::Relaxed);
        self.phase.store(Phase::Free as u8, Ordering::Release);
    }
}

struct ThreadTable {
    slots: [ThreadSlot; MAX_THREADS],
    next_id: AtomicU64,
    lifecycle_lock: AtomicBool,
}

impl ThreadTable {
    const fn new() -> Self {
        Self {
            slots: [const { ThreadSlot::new() }; MAX_THREADS],
            next_id: AtomicU64::new(1),
            lifecycle_lock: AtomicBool::new(false),
        }
    }

    fn reserve(&self, detached: bool) -> Option<ThreadId> {
        for slot in &self.slots {
            if slot.phase() != Phase::Free {
                continue;
            }
            // Claim with a CAS so concurrent creators cannot take the same slot.
            if slot
                .phase
                .compare_exchange(
                    Phase::Free as u8,
                    Phase::Starting as u8,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_err()
            {
                continue;
            }
            let id = ThreadId(self.next_id.fetch_add(1, Ordering::Relaxed));
            slot.id.store(id.0, Ordering::Relaxed);
            slot.detached.store(detached, Ordering::Relaxed);
            return Some(id);
        }
        None
    }

    fn slot_of(&self, id: ThreadId) -> Option<usize> {
        self.slots
            .iter()
            .position(|slot| slot.phase() != Phase::Free && slot.id.load(Ordering::Relaxed) == id.0)
    }

    fn attach(&self, id: ThreadId, pthread: pthread_t) {
        // A detached child may have exited and retired before the parent
        // returns from pthread_create. That missing record is expected.
        if let Some(slot) = self.slot_of(id) {
            self.slots[slot]
                .pthread
                .store(pthread as usize, Ordering::Relaxed);
        }
    }

    fn start(&self, id: ThreadId, pthread: pthread_t, tid: pid_t) {
        let slot = &self.slots[self.slot_of(id).expect("starting untracked thread")];
        slot.pthread.store(pthread as usize, Ordering::Relaxed);
        slot.tid.store(tid, Ordering::Relaxed);
        // Publish pthread and tid before the slot becomes visible as running.
        slot.phase.store(Phase::Running as u8, Ordering::Release);
    }

    fn exit(&self, id: ThreadId) {
        let slot = &self.slots[self.slot_of(id).expect("exiting untracked thread")];
        if slot.detached.load(Ordering::Relaxed) {
            slot.clear();
        } else {
            slot.phase.store(Phase::Exited as u8, Ordering::Release);
        }
    }

    fn creation_failed(&self, id: ThreadId) {
        let slot = self
            .slot_of(id)
            .filter(|&slot| self.slots[slot].phase() == Phase::Starting)
            .expect("failed create had no registry entry");
        self.slots[slot].clear();
    }

    fn join(&self, pthread: pthread_t) -> ThreadId {
        let slot = self
            .slots
            .iter()
            .find(|slot| {
                slot.pthread.load(Ordering::Relaxed) == pthread as usize
                    && slot.phase() == Phase::Exited
            })
            .unwrap_or_else(|| panic!("joined untracked thread"));
        let id = ThreadId(slot.id.load(Ordering::Relaxed));
        slot.clear();
        id
    }

    fn detach(&self, pthread: pthread_t) -> ThreadId {
        let slot = self
            .slots
            .iter()
            .find(|slot| {
                slot.pthread.load(Ordering::Relaxed) == pthread as usize
                    && slot.phase() != Phase::Free
            })
            .unwrap_or_else(|| panic!("detached untracked thread"));
        let id = ThreadId(slot.id.load(Ordering::Relaxed));
        if slot.phase() == Phase::Exited {
            slot.clear();
        } else {
            slot.detached.store(true, Ordering::Relaxed);
        }
        id
    }

    fn running_threads(&self) -> impl Iterator<Item = (ThreadId, usize, pthread_t)> + '_ {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(slot, record)| match record.phase() {
                Phase::Starting => {
                    abort!("thread creation in progress during snapshot");
                }
                Phase::Running => Some((
                    ThreadId(record.id.load(Ordering::Acquire)),
                    slot,
                    record.pthread.load(Ordering::Acquire) as pthread_t,
                )),
                Phase::Free | Phase::Exited => None,
            })
    }

    fn lock(&self) -> LifecycleGuard<'_> {
        while self
            .lifecycle_lock
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            std::hint::spin_loop();
        }
        LifecycleGuard(&self.lifecycle_lock)
    }
}

struct LifecycleGuard<'a>(&'a AtomicBool);

impl Drop for LifecycleGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

static TABLE: ThreadTable = ThreadTable::new();

static SUSPEND_ACTIVE: AtomicBool = AtomicBool::new(false);

static CANCELLATION_SEEN: AtomicBool = AtomicBool::new(false);

thread_local! {
    static CURRENT_ID: Cell<Option<ThreadId>> = const { Cell::new(None) };
}

static READY: AtomicBool = AtomicBool::new(false);

pub(crate) fn current_id() -> ThreadId {
    CURRENT_ID
        .with(Cell::get)
        .expect("snapshot from untracked thread")
}

/// Never takes the lifecycle lock: parked threads may hold it mid-transition.
pub(crate) fn running_threads() -> impl Iterator<Item = (ThreadId, usize, pthread_t)> {
    TABLE.running_threads()
}

pub(crate) fn slot(index: usize) -> &'static ThreadSlot {
    &TABLE.slots[index]
}

pub(crate) fn freeze_lifecycle() {
    SUSPEND_ACTIVE.store(true, Ordering::SeqCst);
}

pub(crate) fn unfreeze_lifecycle() {
    SUSPEND_ACTIVE.store(false, Ordering::SeqCst);
}

fn lifecycle<R>(f: impl FnOnce(&ThreadTable) -> R) -> R {
    let _guard = TABLE.lock();
    // Check under the lock: an operation blocked before the freeze must abort
    // after acquiring it, not slip into the frozen table.
    if SUSPEND_ACTIVE.load(Ordering::SeqCst) {
        abort!("pthread lifecycle changed during snapshot");
    }
    f(&TABLE)
}

pub(crate) fn init_main() {
    lifecycle(|table| {
        let id = table.reserve(false).expect("main thread registry slot");
        // SAFETY: pthread_self and gettid read the calling thread's identity.
        let pthread = unsafe { libc::pthread_self() };
        // SAFETY: SYS_gettid has no pointer arguments or other preconditions.
        let tid = unsafe { libc::syscall(libc::SYS_gettid) as pid_t };
        table.start(id, pthread, tid);
        CURRENT_ID.with(|current| current.set(Some(id)));
    });
    READY.store(true, Ordering::Release);
}

pub(crate) fn require_ready() {
    if !READY.load(Ordering::Acquire) {
        abort!("pthread hook called before thread registry initialization");
    }
}

pub(crate) fn reserve(detached: bool) -> Option<ThreadId> {
    lifecycle(|table| table.reserve(detached))
}

pub(crate) fn mark_cancellation() {
    CANCELLATION_SEEN.store(true, Ordering::SeqCst);
    crate::warn!(
        Hooks,
        "pthread_cancel succeeded; checkpoint and restore disabled"
    );
}

pub(crate) fn require_no_cancellation() {
    if CANCELLATION_SEEN.load(Ordering::SeqCst) {
        abort!("cannot checkpoint or restore after untracked thread cancellation");
    }
}

pub(crate) fn attach(id: ThreadId, pthread: pthread_t) {
    lifecycle(|table| table.attach(id, pthread));
    crate::debug!(Hooks, "pthread_create id={}", id.0);
}

pub(crate) fn start(id: ThreadId) {
    CURRENT_ID.with(|current| current.set(Some(id)));
    lifecycle(|table| {
        let slot = table.slot_of(id).expect("starting untracked thread");
        // Prepare suspension before the table exposes this thread as running.
        crate::checkpoint::threads::prepare_thread_for_suspend(slot);
        // SAFETY: pthread_self reads the calling thread's identity.
        let pthread = unsafe { libc::pthread_self() };
        // SAFETY: SYS_gettid has no pointer arguments or other preconditions.
        let tid = unsafe { libc::syscall(libc::SYS_gettid) as pid_t };
        table.start(id, pthread, tid);
    });
    crate::debug!(Hooks, "pthread start id={}", id.0);
}

pub(crate) fn exit_current() {
    let id = CURRENT_ID
        .with(|current| current.take())
        .expect("exiting untracked thread");
    lifecycle(|table| table.exit(id));
    crate::debug!(Hooks, "pthread exit id={}", id.0);
}

pub(crate) fn creation_failed(id: ThreadId) {
    lifecycle(|table| table.creation_failed(id));
    crate::debug!(Hooks, "pthread_create failed id={}", id.0);
}

pub(crate) fn joined(pthread: pthread_t) {
    lifecycle(|table| table.join(pthread));
    crate::debug!(Hooks, "pthread_join pthread={pthread}");
}

pub(crate) fn detached(pthread: pthread_t) {
    lifecycle(|table| table.detach(pthread));
    crate::debug!(Hooks, "pthread_detach pthread={pthread}");
}

#[cfg(test)]
impl ThreadTable {
    fn record(&self, id: ThreadId) -> Option<(Phase, Option<pthread_t>, Option<pid_t>)> {
        let slot = &self.slots[self.slot_of(id)?];
        let pthread = slot.pthread.load(Ordering::Relaxed);
        let tid = slot.tid.load(Ordering::Relaxed);
        Some((
            slot.phase(),
            (pthread != 0).then_some(pthread as pthread_t),
            (tid != 0).then_some(tid),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_exit_before_parent_returns_and_handle_reuse() {
        let table = ThreadTable::new();
        let first = table.reserve(false).unwrap();
        table.start(first, 11, 101);
        table.exit(first);
        table.attach(first, 11);
        let (exit_phase, pthread, _) = table.record(first).unwrap();
        assert_eq!(exit_phase, Phase::Exited);
        assert_eq!(pthread, Some(11));
        table.join(11);

        let second = table.reserve(false).unwrap();
        assert_ne!(first, second);
        table.start(second, 11, 102);
        assert_eq!(table.record(second).unwrap().2, Some(102));
        table.exit(second);
        table.join(11);
        assert!(table.record(second).is_none());
    }

    #[test]
    fn detached_child_exit_before_parent_returns() {
        let table = ThreadTable::new();
        let id = table.reserve(true).unwrap();
        table.start(id, 12, 103);
        table.exit(id);
        table.attach(id, 12);
        assert!(table.record(id).is_none());
    }
}
