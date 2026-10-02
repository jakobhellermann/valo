// TODO(ai-review): generated, not yet audited
//! Pthread lifecycle bookkeeping. Suspension will consume this registry later.
//!
//! Records live in `ReservedMemory`, outside checkpointed mappings. The registry
//! never allocates, and no pthread operation runs while its mutex is held.
//! Unlike libTAS's heap-allocated, pointer-linked `ThreadInfo` list, this
//! registry uses monotonic IDs and fixed slots. A reused `pthread_t` therefore
//! cannot name an older incarnation, and restore cannot roll back the registry
//! while live threads still refer to it. libTAS also stores suspension context
//! in `ThreadInfo`; that belongs to a later, separately verified layer here.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};

use libc::{pid_t, pthread_t};

use crate::abort;
use crate::global_state::GlobalState;
use crate::utils::FixedSlots;

const MAX_THREADS: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ThreadId(u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Starting,
    Running,
    Exited,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ThreadRecord {
    id: ThreadId,
    pthread: Option<pthread_t>,
    tid: Option<pid_t>,
    phase: Phase,
    detached: bool,
}

pub(crate) struct ThreadRegistry {
    next_id: u64,
    records: FixedSlots<ThreadRecord, MAX_THREADS>,
    cancellation_seen: bool,
}

impl ThreadRegistry {
    pub(crate) const fn new() -> Self {
        Self {
            next_id: 1,
            records: FixedSlots::new(),
            cancellation_seen: false,
        }
    }

    fn reserve(&mut self, detached: bool) -> Option<ThreadId> {
        let id = ThreadId(self.next_id);
        if self
            .records
            .insert(ThreadRecord {
                id,
                pthread: None,
                tid: None,
                phase: Phase::Starting,
                detached,
            })
            .is_err()
        {
            return None;
        }
        self.next_id += 1;
        Some(id)
    }

    fn record_mut(&mut self, id: ThreadId) -> Option<&mut ThreadRecord> {
        self.records.find_mut(|record| record.id == id)
    }

    fn attach(&mut self, id: ThreadId, pthread: pthread_t) {
        if let Some(record) = self.record_mut(id) {
            record.pthread = Some(pthread);
        }
        // A detached child may have exited and retired before the parent
        // returns from pthread_create. That missing record is expected.
    }

    fn start(&mut self, id: ThreadId, pthread: pthread_t, tid: pid_t) {
        let record = self.record_mut(id).expect("starting untracked thread");
        record.pthread = Some(pthread);
        record.tid = Some(tid);
        record.phase = Phase::Running;
    }

    fn exit(&mut self, id: ThreadId) {
        if let Some(record) = self.record_mut(id) {
            if record.detached {
                self.records.remove(|record| record.id == id);
            } else {
                record.phase = Phase::Exited;
            }
        } else {
            panic!("exiting untracked thread");
        }
    }

    fn creation_failed(&mut self, id: ThreadId) {
        assert!(
            self.records.remove(|record| record.id == id).is_some(),
            "failed create had no registry entry"
        );
    }

    fn join(&mut self, pthread: pthread_t) -> ThreadId {
        self.records
            .remove(|record| record.pthread == Some(pthread))
            .expect("joined untracked thread")
            .id
    }

    fn detach(&mut self, pthread: pthread_t) -> ThreadId {
        if let Some(record) = self
            .records
            .find_mut(|record| record.pthread == Some(pthread))
        {
            let id = record.id;
            if record.phase == Phase::Exited {
                self.records
                    .remove(|record| record.pthread == Some(pthread));
            } else {
                record.detached = true;
            }
            id
        } else {
            panic!("detached untracked thread");
        }
    }
}

thread_local! {
    static CURRENT_ID: Cell<Option<ThreadId>> = const { Cell::new(None) };
}

static READY: AtomicBool = AtomicBool::new(false);

fn with_registry<R>(f: impl FnOnce(&mut ThreadRegistry) -> R) -> R {
    // SAFETY: GlobalState::new initialized this mutex in restore-excluded memory.
    let registry = unsafe { &(*GlobalState::get().reserved).thread_registry };
    let mut guard = registry.lock().unwrap();
    f(&mut guard)
}

pub(crate) fn init_main() {
    // SAFETY: pthread_self and gettid read the calling thread's identity.
    let pthread = unsafe { libc::pthread_self() };
    // SAFETY: SYS_gettid has no pointer arguments or other preconditions.
    let tid = unsafe { libc::syscall(libc::SYS_gettid) as pid_t };
    let id = with_registry(|registry| {
        let id = registry.reserve(false).expect("main thread registry slot");
        registry.start(id, pthread, tid);
        id
    });
    CURRENT_ID.with(|current| current.set(Some(id)));
    READY.store(true, Ordering::Release);
}

pub(crate) fn require_ready() {
    if !READY.load(Ordering::Acquire) {
        abort!("pthread hook called before thread registry initialization");
    }
}

pub(crate) fn reserve(detached: bool) -> Option<ThreadId> {
    with_registry(|registry| registry.reserve(detached))
}

pub(crate) fn mark_cancellation() {
    with_registry(|registry| registry.cancellation_seen = true);
    crate::warn!(
        Hooks,
        "pthread_cancel succeeded; checkpoint and restore disabled"
    );
}

pub(crate) fn require_no_cancellation() {
    if with_registry(|registry| registry.cancellation_seen) {
        abort!("cannot checkpoint or restore after untracked thread cancellation");
    }
}

pub(crate) fn attach(id: ThreadId, pthread: pthread_t) {
    with_registry(|registry| registry.attach(id, pthread));
    crate::debug!(Hooks, "pthread_create id={}", id.0);
}

pub(crate) fn start(id: ThreadId) {
    CURRENT_ID.with(|current| current.set(Some(id)));
    // SAFETY: pthread_self reads the calling thread's identity.
    let pthread = unsafe { libc::pthread_self() };
    // SAFETY: SYS_gettid has no pointer arguments or other preconditions.
    let tid = unsafe { libc::syscall(libc::SYS_gettid) as pid_t };
    with_registry(|registry| registry.start(id, pthread, tid));
    crate::debug!(Hooks, "pthread start id={} tid={tid}", id.0);
}

pub(crate) fn exit_current() {
    let id = CURRENT_ID
        .with(|current| current.take())
        .expect("exiting untracked thread");
    with_registry(|registry| registry.exit(id));
    crate::debug!(Hooks, "pthread exit id={}", id.0);
}

pub(crate) fn creation_failed(id: ThreadId) {
    with_registry(|registry| registry.creation_failed(id));
    crate::debug!(Hooks, "pthread_create failed id={}", id.0);
}

pub(crate) fn joined(pthread: pthread_t) {
    let id = with_registry(|registry| registry.join(pthread));
    crate::debug!(Hooks, "pthread_join id={}", id.0);
}

pub(crate) fn detached(pthread: pthread_t) {
    let id = with_registry(|registry| registry.detach(pthread));
    crate::debug!(Hooks, "pthread_detach id={}", id.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_exit_before_parent_returns_and_handle_reuse() {
        let mut registry = ThreadRegistry::new();
        let first = registry.reserve(false).unwrap();
        registry.start(first, 11, 101);
        registry.exit(first);
        registry.attach(first, 11);
        assert!(registry.record_mut(first).is_some());
        registry.join(11);

        let second = registry.reserve(false).unwrap();
        assert_ne!(first, second);
        registry.start(second, 11, 102);
        assert_eq!(registry.record_mut(second).unwrap().tid, Some(102));
        registry.exit(second);
        registry.join(11);
        assert!(registry.record_mut(second).is_none());
    }

    #[test]
    fn detached_child_exit_before_parent_returns() {
        let mut registry = ThreadRegistry::new();
        let id = registry.reserve(true).unwrap();
        registry.start(id, 12, 103);
        registry.exit(id);
        registry.attach(id, 12);
        assert!(registry.record_mut(id).is_none());
    }
}
