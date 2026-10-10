// TODO(ai-review): generated, not yet audited
//! Park tracked threads while checkpointing and restoring.

mod recreate;

pub(crate) use recreate::recreate_missing;

use std::cell::{Cell, UnsafeCell};
use std::ffi::c_void;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicU32, Ordering};

use libc::ucontext_t;

use rustix::thread::futex;

use crate::global_state::GlobalState;
use crate::hooks::run_native;
use crate::thread_tracker::{self, MAX_THREADS, ParkState, ThreadId};
use crate::token::{Exclusive, Frozen};
use crate::utils::SignalSafeFormat;
use crate::{abort, sys};

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct SavedThreads {
    pub(crate) ids: [ThreadId; MAX_THREADS],
    pub(crate) count: usize,
}

impl SavedThreads {
    pub(crate) const fn new() -> Self {
        Self {
            ids: [ThreadId::UNUSED; MAX_THREADS],
            count: 0,
        }
    }
}

/// Resume gate for parked threads: 0 keeps them waiting, 1 releases them.
/// Zero-initialized so threads park before the first resume.
static RESUME_GATE: AtomicU32 = AtomicU32::new(0);

/// A POSIX semaphore in static storage, initialized by `init_main` before
/// any thread could park.
struct ParkSemaphore(UnsafeCell<MaybeUninit<libc::sem_t>>);

// SAFETY: sem_t operations are internally thread-safe.
unsafe impl Sync for ParkSemaphore {}

impl ParkSemaphore {
    /// Only usable once `init_main` initialized the semaphore.
    pub(crate) fn as_ptr(&self) -> *mut libc::sem_t {
        self.0.get().cast()
    }
}

/// Parked threads post this semaphore right before they block on the resume
/// gate. Waiting for the posts is the barrier that lets the restore touch
/// memory: once a thread has posted, it executes no user code on its stack,
/// which the restore may rewrite or remap underneath it.
static THREAD_PARKED: ParkSemaphore = ParkSemaphore(UnsafeCell::new(MaybeUninit::zeroed()));

fn parked_sem() -> *mut libc::sem_t {
    THREAD_PARKED.as_ptr()
}

/// Per-slot saved thread contexts: each slot is written by the checkpoint
/// thread while all others are parked, and read only by the thread owning
/// the slot.
struct SavedContexts(UnsafeCell<MaybeUninit<[ucontext_t; MAX_THREADS]>>);

// SAFETY: accesses are per-slot disjoint and protocol-ordered.
unsafe impl Sync for SavedContexts {}

static SAVED_CONTEXTS: SavedContexts = SavedContexts(UnsafeCell::new(MaybeUninit::uninit()));

/// The saved-context slot of the thread owning `slot`.
///
/// # Safety
/// - `slot` came from the checked thread table.
pub(crate) unsafe fn saved_context(slot: usize) -> *mut ucontext_t {
    // SAFETY: per the function contract.
    unsafe { SAVED_CONTEXTS.0.get().cast::<ucontext_t>().add(slot) }
}

pub(crate) fn saved_threads() -> SavedThreads {
    // Lock-free: other threads are parked here, and a parked thread may hold
    // the lifecycle lock mid-transition.
    let mut saved = SavedThreads::new();
    for (id, _, _) in thread_tracker::running_threads() {
        saved.ids[saved.count] = id;
        saved.count += 1;
    }
    saved
}

thread_local! {
    static CURRENT_SLOT: Cell<usize> = const { Cell::new(usize::MAX) };
}

// TODO: Detect targets that use or block this signal.
pub(crate) const SUSPEND_SIGNAL: i32 = libc::SIGXFSZ;

// ARCH_GET_FS from uapi asm/prctl.h; libc 0.2 lacks the constant.
const ARCH_GET_FS: i64 = 0x1003;

unsafe extern "C" {
    fn valo_park_on_stack(
        stack_low: usize,
        park: extern "C" fn(*mut c_void),
        arg: *mut c_void,
    ) -> i32;
}

extern "C" fn park_thread(arg: *mut c_void) {
    let slot = arg.cast::<thread_tracker::ThreadSlot>();
    let marker = 0u8;
    let address = &raw const marker as usize;
    let state = GlobalState::get();
    // SAFETY: arg is the stable slot pointer passed to valo_park_on_stack by
    // this thread; only this thread writes its park address.
    unsafe {
        if state.restore_in_progress() {
            let expected = (*slot).park_address.load(Ordering::SeqCst);
            if expected != address {
                abort!(
                    Hooks,
                    "thread park stack address changed: expected {expected:#x}, now {address:#x}"
                );
            }
        } else {
            (*slot).park_address.store(address, Ordering::SeqCst);
        }
    }

    // SAFETY: init_main initialized the semaphore before any thread could park.
    if unsafe { libc::sem_post(parked_sem()) } != 0 {
        abort!("failed to unlock semaphore on thread park");
    }
    wait_for_resume();
    if state.restore_in_progress() {
        // SAFETY: getcontext wrote this slot at checkpoint; the selected
        // snapshot restored it.
        unsafe { libc::setcontext(saved_context(CURRENT_SLOT.with(Cell::get))) };
        abort!("execution resumed after setcontext");
    }
}

