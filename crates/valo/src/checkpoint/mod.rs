pub mod reader;
pub mod ucontext;
pub mod writer;

use std::cell::SyncUnsafeCell;

use libc::{siginfo_t, ucontext_t};

use crate::global_state::GlobalState;
use crate::{Result, alt_stack, sys, utils};

const SIG_CHECKPOINT: i32 = libc::SIGSYS;
const SNAPSHOT_PATH_META: &str = "/tmp/valo-snapshot-areas";
const SNAPSHOT_PATH_PAGEDATA: &str = "/tmp/valo-snapshot-pages";

static RESTORE_IN_PROGRESS: SyncUnsafeCell<bool> = SyncUnsafeCell::new(false);

#[repr(u8)]
#[derive(Clone, Copy, Debug)]
pub enum PageFlag {
    No,
    #[allow(dead_code)]
    Zero,
    Full,
    File,
    Guard,
}

#[derive(Debug)]
#[repr(C)]
pub struct AreaMeta {
    skip: bool,
    uncommitted: bool,
    page_data_offset: u64,
}

#[repr(C)]
pub struct Header {
    area_count: u64,
}

unsafe fn snapshot_create(ucontext: *const libc::ucontext_t) -> Result<()> {
    unsafe { ucontext::save(ucontext) };
    writer::write_all_areas()?;

    Ok(())
}

unsafe fn snapshot_restore(ucontext: *const libc::ucontext_t) -> Result<()> {
    reader::read_all_areas()?;
    unsafe { *RESTORE_IN_PROGRESS.get() = true };

    unsafe { ucontext::restore(&mut *ucontext.cast_mut(), ucontext) };

    Ok(())
}

pub fn checkpoint_handler_inner(ucontext: *const ucontext_t) -> Result<()> {
    let state = GlobalState::get();

    utils::assert_stack_in_range(state.alt_stack_memory());

    let is_loading = unsafe { *RESTORE_IN_PROGRESS.get() };
    if !is_loading {
        eprintln!("-- CREATE SAVESTATE --");
        unsafe { snapshot_create(ucontext)? }
    } else {
        eprintln!("-- LOAD SAVESTATE --");
        unsafe { snapshot_restore(ucontext)? };
    }

    Ok(())
}

pub unsafe extern "C" fn signal_handler(
    _signum: std::ffi::c_int,
    _info: *const siginfo_t,
    ucontext: *const ucontext_t,
) {
    if let Err(e) = utils::assert_no_alloc(|| checkpoint_handler_inner(ucontext)) {
        eprintln!("Error in checkpoint handler {}", e);
    }
}

pub fn init_thread() -> Result<()> {
    sys::pthread_unblock_signal(SIG_CHECKPOINT);
    sys::register_onstack_signal_handler(SIG_CHECKPOINT, signal_handler)?;

    Ok(())
}

pub fn checkpoint() -> Result<()> {
    unsafe { *RESTORE_IN_PROGRESS.get() = false };

    alt_stack::save_stack()?;

    alt_stack::prepare_altstack()?;
    unsafe { libc::raise(SIG_CHECKPOINT) };

    alt_stack::restore_stack()?;

    Ok(())
}

pub fn restore() -> Result<()> {
    unsafe { *RESTORE_IN_PROGRESS.get() = true };

    alt_stack::prepare_altstack()?;

    unsafe { libc::raise(SIG_CHECKPOINT) };

    Ok(())
}
