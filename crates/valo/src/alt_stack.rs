use crate::global_state::GlobalState;
use crate::utils::SendSync;

use std::cell::SyncUnsafeCell;
use std::mem::MaybeUninit;

use libc::{sigaltstack, stack_t};

type Result<T, E = std::io::Error> = std::result::Result<T, E>;

static OSS: SyncUnsafeCell<MaybeUninit<SendSync<stack_t>>> =
    SyncUnsafeCell::new(MaybeUninit::uninit());

pub const ALT_STACK_SIZE: usize = 5 * 1024 * 1024;

fn sigaltstack_get() -> Result<stack_t, i32> {
    let mut oss = MaybeUninit::uninit();
    let ret = unsafe { sigaltstack(std::ptr::null(), oss.as_mut_ptr()) };
    if ret != 0 {
        return Err(ret);
    }
    let oss = unsafe { oss.assume_init() };
    Ok(oss)
}

pub fn save_stack() -> Result<()> {
    let prev_altstack = sigaltstack_get().map_err(std::io::Error::from_raw_os_error)?;
    unsafe { (*OSS.get()).write(SendSync(prev_altstack)) };
    Ok(())
}

pub fn prepare_altstack() -> Result<(), std::io::Error> {
    let global_state = GlobalState::get();
    unsafe {
        let alt_stack = &raw mut (*global_state.reserved).alt_stack;

        let x = stack_t {
            ss_sp: alt_stack.cast(),
            ss_flags: 0,
            ss_size: ALT_STACK_SIZE,
        };
        let ret = sigaltstack(&raw const x, std::ptr::null_mut());
        if ret != 0 {
            return Err(std::io::Error::from_raw_os_error(ret));
        }

        Ok(())
    }
}

pub fn restore_stack() -> Result<(), std::io::Error> {
    let ret = unsafe { sigaltstack(&raw const (*(*OSS.get()).as_ptr()).0, std::ptr::null_mut()) };
    if ret != 0 {
        return Err(std::io::Error::from_raw_os_error(ret));
    }
    Ok(())
}