/// Block until resume_others opens the gate. Signal-handler-safe: a futex
/// wait on the static gate, no allocation.
pub(super) fn wait_for_resume() {
    while RESUME_GATE.load(Ordering::SeqCst) == 0 {
        // EAGAIN/EINTR mean the gate changed or a signal arrived; the loop
        // re-checks the value.
        let _ = futex::wait(&RESUME_GATE, futex::Flags::PRIVATE, 0, None);
    }
}

unsafe extern "C" fn suspend_handler(
    _signal: i32,
    _info: *const libc::siginfo_t,
    _interrupted: *const ucontext_t,
) {
    let slot = CURRENT_SLOT.with(Cell::get);
    if slot >= MAX_THREADS {
        abort!(Hooks, "suspend signal reached untracked thread");
    }
    let target = thread_tracker::slot(slot);
    let state = GlobalState::get();
    // Termination requested by restore: raw thread exit, no unwinding, no
    // user code. The state stays as tombstone until the slot is retired.
    if target.park_state() == ParkState::Terminated {
        exit_thread_raw();
    }
    // Ignore foreign signals: only threads signaled by suspend_others park.
    if !target.update_state(ParkState::Parked, ParkState::Signaled) {
        return;
    }
    if !state.restore_in_progress() {
        // Capture the TLS base register: getcontext does not save it, and a
        // recreated thread needs it as its CLONE_SETTLS value.
        let mut fs_base = 0usize;
        // SAFETY: correct api usage
        unsafe { libc::syscall(libc::SYS_arch_prctl, ARCH_GET_FS, &raw mut fs_base) };
        target.fs_base.store(fs_base, Ordering::SeqCst);
        // TODO: Verify FP/vector registers beyond the integer-stack test.
        // SAFETY: this thread owns its checked slot and getcontext writes its context.
        if unsafe { libc::getcontext(saved_context(slot)) } != 0 {
            abort!("failed to run getcontext");
        }
        if state.restore_in_progress() {
            return;
        }
    }
    // SAFETY: the slot came from this thread's checked table entry; stack_low was
    // set before it became running. valo_park_on_stack calls park_thread
    // synchronously with the same persistent pointer.
    if unsafe {
        valo_park_on_stack(
            target.stack_low.load(Ordering::SeqCst),
            park_thread,
            std::ptr::from_ref(target).cast_mut().cast(),
        )
    } != 0
    {
        abort!(Hooks, "thread stack has insufficient room for suspension");
    }
}

fn current_thread_stack_low() -> usize {
    let mut attr = MaybeUninit::<libc::pthread_attr_t>::uninit();
    // SAFETY: pthread_self identifies this live thread; attr points to writable storage.
    if unsafe { libc::pthread_getattr_np(libc::pthread_self(), attr.as_mut_ptr()) } != 0 {
        abort!("cannot inspect thread stack");
    }
    // SAFETY: pthread_getattr_np returned success and initialized attr.
    let mut attr = unsafe { attr.assume_init() };
    let mut base = std::ptr::null_mut();
    let mut size = 0;
    // SAFETY: attr came from pthread_getattr_np; base and size are writable locals.
    let result = unsafe { libc::pthread_attr_getstack(&attr, &mut base, &mut size) };
    // SAFETY: attr still holds the attributes returned by pthread_getattr_np.
    unsafe { libc::pthread_attr_destroy(&mut attr) };
    if result != 0 || base.is_null() || size < 48 * 1024 {
        abort!("thread stack bounds unsupported: addr={base:p}, size={size}");
    }
    base as usize
}

// Prepare suspension before the table exposes this thread as running.
pub(crate) fn prepare_thread_for_suspend(slot: usize) {
    CURRENT_SLOT.with(|current| current.set(slot));

    let stack_low = current_thread_stack_low();
    // SAFETY: slot came from this thread's table entry and the table lives for
    // the process lifetime.
    thread_tracker::slot(slot)
        .stack_low
        .store(stack_low, Ordering::SeqCst);

    let clear_child_tid = match rustix::thread::get_clear_child_tid_address() {
        Ok(Some(address)) => address,
        Ok(None) => {
            abort!("thread has no clear_child_tid address");
        }
        Err(_) => {
            abort!("cannot inspect thread clear_child_tid address");
        }
    };
    thread_tracker::slot(slot)
        .clear_child_tid
        .store(clear_child_tid.as_ptr() as usize, Ordering::SeqCst);

    crate::sys::pthread_unblock_signal(SUSPEND_SIGNAL);
}

