use crate::global_state::GlobalState;
use crate::hooks::run_native;
use crate::utils::UnsafeSendSync;

use std::cell::SyncUnsafeCell;
use std::mem::MaybeUninit;

use libc::{sigaltstack, stack_t};

type Result<T, E = std::io::Error> = std::result::Result<T, E>;

static OSS: SyncUnsafeCell<MaybeUninit<UnsafeSendSync<stack_t>>> =
    SyncUnsafeCell::new(MaybeUninit::uninit());

pub const ALT_STACK_SIZE: usize = 5 * 1024 * 1024;

fn sigaltstack_get() -> Result<stack_t, i32> {
    let mut oss = MaybeUninit::uninit();
    // SAFETY: has no preconditions
    let ret = run_native(|| unsafe { sigaltstack(std::ptr::null(), oss.as_mut_ptr()) });
    if ret != 0 {
        return Err(ret);
    }
    // SAFETY: initialized by sigaltstack
    let oss = unsafe { oss.assume_init() };
    Ok(oss)
}

// # Safety
// Must not be called concurrently
pub unsafe fn save_stack() -> Result<()> {
    let prev_altstack = sigaltstack_get().map_err(std::io::Error::from_raw_os_error)?;
    // SAFETY: function precondition
    unsafe { (*OSS.get()).write(UnsafeSendSync(prev_altstack)) };
    Ok(())
}

pub fn prepare_altstack() -> Result<(), std::io::Error> {
    let global_state = GlobalState::get();

    // SAFETY: correct api usage
    unsafe {
        let alt_stack = &raw mut (*global_state.reserved).alt_stack;
        let x = stack_t {
            ss_sp: alt_stack.cast(),
            ss_flags: 0,
            ss_size: ALT_STACK_SIZE,
        };
        let ret = run_native(|| sigaltstack(&raw const x, std::ptr::null_mut()));
        if ret != 0 {
            return Err(std::io::Error::from_raw_os_error(ret));
        }

        Ok(())
    }
}

// # Safety
// - Must not be called concurrently
// - save_stack must have been caller prior
pub fn restore_stack() -> Result<(), std::io::Error> {
    // SAFETY: preconditions
    let stack = unsafe { &raw const (*(*OSS.get()).as_ptr()).0 };
    // SAFETY: stack is valid since it comes from save_stack
    let ret = run_native(|| unsafe { sigaltstack(stack, std::ptr::null_mut()) });
    if ret != 0 {
        return Err(std::io::Error::from_raw_os_error(ret));
    }
    Ok(())
}
