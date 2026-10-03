pub mod reader;
pub mod ucontext;
pub mod writer;

use libc::{siginfo_t, ucontext_t};
use std::fmt::Write;
use std::sync::atomic::Ordering;

use crate::global_state::GlobalState;
use crate::log::structured;
use crate::{Result, alt_stack, error, info, sys, thread_tracker, utils};

pub(crate) const SIG_CHECKPOINT: i32 = libc::SIGSYS;

#[derive(Clone, Copy)]
pub(crate) struct SnapshotSlot(pub u32);

fn snapshot_path(slot: SnapshotSlot, kind: &str) -> heapless::String<64> {
    let mut path = heapless::String::new();
    write!(path, "/tmp/valo-snapshot-{}-{kind}", slot.0).unwrap();
    path
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageFlag {
    // No physical page is mapped at the address
    No,
    // Page is entirely zero
    #[allow(dead_code)]
    Zero,
    // Page is saved
    Full,
    // Page is same as the mapped file
    File,
    // Page causing signal on access
    Guard,
}

impl PageFlag {
    /// Reinterpret a flag byte without validation.
    ///
    /// # Safety
    /// `byte` must be a valid `PageFlag` discriminant (0..=4)
    #[inline]
    pub(crate) unsafe fn from_u8_unchecked(byte: u8) -> Self {
        // SAFETY: repr(u8) fieldless enum, valid discriminant per contract
        unsafe { std::mem::transmute::<u8, PageFlag>(byte) }
    }
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
unsafe fn snapshot_create(ucontext: *const libc::ucontext_t, slot: SnapshotSlot) -> Result<()> {
    // SAFETY: same preconditions
    unsafe { ucontext::save(ucontext) };
    let mut writer = writer::SnapshotWriter::new(slot)?;
    writer::write_all_areas(&mut writer)?;

    Ok(())
}

// # Safety
// - Must not be called concurrently
// - `snapshot_create` must have been called prior
unsafe fn snapshot_restore(ucontext: *const libc::ucontext_t, slot: SnapshotSlot) -> Result<()> {
    // Save current X11 connection state before restore overwrites it
    let x11_state = crate::hooks::xlib::xfixup::X11SavedState::save()?;

    let mut reader = reader::SnapshotReader::new(slot)?;
    reader::read_all_areas(&mut reader)?;

    // Restore X11 connection state (sequence numbers, xcb connection)
    // Must happen after read_all_areas but before ucontext::restore
    x11_state.restore()?;

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
    let slot = SnapshotSlot(state.snapshot_slot.load(Ordering::SeqCst));

    utils::assert_stack_in_range(state.alt_stack_memory());

    if !state.restore_in_progress() {
        info!(Checkpoint, "snapshot create begin");
        // SAFETY: same precondition
        unsafe { snapshot_create(ucontext, slot)? }
        info!(Checkpoint, "snapshot create done");
    } else {
        info!(Restore, "snapshot restore begin");
        // SAFETY: same precondition
        unsafe { snapshot_restore(ucontext, slot)? };
        info!(Restore, "snapshot restore done");
    }

    Ok(())
}

pub unsafe extern "C" fn signal_handler(
    _signum: std::ffi::c_int,
    _info: *const siginfo_t,
    ucontext: *const ucontext_t,
) {
    if let Err(e) = utils::assert_no_alloc(|| checkpoint_handler_inner(ucontext)) {
        error!(Checkpoint, "error in checkpoint handler: {}", e);
    }
}

pub fn init_thread() -> Result<()> {
    sys::pthread_unblock_signal(SIG_CHECKPOINT);
    sys::register_onstack_signal_handler(SIG_CHECKPOINT, signal_handler)?;

    Ok(())
}

// # Safety
// Must not be called concurrently
pub unsafe fn checkpoint(slot: SnapshotSlot) -> Result<()> {
    thread_tracker::require_no_cancellation();
    let state = GlobalState::get();
    state.snapshot_slot.store(slot.0, Ordering::SeqCst);
    state.set_restore_in_progress(false);
    structured::events::checkpoint_begin(slot);

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
pub unsafe fn restore(slot: SnapshotSlot) -> Result<()> {
    thread_tracker::require_no_cancellation();
    let state = GlobalState::get();

    // SAFETY: reserved is valid and restore_count is in the excluded region
    let restore_count = unsafe { &(*state.reserved).restore_count }
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

    if state.max_restores > 0 && restore_count >= state.max_restores {
        info!(
            "VALO_MAX_RESTORES={} reached, exiting--",
            state.max_restores
        );
        std::process::exit(0);
    }

    state.snapshot_slot.store(slot.0, Ordering::SeqCst);
    structured::events::restore_begin(slot);
    state.set_restore_in_progress(true);

    alt_stack::prepare_altstack()?;

    // SAFETY: no preconditions
    unsafe { libc::raise(SIG_CHECKPOINT) };

    Ok(())
}