pub(crate) fn init_main() {
    // Compute the glibc tid offset early, from the main thread's live
    // descriptor, so the restore handler never runs the descriptor scan.
    let _ = crate::glibc::tid_offset();
    // SAFETY: parked_sem points to zeroed writable static storage; init_main
    // runs once before any thread could park.
    if unsafe { libc::sem_init(parked_sem(), 0, 0) } != 0 {
        abort!("failed to init semaphore");
    }
    if let Err(e) =
        sys::register_signal_handler(SUSPEND_SIGNAL, suspend_handler, libc::SA_SIGINFO, true)
    {
        abort!(
            "failed to register suspend handler: {}",
            SignalSafeFormat(&e)
        );
    }
}

pub(crate) fn suspend_others(exclusive: &mut Exclusive, expected: Option<&SavedThreads>) -> Frozen {
    let self_id = thread_tracker::current_id();
    if CURRENT_SLOT.with(Cell::get) != usize::MAX {
        abort!("checkpoint or restore from a non-main thread is unsupported");
    }
    // Freeze lifecycle transitions before the scans read them.
    thread_tracker::freeze_lifecycle();

    // A restore's memory rewind resets a recreation's glibc descriptor tid
    // back to the dead checkpoint value. Restamp from the reserved record
    // before anything addresses a thread through its descriptor: without
    // this, a live recreation that has not been scheduled since the rewind
    // looks dead, gets retired, and would be cloned again.
    for (id, slot, pthread) in thread_tracker::running_threads() {
        let record = &GlobalState::reserved_shared().live_tids[slot];
        if record.thread_id.load(Ordering::SeqCst) != id.raw() {
            continue;
        }
        let tid = record.tid.load(Ordering::SeqCst);
        // SAFETY: pthread is the live descriptor of the running thread that
        // owns this slot, and the record belongs to that occupant; the
        // aligned pid_t store cannot tear, and concurrent readers see a
        // tid naming the same kernel thread either way.
        unsafe { crate::glibc::write_tid(pthread as usize, tid) };
    }

    if let Some(saved) = expected {
        // Terminate live threads the savestate does not know about: they were
        // never saved, so the restored process must not keep them alive.
        for (id, slot, pthread) in thread_tracker::running_threads() {
            if saved.ids[..saved.count].contains(&id) {
                continue;
            }
            let target = thread_tracker::slot(slot);
            let clear_child_tid = target.clear_child_tid.load(Ordering::SeqCst);
            if clear_child_tid == 0 {
                abort!(Restore, "thread slot {slot} has no clear_child_tid address");
            }
            if !target.update_state(ParkState::Terminated, ParkState::Running) {
                abort!(Restore, "unexpected thread park state before termination");
            }
            // SAFETY: pthread_kill addresses the thread through its glibc
            // descriptor. It reports failures as its return value (glibc does
            // not reliably set errno here): ESRCH means the thread died since
            // the scan and its exit futex is already cleared.
            let ret = run_native(|| unsafe { libc::pthread_kill(pthread, SUSPEND_SIGNAL) });
            if ret != 0 && ret != libc::ESRCH {
                abort!(Restore, "cannot signal untracked thread: ret={ret}");
            }
            wait_thread_gone(clear_child_tid);
            // Retire the entry so later scans no longer see the dead thread.
            target.retire();
        }
    }

    RESUME_GATE.store(0, Ordering::SeqCst);
    for (id, slot, pthread) in thread_tracker::running_threads() {
        if id == self_id {
            continue;
        }
        let target = thread_tracker::slot(slot);
        let tid = target.tid.load(Ordering::SeqCst);
        if !target.update_state(ParkState::Signaled, ParkState::Running) {
            abort!(Hooks, "unexpected thread park state before signal");
        }
        // SAFETY: pthread_kill addresses the thread through its glibc
        // descriptor; recreate() stamps the occupying kernel thread's tid
        // into that descriptor (CLONE_PARENT_SETTID), so delivery reaches
        // the original thread and any recreation from an earlier restore
        // alike, whatever tid it runs on now.
        let delivered = run_native(|| unsafe { libc::pthread_kill(pthread, SUSPEND_SIGNAL) }) == 0;
        crate::debug!(Restore, "signal pass tid={tid} delivered={delivered}");
    }
    // Suspend rescan: spin until every signaled thread is parked or provably
    // dead, retiring the dead ones. The parked count is taken here - after
    // the dead are gone - so it only ever contains threads that still owe a
    // post; a dead thread would otherwise leave the barrier waiting forever
    // or end it early.
    let parked_count = loop {
        let mut need_rescan = false;
        let mut count = 0usize;
        for (id, slot, pthread) in thread_tracker::running_threads() {
            if id == self_id {
                continue;
            }
            let target = thread_tracker::slot(slot);
            match target.park_state() {
                ParkState::Parked => count += 1,
                ParkState::Signaled => {
                    // SAFETY: signal 0 only probes liveness, through the same
                    // descriptor-based delivery as the suspend signal.
                    let ret = run_native(|| unsafe { libc::pthread_kill(pthread, 0) });
                    if ret == 0 {
                        need_rescan = true;
                    } else if ret == libc::ESRCH {
                        // Died between the signal and its park: it will
                        // never post. Retire it so neither this rescan nor
                        // recreate_missing counts it as alive.
                        target.retire();
                    } else {
                        abort!(Hooks, "cannot probe signaled thread: ret={ret}");
                    }
                }
                ParkState::Running | ParkState::Terminated => {
                    // Unreachable: the signal pass above claimed every
                    // Running slot, and the terminate pass retired its
                    // targets before this loop.
                    abort!(Hooks, "unexpected park state in the rescan");
                }
            }
        }
        if !need_rescan {
            break count;
        }
        // A brief pause between rescans so probing cannot spin.
        let _ = rustix::thread::nanosleep(&futex::Timespec {
            tv_sec: 0,
            tv_nsec: 10_000,
        });
    };
    // Park barrier: every counted thread posts right before it blocks on
    // the resume gate. Residual window: a thread that dies between its
    // Parked transition and its post hangs this wait - nothing retires a
    // Parked slot.
    for _ in 0..parked_count {
        loop {
            // SAFETY: init_main initialized the semaphore before suspension
            // begins.
            if unsafe { libc::sem_wait(parked_sem()) } == 0 {
                break;
            }
            if std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                abort!("cannot wait for thread suspension");
            }
        }
    }
    crate::debug!(Hooks, "suspended {parked_count} threads");
    // The rewind resets the table's park states, so this list is the only
    // record of who parked; recreate_missing decides against it.
    if GlobalState::get().restore_in_progress() {
        record_parked_threads(exclusive);
    }

    // SAFETY: we just froze all other threads
    unsafe { Frozen::assume() }
}

