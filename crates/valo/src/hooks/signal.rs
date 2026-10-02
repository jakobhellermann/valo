// Prevent apps using suspend/checkpoint signals.

// TODO: libtas WrapperLock equivalent

use std::ffi::{CStr, c_int};

use libc::{sighandler_t, sigset_t, strsignal};

use crate::checkpoint::SIG_CHECKPOINT;
use crate::hooks::is_native;
use crate::{info, override_sym, trace, warn};

// https://man7.org/linux/man-pages/man3/pthread_sigmask.3.html
override_sym!(pthread_sigmask(
    how: c_int,
    set: *const sigset_t,
    oldset: *mut sigset_t
) -> c_int |real| {
    if is_native() {
        return real(how, set, oldset);
    }

    if set.is_null() {
        return real(how, set, oldset);
    }

    // SAFETY: pthread_sigmask required valid set
    let set = unsafe { *set };

    let stripped = strip_valo_signals(set);
    let ret = real(how, &stripped, oldset);
    if ret == 0 && !oldset.is_null() {
        // TODO: report as if unhooked
    }

    ret
});

// https://man7.org/linux/man-pages/man2/sigprocmask.2.html
override_sym!(sigprocmask(
    how: c_int,
    set: *const sigset_t,
    oldset: *mut sigset_t
) -> c_int |real| {
    if is_native() {
        return real(how, set, oldset);
    }

    if set.is_null() {
        return real(how, set, oldset);
    }

    // SAFETY: pthread_sigmask required valid set
    let set = unsafe { *set };

    let stripped = strip_valo_signals(set);
    let ret = real(how, &stripped, oldset);
    if ret == 0 && !oldset.is_null() {
        // TODO: report as if unhooked
    }

    ret
});

// https://man7.org/linux/man-pages/man2/sigaction.2.html
override_sym!(sigaction(
    sig: c_int,
    act: *const libc::sigaction,
    oldact: *mut libc::sigaction
) -> c_int |real| {
    if is_native() {
        return real(sig, act, oldact);
    }

    if is_valo_signal(sig) {
        if !oldact.is_null() {
            warn!("sigaction oldact bookkeeping not yet implemented");
        }
        if !act.is_null() {
            // remember suspend action
        }
        return 0;
    }

    real(sig, act, oldact)
});

// https://man7.org/linux/man-pages/man2/signal.2.html
override_sym!(signal(sig: c_int, handler: sighandler_t) -> sighandler_t |real| {
    if is_valo_signal(sig) {
        if handler as usize != libc::SIG_DFL {
            // TODO: remember handler
        }
        // TODO: report the previously saved handler or SIG_DFL
        return libc::SIG_DFL;
    }
    real(sig, handler)
});

// https://man7.org/linux/man-pages/man3/pthread_kill.3.html
override_sym!(pthread_kill(thread: libc::pthread_t, sig: c_int) -> c_int |real| {
    trace!("pthread_kill called for thread={thread} with sig={}",  {
        let name = unsafe { CStr::from_ptr(strsignal(sig)) };
        name.to_string_lossy()
    });

    real(thread, sig)
});

// https://man7.org/linux/man-pages/man2/sigsuspend.2.html
override_sym!(sigsuspend(set: *const sigset_t) -> c_int |real| {
    if set.is_null() {
        return real(set);
    }

    // SAFETY: sigsuspend's contract guarantees valid sigset_t
    let set = unsafe { *set };
    let stripped = strip_valo_signals(set);

    real(&raw const stripped)
});

// https://man7.org/linux/man-pages/man3/sigwait.3.html
override_sym!(sigwait(set: *const sigset_t, sig: *mut c_int) -> c_int |real| {
    info!("todo: handle sigwait");
    real(set, sig)
});

// https://man7.org/linux/man-pages/man2/sigaltstack.2.html
override_sym!(sigaltstack(
    ss: *const libc::stack_t,
    old_ss: *mut libc::stack_t
) -> c_int |real| {
    if is_native() {
        return real(ss, old_ss);
    }

    if !ss.is_null() {
        // SAFETY: sigaltstack's contract guarantees `ss` points to an
        // initialized stack_t.
        let stack = unsafe { &*ss };
        info!("Setting altstack to {ss:p} with size {}", stack.ss_size);
    }

    let ret = real(ss, old_ss);

    if !old_ss.is_null() {
        // SAFETY: the real call succeeded and initialized `old_ss`.
        let old_stack = unsafe { &*old_ss };
        warn!("Retrieving altstack to {:p} with size {}", old_stack.ss_sp, old_stack.ss_size);
    }

    ret
});

fn is_valo_signal(sig: c_int) -> bool {
    sig == SIG_CHECKPOINT
}

// Return a copy with suspend/checkpoint signals removed
fn strip_valo_signals(mut mask: sigset_t) -> sigset_t {
    // SAFETY: `mask` is initialized
    unsafe {
        libc::sigdelset(&raw mut mask, SIG_CHECKPOINT);
    }
    mask
}
