use std::mem::MaybeUninit;

use libc::{SA_SIGINFO, SIG_UNBLOCK, siginfo_t, sigset_t, ucontext_t};

use crate::Result;

pub fn pthread_unblock_signal(signal: i32) {
    let mut mask = MaybeUninit::<sigset_t>::uninit();
    // SAFETY: no precondition
    unsafe { libc::sigemptyset(mask.as_mut_ptr()) };
    // SAFETY: initialized by sigemptyset
    let mut mask = unsafe { mask.assume_init() };
    // SAFETY: no precondition
    unsafe { libc::sigaddset(&raw mut mask, signal) };
    // SAFETY: no precondition
    unsafe { libc::pthread_sigmask(SIG_UNBLOCK, &mask, std::ptr::null_mut()) };
}

pub fn register_onstack_signal_handler(
    signal: i32,
    handler: unsafe extern "C" fn(std::ffi::c_int, *const siginfo_t, *const ucontext_t),
) -> Result<()> {
    let mut sa = MaybeUninit::<libc::sigaction>::zeroed();
    // SAFETY: no preconditions
    unsafe { libc::sigfillset(&raw mut (*sa.as_mut_ptr()).sa_mask) };
    // SAFETY: initialized
    let mut sa = unsafe { sa.assume_init() };
    // SAFETY: no preconditions
    unsafe { libc::sigfillset(&raw mut sa.sa_mask) };
    // Keep SIGSEGV deliverable nested so the diagnostic handler can run
    // SAFETY: no preconditions
    unsafe { libc::sigdelset(&raw mut sa.sa_mask, libc::SIGSEGV) };
    sa.sa_flags = libc::SA_ONSTACK | SA_SIGINFO;
    sa.sa_sigaction = handler as usize;
    // SAFETY: correct api usage
    let ret = unsafe { libc::sigaction(signal, &sa, std::ptr::null_mut()) };
    if ret != 0 {
        return Err(std::io::Error::from_raw_os_error(ret));
    }

    Ok(())
}
