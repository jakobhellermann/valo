//! Park tracked threads while checkpointing and restoring.

use std::cell::{Cell, SyncUnsafeCell};
use std::ffi::c_void;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicU32, Ordering};

use heapless::Vec;
use libc::ucontext_t;

use crate::abort;
use crate::global_state::GlobalState;
use crate::hooks::run_native;
use crate::thread_tracker::{self, MAX_THREADS, ParkState, ThreadId};
use crate::utils::UnsafeSendSync;

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

static THREAD_PARKED: SyncUnsafeCell<MaybeUninit<libc::sem_t>> =
    SyncUnsafeCell::new(MaybeUninit::zeroed());

fn parked_sem() -> *mut libc::sem_t {
    // SAFETY: init_main initialized the semaphore before any thread could
    // park; the storage is process-lifetime static memory.
    unsafe { (*THREAD_PARKED.get()).as_mut_ptr() }
}

static SAVED_CONTEXTS: SyncUnsafeCell<MaybeUninit<UnsafeSendSync<[ucontext_t; MAX_THREADS]>>> =
    SyncUnsafeCell::new(MaybeUninit::uninit());

fn context(slot: usize) -> *mut ucontext_t {
    // SAFETY: slot came from the checked thread table and is below MAX_THREADS.
    unsafe {
        (*SAVED_CONTEXTS.get())
            .as_mut_ptr()
            .cast::<ucontext_t>()
            .add(slot)
    }
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
            if (*slot).park_address.load(Ordering::SeqCst) != address {
                abort!(Hooks, "thread park stack address changed");
            }
        } else {
            (*slot).park_address.store(address, Ordering::SeqCst);
        }
    }

    // SAFETY: init_main initialized this semaphore before any thread could park.
    if unsafe { libc::sem_post(parked_sem()) } != 0 {
        abort!("failed to unlock semaphore on thread park");
    }
    while RESUME_GATE.load(Ordering::SeqCst) == 0 {
        // SAFETY: the gate is an aligned u32 static; FUTEX_WAIT reads it.
        unsafe {
            libc::syscall(
                libc::SYS_futex,
                &raw const RESUME_GATE as *const u32,
                libc::FUTEX_WAIT | libc::FUTEX_PRIVATE_FLAG,
                0u32,
                std::ptr::null::<libc::timespec>(),
            );
        }
    }
    if state.restore_in_progress() {
        // SAFETY: getcontext wrote this slot at checkpoint; the selected snapshot restored it.
        unsafe { libc::setcontext(context(CURRENT_SLOT.with(Cell::get))) };
        abort!("execution resumed after setcontext");
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
    // Ignore foreign signals: only threads signaled by suspend_others park.
    if !target.update_state(ParkState::Parked, ParkState::Signaled) {
        return;
    }
    if !state.restore_in_progress() {
        // TODO: Preserve TLS register state across restore.
        // TODO: Verify FP/vector registers beyond the integer-stack test.
        // SAFETY: this thread owns its checked slot and getcontext writes its context.
        if unsafe { libc::getcontext(context(slot)) } != 0 {
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

    crate::sys::pthread_unblock_signal(SUSPEND_SIGNAL);
}

pub(crate) fn init_main() {
    // SAFETY: parked_sem points to zeroed writable static storage; init_main
    // runs once before any thread could park.
    if unsafe { libc::sem_init(parked_sem(), 0, 0) } != 0 {
        abort!("failed to init semaphore");
    }
    // SAFETY: zero is a valid starting value for sigaction; fields are set below.
    let mut action = unsafe { MaybeUninit::<libc::sigaction>::zeroed().assume_init() };
    action.sa_flags = libc::SA_SIGINFO;
    action.sa_sigaction = suspend_handler as *const () as usize;
    // SAFETY: action.sa_mask is writable; sigfillset initializes it before sigdelset changes it.
    unsafe {
        libc::sigfillset(&raw mut action.sa_mask);
        libc::sigdelset(&raw mut action.sa_mask, libc::SIGSEGV);
    }
    // SAFETY: action has a valid handler, flags, and initialized mask; oldact is null.
    if run_native(|| unsafe { libc::sigaction(SUSPEND_SIGNAL, &action, std::ptr::null_mut()) }) != 0
    {
        abort!("failed to register suspend handler");
    }
}

pub(crate) fn suspend_others(expected: Option<&SavedThreads>) {
    let self_id = thread_tracker::current_id();
    if CURRENT_SLOT.with(Cell::get) != usize::MAX {
        abort!("checkpoint or restore from a non-main thread is unsupported");
    }
    // Freeze lifecycle transitions before the scan reads them.
    thread_tracker::freeze_lifecycle();
    let (live, targets) = {
        let mut live = Vec::<ThreadId, MAX_THREADS>::new();
        let mut targets = Vec::<(usize, libc::pthread_t), MAX_THREADS>::new();
        for (id, slot, pthread) in thread_tracker::running_threads() {
            live.push(id).unwrap();
            if id != self_id {
                targets.push((slot, pthread)).unwrap();
            }
        }
        (live, targets)
    };
    if let Some(saved) = expected {
        let changed = live.len() != saved.count
            || live.iter().any(|id| !saved.ids[..saved.count].contains(id));
        if changed {
            let missing: heapless::Vec<_, MAX_THREADS> = saved.ids[..saved.count]
                .iter()
                .filter(|id| !live.contains(id))
                .collect();
            let extra: heapless::Vec<_, MAX_THREADS> = live
                .iter()
                .filter(|id| !saved.ids[..saved.count].contains(id))
                .collect();
            // TODO: Recreate missing saved threads and retire extra live threads.
            abort!(
                Restore,
                "thread topology changed since checkpoint: missing={missing:?} extra={extra:?}"
            );
        }
    }
    RESUME_GATE.store(0, Ordering::SeqCst);
    for (slot, thread) in &targets {
        let target = thread_tracker::slot(*slot);
        if !target.update_state(ParkState::Signaled, ParkState::Running) {
            abort!(Hooks, "unexpected thread park state before signal");
        }
        // SAFETY: the handle came from a running table entry; lifecycle changes abort while suspension is active.
        if run_native(|| unsafe { libc::pthread_kill(*thread, SUSPEND_SIGNAL) }) != 0 {
            abort!("cannot signal tracked thread");
        }
    }
    // TODO: Diagnose a thread that never acknowledges, without a fixed deadline.
    for _ in 0..targets.len() {
        loop {
            // SAFETY: init_main initialized this semaphore before suspension begins.
            if unsafe { libc::sem_wait(parked_sem()) } == 0 {
                break;
            }
            if std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                abort!("cannot wait for thread suspension");
            }
        }
    }
    crate::debug!(Hooks, "suspended {} threads", targets.len());
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
    // SAFETY: the gate is an aligned u32 static; FUTEX_WAKE reads its address.
    unsafe {
        libc::syscall(
            libc::SYS_futex,
            &raw const RESUME_GATE as *const u32,
            libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG,
            i32::MAX,
        );
    }
}

pub fn recreate_missing() {
    // TODO
}
