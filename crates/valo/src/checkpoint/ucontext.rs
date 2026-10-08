use libc::ucontext_t;

use crate::token::{Exclusive, ExclusiveCell};

const fn zero<T>() -> T {
    // SAFETY: callers pass all-zero-valid C structs (pointers, integers).
    unsafe { std::mem::MaybeUninit::zeroed().assume_init() }
}

static SS_UCONTEXT: ExclusiveCell<ucontext_t> = ExclusiveCell::new(zero());
static SS_FPREGSET: ExclusiveCell<_xstate> = ExclusiveCell::new(zero());

pub fn restore(excl: &mut Exclusive, ucontext: &mut ucontext_t) {
    let saved_xstate = SS_UCONTEXT.get(excl).uc_flags & UC_FP_XSTATE != 0;
    let saved_xstate = saved_xstate.then(|| *SS_FPREGSET.get(excl));
    let saved_fpstate = if saved_xstate.is_none() {
        Some(SS_FPREGSET.get(excl).fpstate)
    } else {
        None
    };

    let mut ctx = *SS_UCONTEXT.get(excl);
    let live_fpregs = ucontext.uc_mcontext.fpregs;
    ctx.uc_mcontext.fpregs = live_fpregs;
    if let Some(xstate) = saved_xstate {
        // SAFETY: fpregs points into the interrupted context's fpregs_mem.
        unsafe { *live_fpregs.cast::<_xstate>() = xstate };
    }
    if let Some(fpstate) = saved_fpstate {
        // SAFETY: fpregs points into the interrupted context's fpregs_mem.
        unsafe { *live_fpregs = fpstate };
    }
    if ucontext.uc_flags & UC_FP_XSTATE != 0 {
        // SAFETY: fpregs points into the interrupted context's fpregs_mem.
        *SS_FPREGSET.get(excl) = unsafe { *live_fpregs.cast::<_xstate>() };
    }
    *ucontext = ctx;
}

pub fn save(excl: &mut Exclusive, ucontext: &ucontext_t) {
    // TODO: only up to __fpregs_mem
    *SS_UCONTEXT.get(excl) = *ucontext;

    if ucontext.uc_flags & UC_FP_XSTATE != 0 {
        // SAFETY: fpregs points into the interrupted context's fpregs_mem.
        *SS_FPREGSET.get(excl) = unsafe { *ucontext.uc_mcontext.fpregs.cast::<_xstate>() };
    } else {
        // SAFETY: fpregs points into the interrupted context's fpregs_mem.
        SS_FPREGSET.get(excl).fpstate = unsafe { *ucontext.uc_mcontext.fpregs };
    }
}

// https://sites.uclouvain.be/SystInfo/usr/include/asm/sigcontext.h.html
#[allow(non_camel_case_types, dead_code)]
#[derive(Clone, Copy)]
struct _xsave_hdr {
    xstate_bv: u64,
    reserved1: [u64; 2],
    reserved2: [u64; 5],
}

#[allow(non_camel_case_types, dead_code)]
#[derive(Clone, Copy)]
struct _ymmh_state {
    /* 16 * 16 bytes for each YMMH-reg */
    ymmh_space: [u32; 64],
}

#[allow(non_camel_case_types, dead_code)]
#[derive(Clone, Copy)]
struct _xstate {
    pub fpstate: libc::_libc_fpstate,
    pub xstate_hdr: _xsave_hdr,
    pub ymmh: _ymmh_state,
}

const UC_FP_XSTATE: u64 = 1;
