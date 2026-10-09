use crate::global_state::GlobalState;
use crate::hooks::run_native;
use crate::token::{Exclusive, ExclusiveCell};

use std::mem::MaybeUninit;

use libc::{sigaltstack, stack_t};

type Result<T, E = std::io::Error> = std::result::Result<T, E>;

static OSS: ExclusiveCell<stack_t> = ExclusiveCell::new(stack_t {
    ss_sp: std::ptr::null_mut(),
    ss_flags: 0,
    ss_size: 0,
});

pub const ALT_STACK_SIZE: usize = 5 * 1024 * 1024;

fn sigaltstack_get() -> Result<stack_t> {
    let mut oss = MaybeUninit::uninit();
    // SAFETY: has no preconditions
    let ret = run_native(|| unsafe { sigaltstack(std::ptr::null(), oss.as_mut_ptr()) });
    if ret != 0 {
        // sigaltstack returns -1 and reports the reason via errno.
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: initialized by sigaltstack
    let oss = unsafe { oss.assume_init() };
    Ok(oss)
}

pub fn save_stack(excl: &mut Exclusive) -> Result<()> {
    let prev_altstack = sigaltstack_get()?;
    *OSS.get(excl) = prev_altstack;
    Ok(())
}

pub fn prepare_altstack() -> Result<(), std::io::Error> {
    let global_state = GlobalState::get();

    // SAFETY: correct api usage.
    // The kernel is the only writer of the alt stack memory.
    unsafe {
        let alt_stack = global_state.reserved.alt_stack.get().cast::<u8>();
        let x = stack_t {
            ss_sp: alt_stack.cast(),
            ss_flags: 0,
            ss_size: ALT_STACK_SIZE,
        };
        let ret = run_native(|| sigaltstack(&raw const x, std::ptr::null_mut()));
        if ret != 0 {
            return Err(std::io::Error::last_os_error());
        }

        Ok(())
    }
}

pub fn restore_stack(excl: &mut Exclusive) -> Result<(), std::io::Error> {
    let stack = *OSS.get(excl);
    // SAFETY: stack is valid if save_stack ran before, otherwise it is zero and rejected
    let ret = run_native(|| unsafe { sigaltstack(&stack, std::ptr::null_mut()) });
    if ret != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_stack_without_save_stack_is_rejected() {
        let mut excl = Exclusive::enter_exclusive();
        // withotu calling save_stack prior, restore must not be UB and reject the zeroed altstack
        let err = restore_stack(&mut excl).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::ENOMEM));
    }
}
