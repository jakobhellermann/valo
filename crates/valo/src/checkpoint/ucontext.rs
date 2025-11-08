use std::cell::SyncUnsafeCell;
use std::mem::MaybeUninit;

use libc::ucontext_t;

use crate::SendSync;

static SS_UCONTEXT: SyncUnsafeCell<MaybeUninit<SendSync<ucontext_t>>> =
    SyncUnsafeCell::new(MaybeUninit::uninit());
static SS_FPREGSET: SyncUnsafeCell<MaybeUninit<_xstate>> =
    SyncUnsafeCell::new(MaybeUninit::uninit());

pub unsafe fn restore(into: &mut ucontext_t, no_ucontext: *const ucontext_t) {
    unsafe {
        let ss_ucontext = &mut (*SS_UCONTEXT.get()).assume_init_mut().0;
        let ss_fpregset = &mut *SS_FPREGSET.get();

        ss_ucontext.uc_mcontext.fpregs = (*no_ucontext).uc_mcontext.fpregs;
        if ss_ucontext.uc_flags & UC_FP_XSTATE != 0 {
            *ss_ucontext.uc_mcontext.fpregs.cast::<_xstate>() = ss_fpregset.assume_init_read();
        } else {
            *ss_ucontext.uc_mcontext.fpregs = (*ss_fpregset.as_mut_ptr()).fpstate;
        }

        if (*no_ucontext).uc_flags & UC_FP_XSTATE != 0 {
            (*SS_FPREGSET.get()).write(*(*no_ucontext).uc_mcontext.fpregs.cast::<_xstate>());
        }

        *into = *ss_ucontext;
    }
}

pub unsafe fn save(no_ucontext: *const ucontext_t) {
    // TODO: only up to __fpregs_mem
    unsafe {
        let ss_ucontext = SS_UCONTEXT.get().cast::<MaybeUninit<ucontext_t>>();
        let ss_ucontext = (*ss_ucontext).write(*no_ucontext);

        let ss_fpregset = &mut *SS_FPREGSET.get();
        if ss_ucontext.uc_flags & UC_FP_XSTATE != 0 {
            ss_fpregset.write(*ss_ucontext.uc_mcontext.fpregs.cast::<_xstate>());
        } else {
            let fpstate_ptr = &raw mut (*(*SS_FPREGSET.get()).as_mut_ptr()).fpstate;
            *fpstate_ptr = *ss_ucontext.uc_mcontext.fpregs;
        }
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
