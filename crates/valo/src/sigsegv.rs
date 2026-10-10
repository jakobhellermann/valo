//! SIGSEGV diagnostic handler
//! Logs the original signal and restores the default SIGSEGV action.

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
    // Block SIGSEGV in the handler too: after logging we raise it and want
    // the pending delivery to hit the reset default action on return.
    crate::sys::register_signal_handler(
        libc::SIGSEGV,
        sigsegv_handler,
        SA_ONSTACK | SA_SIGINFO | SA_RESETHAND,
        false,
    )
}
