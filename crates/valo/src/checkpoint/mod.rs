pub mod fd_sync;
pub mod gpu;
pub mod reader;
pub(crate) mod threads;
pub mod ucontext;
pub mod writer;

use libc::{siginfo_t, ucontext_t};
use std::fmt::{NumBuffer, Write};
use std::fs::{File, OpenOptions};
use std::sync::atomic::Ordering;

use crate::global_state::GlobalState;
use crate::log::structured;
use crate::token::{Exclusive, ExclusiveCell, Frozen};
use crate::utils::SignalSafeFormat;
use crate::{Result, abort, alt_stack, error, home, info, sys, thread_tracker, utils};

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

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct SnapshotSlot(pub u32);

pub(crate) fn ensure_slot_dir(slot: SnapshotSlot) -> Result<()> {
    let path = home::get().join("slots").join(slot.0.to_string());
    std::fs::create_dir_all(path)
}

fn open_snapshot_files(slot: SnapshotSlot) -> Result<(File, File), std::io::Error> {
    let dir = home::open()?;
    let mut dir_opts = OpenOptions::new();
    dir_opts.read(true);
    let mut file_opts = OpenOptions::new();
    file_opts.write(true).read(true).create(true);

    let slots = dir.open_dir_with("slots", &dir_opts)?;
    let slot = slots.open_dir_with(slot.0.format_into(&mut NumBuffer::new()), &dir_opts)?;
    let snapshot_meta = slot.open_file_with("meta", &file_opts)?;
    let snapshot_page_data = slot.open_file_with("pages", &file_opts)?;
    Ok((snapshot_meta, snapshot_page_data))
}

/// Whether `slot` holds a completed snapshot.
pub fn slot_exists(slot: SnapshotSlot) -> bool {
    let Ok(meta) = std::fs::metadata(
        home::get()
            .join("slots")
            .join(slot.0.to_string())
            .join("meta"),
    ) else {
        return false;
    };
    meta.len() >= size_of::<Header>() as u64
}