/// Record the slots whose threads parked in this suspension pass, after the
/// park barrier confirmed every park. Plain fields: the writer (before the
/// raise that enters the restore handler) and the reader (inside it) are the
/// same thread, so program order orders the accesses.
fn record_parked_threads(exclusive: &mut Exclusive) {
    // SAFETY: reserved memory was mapped and zeroed during init.
    let parked = GlobalState::parked_threads(exclusive);
    let mut count = 0usize;
    for (_, slot, _) in thread_tracker::running_threads() {
        if thread_tracker::slot(slot).park_state() != ParkState::Parked {
            continue;
        }
        parked.slots[count] = slot as u32;
        count += 1;
    }
    parked.count = count;
}

/// Wait until the terminated thread exits: the kernel clears its exit futex
/// (the address PR_GET_TID_ADDRESS reported, a distinct member of struct
/// pthread next to the tid field) and wakes the address at exit, so zero
/// proves the kernel thread is gone - even for a raw SYS_exit without any
/// glibc teardown.
fn wait_thread_gone(clear_child_tid: usize) {
    let exit_futex = clear_child_tid as *const libc::pid_t;
    for _ in 0..1000 {
        // SAFETY: the futex lives in the target's thread descriptor, whose
        // stack mapping nothing rewrites until the memory restore - this
        // runs before it - and the kernel writes the full word at exit.
        if unsafe { exit_futex.read_volatile() } == 0 {
            return;
        }
        let _ = rustix::thread::nanosleep(&futex::Timespec {
            tv_sec: 0,
            tv_nsec: 1_000_000,
        });
    }
    abort!(Restore, "extra thread did not terminate");
}

/// Terminate the calling kernel thread without unwinding or running user code.
fn exit_thread_raw() -> ! {
    // SAFETY: SYS_exit terminates the calling kernel thread and never returns.
    unsafe { libc::syscall(libc::SYS_exit, 0) };
    unreachable!("raw thread exit returned");
}

pub(crate) fn resume_others() {
    // Reset all suspend gates before waking anyone: a thread waking from
    // setcontext must find its gate back at RUNNING so a later checkpoint
    // can signal it again. Slots never signaled stay RUNNING (CAS fails
    // harmlessly).
    for slot in 0..MAX_THREADS {
        // SAFETY: slot is inside the fixed table.
        let _ = thread_tracker::slot(slot).update_state(ParkState::Running, ParkState::Parked);
    }
    thread_tracker::unfreeze_lifecycle();
    RESUME_GATE.store(1, Ordering::SeqCst);
    let _ = futex::wake(&RESUME_GATE, futex::Flags::PRIVATE, i32::MAX as u32);
}
