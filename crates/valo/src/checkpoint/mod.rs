pub mod gpu;
pub mod reader;
pub(crate) mod threads;
pub mod ucontext;
pub mod writer;

use libc::{siginfo_t, ucontext_t};
use std::fmt::Write;
use std::sync::atomic::Ordering;

use crate::global_state::GlobalState;
use crate::log::structured;
use crate::utils::SignalSafeFormat;
use crate::{Result, abort, alt_stack, error, info, sys, thread_tracker, utils};

pub(crate) const SIG_CHECKPOINT: i32 = libc::SIGSYS;

/// Monotonic timestamp in nanoseconds. async-signal-safe.
fn monotonic_ns() -> u64 {
    let ts = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

/// Allocation-free duration formatting: "123.4ms" below a second, "1.3s" above.
fn fmt_duration(ns: u64) -> heapless::String<16> {
    let mut s = heapless::String::new();
    if ns < 1_000_000_000 {
        let _ = write!(s, "{:.1}ms", ns as f64 / 1e6);
    } else {
        let _ = write!(s, "{:.1}s", ns as f64 / 1e9);
    }
    s
}

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
    /// Parse a flag byte; `None` means the byte is not a valid flag.
    #[inline]
    pub(crate) fn from_u8(byte: u8) -> Option<Self> {
        Some(match byte {
            0 => PageFlag::No,
            1 => PageFlag::Zero,
            2 => PageFlag::Full,
            3 => PageFlag::File,
            4 => PageFlag::Guard,
            _ => return None,
        })
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
    pub(crate) threads: threads::SavedThreads,
}
impl Header {
    pub(crate) fn as_bytes(&self) -> &[u8] {
        // SAFETY: repr(C)
        unsafe { std::slice::from_raw_parts(std::ptr::from_ref(self).cast(), size_of::<Self>()) }
    }
}

// # Safety
// - Must not be called concurrently
unsafe fn snapshot_create(ucontext: *const libc::ucontext_t, slot: SnapshotSlot) -> Result<()> {
    // SAFETY: same preconditions
    unsafe { ucontext::save(ucontext) };
    let mut writer = writer::SnapshotWriter::new(slot)?;
    writer::write_all_areas(&mut writer, threads::saved_threads())?;

    Ok(())
}

// # Safety
// - Must not be called concurrently
// - `snapshot_create` must have been called prior
unsafe fn snapshot_restore(ucontext: *const libc::ucontext_t, slot: SnapshotSlot) -> Result<()> {
    // Save current X11 connection state before restore overwrites it
    let x11_state = crate::hooks::xlib::xfixup::X11SavedState::save()?;

    let mut reader = reader::SnapshotReader::new(slot)?;

    // GPU restore time: trigger + wait
    let mut gpu_ns = 0_u64;

    // Restore the GPU host concurrently with the CPU memory restore:
    // Memory and kumquat handle disjoint data:
    // - the CPU restore rewrites the guest-shared memfd contents
    //   (kumquat-guest-blob areas, ASG rings) from the snapshot,
    // - kumquat's restore only re-attaches those mappings and replays its
    //   own host state; the single guest-visible write it performs (ASG ring
    //   control state) is identical to the snapshot content, so racing
    //   writers produce the same bytes either way.
    structured::events::trace_step("restore.gpu.trigger.begin");
    let gpu_start = monotonic_ns();
    gpu::trigger_restore(slot)?;
    gpu_ns += monotonic_ns() - gpu_start;
    structured::events::trace_step("restore.gpu.trigger.end");

    let mem_start = monotonic_ns();
    structured::events::trace_step("restore.memory.begin");
    reader::read_all_areas(&mut reader)?;
    structured::events::trace_step("restore.memory.end");
    let mem_ns = monotonic_ns() - mem_start;

    structured::events::trace_step("restore.gpu.wait.begin");
    let gpu_start = monotonic_ns();
    gpu::wait_restore_complete()?;
    gpu_ns += monotonic_ns() - gpu_start;
    structured::events::trace_step("restore.gpu.wait.end");

    // Restore threads that died since the checkpoint
    threads::recreate_missing();

    // Restore X11 connection state (sequence numbers, xcb connection)
    // Must happen after read_all_areas but before ucontext::restore
    x11_state.restore()?;

    crate::hooks::vulkan::queue_outofdate_swapchain();

    GlobalState::get().set_restore_in_progress(true);

    info!(
        Restore,
        "snapshot restore done (memory {} + gpu wait {})",
        fmt_duration(mem_ns),
        fmt_duration(gpu_ns)
    );

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
        structured::events::trace_step("checkpoint.memory.begin");
        // SAFETY: same precondition
        unsafe { snapshot_create(ucontext, slot)? }
        structured::events::trace_step("checkpoint.memory.end");
        info!(Checkpoint, "snapshot create done");
    } else {
        info!(Restore, "snapshot restore begin");
        structured::events::trace_step("restore.apply.begin");
        // SAFETY: same precondition
        unsafe { snapshot_restore(ucontext, slot)? };
        structured::events::trace_step("restore.apply.end");
    }

    Ok(())
}