/// Check whether a restore can be done
pub fn preflight_restore(slot: SnapshotSlot) -> Result<(), String> {
    if !slot_exists(slot) {
        return Err(format!("no snapshot in slot {}", slot.0));
    }
    Ok(())
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

// The fd table of the checkpoint, in restorable memory.
static FD_SNAPSHOT: ExclusiveCell<Vec<fd_sync::FdRecord>> = ExclusiveCell::new(Vec::new());
fn fd_snapshot(excl: &mut Exclusive) -> &[fd_sync::FdRecord] {
    FD_SNAPSHOT.get(excl).as_slice()
}

fn snapshot_create(
    excl: &mut Exclusive,
    frozen: &Frozen,
    ucontext: &ucontext_t,
    slot: SnapshotSlot,
) -> Result<()> {
    ucontext::save(excl, ucontext);

    let mut writer = writer::SnapshotWriter::new(slot)?;
    writer::write_all_areas(frozen, excl, &mut writer, threads::saved_threads())?;

    Ok(())
}

fn snapshot_restore(
    excl: &mut Exclusive,
    frozen: &Frozen,
    ucontext: &mut ucontext_t,
    slot: SnapshotSlot,
) -> Result<()> {
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
    reader::read_all_areas(frozen, &mut reader)?;
    structured::events::trace_step("restore.memory.end");
    let mem_ns = monotonic_ns() - mem_start;

    structured::events::trace_step("restore.gpu.wait.begin");
    let gpu_start = monotonic_ns();
    gpu::wait_restore_complete()?;
    gpu_ns += monotonic_ns() - gpu_start;
    structured::events::trace_step("restore.gpu.wait.end");

    // Restore threads that died since the checkpoint
    threads::recreate_missing(excl);

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

    ucontext::restore(excl, ucontext);

    Ok(())
}

// # Safety
// - Must not be called concurrently
// - globalstate.restore_in_progress must be false/true for create/restore
pub fn checkpoint_handler_inner(ucontext: *const ucontext_t) -> Result<()> {
    let state = GlobalState::get();
    let slot = SnapshotSlot(state.snapshot_slot.load(Ordering::SeqCst));

    utils::assert_stack_in_range(state.alt_stack_memory());

    // SAFETY: Both checkpoint and resume suspended all other threads before raising.
    let frozen = &unsafe { Frozen::assume() };
    // SAFETY: runs inside the single checkpoint/restore session.
    let exclusive = &mut unsafe { Exclusive::assume() };
    // SAFETY: the kernel passes the live interrupted context to the handler.
    let ucontext = unsafe { &mut *ucontext.cast_mut() };

    if !state.restore_in_progress() {
        info!(Checkpoint, "snapshot create begin");
        structured::events::trace_step("checkpoint.memory.begin");
        // SAFETY: same precondition
        snapshot_create(exclusive, frozen, ucontext, slot)?;
        structured::events::trace_step("checkpoint.memory.end");
        info!(Checkpoint, "snapshot create done");
    } else {
        info!(Restore, "snapshot restore begin");
        structured::events::trace_step("restore.apply.begin");
        snapshot_restore(exclusive, frozen, ucontext, slot)?;
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

pub fn checkpoint(slot: SnapshotSlot) -> Result<()> {
    let exclusive = &mut Exclusive::enter_exclusive();

    thread_tracker::require_no_cancellation();
    let state = GlobalState::get();
    state.snapshot_slot.store(slot.0, Ordering::SeqCst);
    state.set_restore_in_progress(false);
    structured::events::checkpoint_begin(slot);

    ensure_slot_dir(slot)?;

    alt_stack::save_stack(exclusive)?;

    alt_stack::prepare_altstack()?;

    structured::events::trace_step("checkpoint.freeze.begin");
    let frozen = &threads::suspend_others(exclusive, None);
    structured::events::trace_step("checkpoint.freeze.end");

    structured::events::trace_step("checkpoint.fds.begin");
    let fd_records = fd_sync::snapshot(exclusive, frozen, slot)?;
    *FD_SNAPSHOT.get(exclusive) = fd_records;
    structured::events::trace_step("checkpoint.fds.end");

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
    } else {
        structured::events::trace_step("restore.fds.sync.begin");
        // The rewind restored the slot's records into the channel.
        let fd_records = std::mem::take(FD_SNAPSHOT.get(exclusive));
        fd_sync::restore(exclusive, frozen, slot, &fd_records)?;
        structured::events::trace_step("restore.fds.sync.end");
    }

    threads::resume_others();

    let restoring = state.restore_in_progress();

    alt_stack::restore_stack(exclusive)?;

    if restoring {
        structured::events::restore_end(slot);
    } else {
        structured::events::checkpoint_end(slot);
    }

    Ok(())
}

pub fn restore(slot: SnapshotSlot) -> Result<()> {
    let mut exclusive = Exclusive::enter_exclusive();

    thread_tracker::require_no_cancellation();
    let state = GlobalState::get();

    let reserved_state = state.reserved.exclusive.get(&mut exclusive);
    let restore_count = reserved_state.restore_count;
    reserved_state.restore_count += 1;

    let max_restores = state.max_restores.load(Ordering::Relaxed);
    if max_restores > 0 && restore_count >= max_restores {
        info!("max_restores={max_restores} reached, exiting--");
        std::process::exit(0);
    }

    let snapshot = reader::SnapshotReader::new(slot)?;

    state.snapshot_slot.store(slot.0, Ordering::SeqCst);
    structured::events::restore_begin(slot);
    state.set_restore_in_progress(true);

    alt_stack::prepare_altstack()?;

    structured::events::trace_step("restore.freeze.begin");
    let frozen = threads::suspend_others(&mut exclusive, Some(&snapshot.header.threads));
    drop(snapshot);
    structured::events::trace_step("restore.freeze.end");

    structured::events::trace_step("restore.fds.begin");
    structured::events::log_fd_list(&mut exclusive, &frozen, "restore.fds")?;
    structured::events::trace_step("restore.fds.end");

    // SAFETY: no preconditions
    unsafe { libc::raise(SIG_CHECKPOINT) };

    abort!(
        Checkpoint,
        "restore returned without restoring main context"
    );
}
