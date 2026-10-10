use std::mem::MaybeUninit;

use libc::{SIG_UNBLOCK, siginfo_t, sigset_t, ucontext_t};

use crate::Result;
use crate::hooks::run_native;

pub fn pthread_unblock_signal(signal: i32) {
    let mut mask = MaybeUninit::<sigset_t>::uninit();
    // SAFETY: no precondition
    unsafe { libc::sigemptyset(mask.as_mut_ptr()) };
    // SAFETY: initialized by sigemptyset
    let mut mask = unsafe { mask.assume_init() };
    // SAFETY: no precondition
    unsafe { libc::sigaddset(&raw mut mask, signal) };
    run_native(|| {
        // SAFETY: no precondition
        unsafe { libc::pthread_sigmask(SIG_UNBLOCK, &mask, std::ptr::null_mut()) };
    });
}

/// The glibc handle and the kernel tid of the calling thread.
pub fn current_thread_ids() -> (libc::pthread_t, libc::pid_t) {
    // SAFETY: pthread_self reads the calling thread's identity and has no
    // preconditions; gettid is a direct syscall without failure modes.
    unsafe { (libc::pthread_self(), rustix::thread::gettid().as_raw_pid()) }
}

/// Register a signal handler. The handler runs with every signal blocked
/// except SIGSEGV, so the diagnostic fault handler stays deliverable; pass
/// `keep_sigsegv = false` to block SIGSEGV as well.
pub fn register_signal_handler(
    signal: i32,
    handler: unsafe extern "C" fn(std::ffi::c_int, *const siginfo_t, *const ucontext_t),
    flags: libc::c_int,
    keep_sigsegv: bool,
) -> Result<()> {
    let mut action = MaybeUninit::<libc::sigaction>::zeroed();
    // SAFETY: sigfillset only writes the mask of the zeroed sigaction.
    unsafe { libc::sigfillset(&raw mut (*action.as_mut_ptr()).sa_mask) };
    // SAFETY: zeroed sigaction is fully initialized; the fields set below.
    let mut action = unsafe { action.assume_init() };
    if keep_sigsegv {
        // SAFETY: no precondition
        unsafe { libc::sigdelset(&raw mut action.sa_mask, libc::SIGSEGV) };
    }
    action.sa_flags = flags;
    action.sa_sigaction = handler as usize;
    let ret = run_native(|| unsafe {
        // SAFETY: action is fully initialized; oldact is not requested.
        libc::sigaction(signal, &action, std::ptr::null_mut())
    });
    if ret != 0 {
        // sigaction returns -1 and reports the reason via errno.
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Register a handler that runs on the alternate stack.
pub fn register_onstack_signal_handler(
    signal: i32,
    handler: unsafe extern "C" fn(std::ffi::c_int, *const siginfo_t, *const ucontext_t),
) -> Result<()> {
    register_signal_handler(signal, handler, libc::SA_ONSTACK | libc::SA_SIGINFO, true)
}

/// uapi `struct clone_args` (linux/sched.h).
#[repr(C)]
pub(crate) struct CloneArgs {
    pub flags: u64,
    pub pidfd: u64,
    pub child_tid: u64,
    pub parent_tid: u64,
    pub exit_signal: u64,
    pub stack: u64,
    pub stack_size: u64,
    pub tls: u64,
    pub set_tid: u64,
    pub set_tid_size: u64,
    pub cgroup: u64,
}

/// Runs clone3(2). The parent receives the child's tid (or the negated
/// errno); the child pops its trampoline from the top of the stack given in
/// `args` and never returns into this frame.
///
/// # Safety
/// - `args.stack..args.stack + args.stack_size` must be mapped, private and
///   writable, with the trampoline function pointer at `stack + stack_size`
///   and its argument at `stack + stack_size + 8` (the child's rsp starts at
///   `stack + stack_size` and pops upward).
/// - The trampoline must never return: it resumes a saved context or exits.
pub(crate) unsafe fn clone3_child(args: &CloneArgs) -> i64 {
    let ret: i64;
    // SAFETY: the kernel validates the syscall arguments (returning the
    // child tid or a negated errno); the child diverges after the syscall
    // and never rejoins this frame, so the parent's stack state is
    // untouched.
    unsafe {
        std::arch::asm!(
        "syscall",
        "test rax, rax",
        "jnz 3f",
        "xor rbp, rbp",
        "pop rax",
        "pop rdi",
        "call rax",
        "ud2",
        "3:",
        inlateout("rax") libc::SYS_clone3 => ret,
        in("rdi") args,
        in("rsi") std::mem::size_of::<CloneArgs>(),
        lateout("rcx") _,
        lateout("r11") _,
            options(nostack)
        )
    }
    ret
}
