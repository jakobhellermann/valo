use std::mem::MaybeUninit;

use libc::{SA_SIGINFO, SIG_UNBLOCK, siginfo_t, sigset_t, ucontext_t};

use crate::Result;

pub fn pthread_unblock_signal(signal: i32) {
    let mut mask = MaybeUninit::<sigset_t>::uninit();
    unsafe { libc::sigemptyset(mask.as_mut_ptr()) };
    let mut mask = unsafe { mask.assume_init() };
    unsafe { libc::sigaddset(&raw mut mask, signal) };

    unsafe { libc::pthread_sigmask(SIG_UNBLOCK, &mask, std::ptr::null_mut()) };
}

pub fn register_onstack_signal_handler(
    signal: i32,
    handler: unsafe extern "C" fn(std::ffi::c_int, *const siginfo_t, *const ucontext_t),
) -> Result<()> {
    let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
    unsafe { libc::sigfillset(&raw mut sa.sa_mask) };
    sa.sa_flags = libc::SA_ONSTACK | SA_SIGINFO;
    sa.sa_sigaction = handler as usize;
    let ret = unsafe { libc::sigaction(signal, &sa, std::ptr::null_mut()) };
    if ret != 0 {
        return Err(std::io::Error::from_raw_os_error(ret));
    }

    Ok(())
}
