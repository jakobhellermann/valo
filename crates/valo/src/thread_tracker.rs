// TODO(ai-review): generated, not yet audited
//! Track pthread lifecycle in a table that checkpoints rewind like any other
//! process memory.

use std::cell::Cell;
use std::sync::atomic::{
    AtomicBool, AtomicI32, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering,
};

use libc::{pid_t, pthread_t};

use crate::global_state::GlobalState;
use crate::{abort, info};

pub(crate) const MAX_THREADS: usize = 256;

#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ThreadId(u64);

impl ThreadId {
    pub(crate) const UNUSED: Self = Self(0);

    /// The numeric id; stable across restores and never reused.
    pub(crate) fn raw(self) -> u64 {
        self.0
    }
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
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParkState {
    /// Not part of a suspension; the handler ignores foreign signals.
    Running = 0,
    /// suspend_others signaled the thread.
    Signaled = 1,
    /// Parked at the deterministic stack position.
    Parked = 2,
    /// Marked for termination by restore; the handler raw-exits the thread.
    Terminated = 3,
}

impl TryFrom<u8> for ParkState {
    type Error = u8;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Running),
            1 => Ok(Self::Signaled),
            2 => Ok(Self::Parked),
            3 => Ok(Self::Terminated),
            other => Err(other),
        }
    }
}

impl TryFrom<u8> for Phase {
    type Error = u8;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Free),
            1 => Ok(Self::Starting),
            2 => Ok(Self::Running),
            3 => Ok(Self::Exited),
            other => Err(other),
        }
    }
}

pub(crate) struct ThreadSlot {
    phase: AtomicU8, // Phase
    id: AtomicU64,
    /// glibc thread descriptor pointer (pthread_t == struct pthread base).
    pub(crate) pthread: AtomicUsize,
    pub(crate) tid: AtomicI32,
    detached: AtomicBool,
    /// Stores `ParkState`: the suspend gate; the handler only parks threads
    /// signaled by suspend_others. `ParkState::Running == 0` matches the
    /// zero-initialized table.
    park: AtomicU8,
    pub(crate) park_address: AtomicUsize,
    pub(crate) stack_low: AtomicUsize,
    /// Kernel exit futex for this pthread, captured before checkpoint.
    pub(crate) clear_child_tid: AtomicUsize,
    /// FS base of the thread, captured at its checkpoint suspension
    pub(crate) fs_base: AtomicUsize,
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
            clear_child_tid: AtomicUsize::new(0),
            fs_base: AtomicUsize::new(0),
        }
    }

    /// Current park state of the slot.
    pub(crate) fn park_state(&self) -> ParkState {
        match ParkState::try_from(self.park.load(Ordering::SeqCst)) {
            Ok(state) => state,
            Err(_) => {
                abort!("thread slot park state corrupted");
            }
        }
    }

    /// The occupant's stable logical id; constant while the slot is
    /// occupied and never reused after the slot is cleared.
    pub(crate) fn thread_id(&self) -> ThreadId {
        ThreadId(self.id.load(Ordering::Relaxed))
    }

    /// CAS transition of the park state; argument order follows libTAS
    /// updateState: new state first, expected state second.
    pub(crate) fn update_state(&self, to: ParkState, from: ParkState) -> bool {
        self.park
            .compare_exchange(from as u8, to as u8, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    fn phase(&self) -> Phase {
        match Phase::try_from(self.phase.load(Ordering::Acquire)) {
            Ok(phase) => phase,
            Err(_) => {
                abort!("thread slot phase corrupted");
            }
        }
    }

    /// Retire a slot whose thread died without running the exit hooks.
    pub(crate) fn retire(&self) {
        self.phase.store(Phase::Exited as u8, Ordering::Release);
    }

    fn clear(&self) {
        self.pthread.store(0, Ordering::Relaxed);
        self.tid.store(0, Ordering::Relaxed);
        self.clear_child_tid.store(0, Ordering::Relaxed);
        self.id.store(0, Ordering::Relaxed);
        self.phase.store(Phase::Free as u8, Ordering::Release);
    }
}

struct ThreadTable {
    slots: [ThreadSlot; MAX_THREADS],
    lifecycle_lock: AtomicBool,
}

impl ThreadTable {
    const fn new() -> Self {
        Self {
            slots: [const { ThreadSlot::new() }; MAX_THREADS],
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
            // SAFETY: reserved is initialized before the pthread hook can
            // spawn tracked threads.
            let id = ThreadId(
                GlobalState::reserved_shared()
                    .next_thread_id
                    .fetch_add(1, Ordering::Relaxed),
            );
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

// Initial thread, then the one that performed the last frame boundary
static MAIN_ID: AtomicU64 = AtomicU64::new(0);
static MAIN_TID: AtomicU32 = AtomicU32::new(0);

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
        let (pthread, tid) = crate::sys::current_thread_ids();
        table.start(id, pthread, tid);
        CURRENT_ID.with(|current| current.set(Some(id)));
        MAIN_ID.store(id.0, Ordering::Relaxed);
        MAIN_TID.store(tid as u32, Ordering::Relaxed);
    });
    READY.store(true, Ordering::Release);
}

/// Makes the calling thread the main thread
pub(crate) fn is_main() -> bool {
    CURRENT_ID
        .with(Cell::get)
        .is_some_and(|id| id.0 == MAIN_ID.load(Ordering::Relaxed))
}

/// Makes the calling thread the main thread
pub(crate) fn claim_main() {
    let Some(id) = CURRENT_ID.with(Cell::get) else {
        abort!("frame boundary on a thread the registry does not track");
    };
    let (_, tid) = crate::sys::current_thread_ids();
    let previous_tid = MAIN_TID.swap(tid as u32, Ordering::Relaxed);
    if MAIN_ID.swap(id.0, Ordering::Relaxed) != id.0 {
        info!(
            Frame,
            "main thread switches from tid {previous_tid} to {tid}"
        );
    }
}

pub(crate) fn main_thread_tid() -> u32 {
    MAIN_TID.load(Ordering::Relaxed)
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
        let (pthread, tid) = crate::sys::current_thread_ids();
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
