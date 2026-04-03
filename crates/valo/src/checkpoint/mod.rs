pub mod reader;
pub mod ucontext;
pub mod writer;

use libc::{siginfo_t, ucontext_t};

use crate::global_state::GlobalState;
use crate::{Result, alt_stack, sys, utils};

const SIG_CHECKPOINT: i32 = libc::SIGSYS;
const SNAPSHOT_PATH_META: &str = "/tmp/valo-snapshot-areas";
const SNAPSHOT_PATH_PAGEDATA: &str = "/tmp/valo-snapshot-pages";

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
impl AreaMeta {
    pub(crate) fn as_bytes(&self) -> &[u8] {
        // SAFETY: repr(C)
        unsafe { std::slice::from_raw_parts(std::ptr::from_ref(self).cast(), size_of::<Self>()) }
    }
}

#[repr(C)]
pub struct Header {
    area_count: u64,
}

// # Safety
// - Must not be called concurrently
unsafe fn snapshot_create(ucontext: *const libc::ucontext_t) -> Result<()> {
    // SAFETY: same preconditions
    unsafe { ucontext::save(ucontext) };
    writer::write_all_areas()?;

    Ok(())
}

// # Safety
// - Must not be called concurrently
// - `snapshot_create` must have been called prior
unsafe fn snapshot_restore(ucontext: *const libc::ucontext_t) -> Result<()> {
    reader::read_all_areas()?;

    GlobalState::get().set_restore_in_progress(true);

    // SAFETY: same preconditions
    unsafe { ucontext::restore(&mut *ucontext.cast_mut(), &*ucontext) };

    Ok(())
}

// # Safety
// - Must not be called concurrently
// - globalstate.restore_in_progress must be false/true for create/restore
pub fn checkpoint_handler_inner(ucontext: *const ucontext_t) -> Result<()> {
    let state = GlobalState::get();

    utils::assert_stack_in_range(state.alt_stack_memory());

    if !state.restore_in_progress() {
        eprintln!("-- CREATE SAVESTATE --");
        // SAFETY: same precondition
        unsafe { snapshot_create(ucontext)? }
    } else {
        eprintln!("-- LOAD SAVESTATE --");
        // SAFETY: same precondition
        unsafe { snapshot_restore(ucontext)? };
        eprintln!("-- LOAD SAVESTATE DONE --");
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

// # Safety
// Must not be called concurrently
pub unsafe fn checkpoint() -> Result<()> {
    GlobalState::get().set_restore_in_progress(false);

    // Safety: function precondition
    unsafe { alt_stack::save_stack()? };

    alt_stack::prepare_altstack()?;

    // SAFETY: no preconditions
    unsafe { libc::raise(SIG_CHECKPOINT) };

    alt_stack::restore_stack()?;

    Ok(())
}

// # Safety
// Must not be called concurrently
pub unsafe fn restore() -> Result<()> {
    GlobalState::get().set_restore_in_progress(true);

    alt_stack::prepare_altstack()?;

    // SAFETY: no preconditions
    unsafe { libc::raise(SIG_CHECKPOINT) };

    Ok(())
}
