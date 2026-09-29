//! SIGSEGV diagnostic handler
//! Logs the original signal and restores the default SIGSEGV action.

use std::mem::MaybeUninit;

use libc::{REG_RIP, SA_ONSTACK, SA_RESETHAND, SA_SIGINFO, siginfo_t, ucontext_t};

use crate::error;

pub unsafe extern "C" fn sigsegv_handler(
    _signum: std::ffi::c_int,
    info: *const siginfo_t,
    ucontext: *const ucontext_t,
) {
    // SAFETY: kernel passes a valid siginfo_t pointer to the handler.
    let fault_addr = unsafe { (*info).si_addr() };
    // SAFETY: kernel passes a valid siginfo_t pointer to the handler.
    let si_code = unsafe { (*info).si_code };
    // SAFETY: kernel passes a valid ucontext_t pointer to the handler.
    let rip = unsafe { (*ucontext).uc_mcontext.gregs[REG_RIP as usize] };

    error!(
        Adhoc,
        "SIGSEGV si_code={} fault_addr={:p} rip={:#x}", si_code, fault_addr, rip
    );

    // SAFETY: raise is async-signal-safe and SIGSEGV is blocked by sa_mask.
    unsafe { libc::raise(libc::SIGSEGV) };
}

pub fn install() -> crate::Result<()> {
    let mut sa = MaybeUninit::<libc::sigaction>::zeroed();
    // SAFETY: no preconditions
    unsafe { libc::sigfillset(&raw mut (*sa.as_mut_ptr()).sa_mask) };
    // SAFETY: initialized
    let mut sa = unsafe { sa.assume_init() };
    sa.sa_flags = SA_ONSTACK | SA_SIGINFO | SA_RESETHAND;
    sa.sa_sigaction = sigsegv_handler as *const () as usize;
    // SAFETY: correct sigaction usage
    let ret = unsafe { libc::sigaction(libc::SIGSEGV, &sa, std::ptr::null_mut()) };
    if ret != 0 {
        return Err(std::io::Error::from_raw_os_error(ret));
    }
    Ok(())
}