pub unsafe extern "C" fn signal_handler(
    _signum: std::ffi::c_int,
    _info: *const siginfo_t,
    ucontext: *const ucontext_t,
) {
    if let Err(e) = utils::assert_no_alloc(|| checkpoint_handler_inner(ucontext)) {
        // SignalSafeError: io::Error's Display calls strerror, which
        // allocates and would trip the guard before this line is logged.
        error!(
            Checkpoint,
            "error in checkpoint handler: {}",
            SignalSafeFormat(&e)
        );
        std::process::abort();
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

    structured::events::trace_step("checkpoint.freeze.begin");
    threads::suspend_others(None);
    structured::events::trace_step("checkpoint.freeze.end");

    structured::events::trace_step("checkpoint.gpu.trigger.begin");
    gpu::trigger_snapshot(slot)?;
    structured::events::trace_step("checkpoint.gpu.trigger.end");

    let mem_start_ns = monotonic_ns();
    // SAFETY: no preconditions
    unsafe { libc::raise(SIG_CHECKPOINT) };
    let mem_ns = monotonic_ns() - mem_start_ns;

    let restoring = state.restore_in_progress();

    // only wait for gpu on checkpoint, not on resume
    if !restoring {
        structured::events::trace_step("checkpoint.gpu.wait.begin");
        gpu::wait_snapshot_complete()?; // stay frozen until gpu checkpoint is finished
        structured::events::trace_step("checkpoint.gpu.wait.end");
        let gpu_ns = monotonic_ns() - mem_start_ns - mem_ns;
        info!(
            Checkpoint,
            "snapshot done (memory {} + gpu wait {})",
            fmt_duration(mem_ns),
            fmt_duration(gpu_ns)
        );
    }

    threads::resume_others();

    let restoring = state.restore_in_progress();

    alt_stack::restore_stack()?;

    if restoring {
        structured::events::restore_end(slot);
    } else {
        structured::events::checkpoint_end(slot);
    }

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

    let snapshot = reader::SnapshotReader::new(slot)?;

    state.snapshot_slot.store(slot.0, Ordering::SeqCst);
    structured::events::restore_begin(slot);
    state.set_restore_in_progress(true);

    alt_stack::prepare_altstack()?;

    structured::events::trace_step("restore.freeze.begin");
    threads::suspend_others(Some(&snapshot.header.threads));
    drop(snapshot);
    structured::events::trace_step("restore.freeze.end");

    // SAFETY: no preconditions
    unsafe { libc::raise(SIG_CHECKPOINT) };

    abort!(
        Checkpoint,
        "restore returned without restoring main context"
    );
}
