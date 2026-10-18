#![warn(clippy::undocumented_unsafe_blocks)]
#![feature(
    sync_unsafe_cell,
    io_const_error,
    maybe_uninit_as_bytes,
    alloc_error_hook,
    extern_types,
    dirfd
)]

mod alt_stack;
mod checkpoint;
mod debug;
mod determinism;
mod engine_setup;
mod frame;
mod gamespecific;
mod glibc;
mod global_state;
mod home;
mod hooks;
mod input_feed;
mod ipc;
pub mod log;
mod present;
mod procfs;
mod sigsegv;
mod sys;
mod thread_tracker;
mod token;
mod utils;

use std::ffi::OsStr;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::global_state::GlobalState;
use crate::log::structured;
use crate::utils::PAGE_SIZE;

fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        error!(Adhoc, "panic: {info}");
    }));
    std::alloc::set_alloc_error_hook(|layout| {
        error!(
            Adhoc,
            "alloc error: {} bytes (align {})",
            layout.size(),
            layout.align()
        );
    });
}

pub type Result<T, E = std::io::Error> = std::result::Result<T, E>;

#[cfg(feature = "assert-no-alloc")]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

#[used]
#[cfg_attr(target_os = "linux", unsafe(link_section = ".init_array"))]
static INIT: extern "C" fn() = _init;

extern "C" fn _init() {
    if let Err(e) = init() {
        abort!("Failed to initialize valo: {e}");
    }
}

fn init() -> Result<()> {
    let pid = std::process::id();
    let exe = std::env::current_exe()?;
    if !is_target() {
        eprintln!(
            "[valo] pid {pid} ({}): not VALO_TARGET, passing through",
            exe.display()
        );
        // LD_PRELOAD stays so the target further down the exec chain loads valo
        return Ok(());
    }
    eprintln!("[valo] pid {pid} ({}): attaching", exe.display());

    remove_ld_preload();
    home::init()?;
    log::init();
    install_panic_hook();
    // SAFETY: the only call of `set`, from the once-run, single-threaded initializer.
    unsafe { GlobalState::set(GlobalState::new()?) };
    thread_tracker::init_main();
    checkpoint::threads::init_main();
    checkpoint::init_thread()?;
    sigsegv::install()?;
    ipc::init()?;
    engine_setup::init(&exe)?;
    checkpoint::gpu::init()?;

    info!(Init, "valo initialized");
    structured::events::init();
    Ok(())
}

/// Resolved lazily, as hooks can run in constructors before `_init`.
pub(crate) fn is_target() -> bool {
    const UNKNOWN: u8 = 0;
    const YES: u8 = 1;
    const NO: u8 = 2;
    static IS_TARGET: AtomicU8 = AtomicU8::new(UNKNOWN);

    match IS_TARGET.load(Ordering::Relaxed) {
        YES => true,
        NO => false,
        _ => {
            let is_target = resolve_target();
            IS_TARGET.store(if is_target { YES } else { NO }, Ordering::Relaxed);
            is_target
        }
    }
}

/// Matches the exe rather than `comm`, which truncates at 15 bytes.
fn resolve_target() -> bool {
    let Some(target) = std::env::var_os("VALO_TARGET") else {
        return true;
    };
    let exe = std::env::current_exe().expect("failed to resolve /proc/self/exe");
    exe.file_name() == Some(target.as_os_str())
}

/// Remove this library from `LD_PRELOAD` so exec'd children of the target do not load it.
fn remove_ld_preload() {
    let Some(entries) = std::env::var_os("LD_PRELOAD") else {
        return;
    };
    let kept: Vec<_> = std::env::split_paths(&entries)
        .filter(|entry| entry.file_name() != Some(OsStr::new("libvalo.so")))
        .collect();

    // SAFETY: init_array code runs before main, single-threaded, before
    // any other code of this process can read the environment.
    unsafe {
        match kept.is_empty() {
            true => std::env::remove_var("LD_PRELOAD"),
            false => std::env::set_var("LD_PRELOAD", std::env::join_paths(kept).unwrap()),
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn valo_checkpoint() {
    valo_checkpoint_slot(0);
}

#[unsafe(no_mangle)]
pub extern "C" fn valo_checkpoint_slot(slot: u32) {
    if let Err(e) = checkpoint::checkpoint(checkpoint::SnapshotSlot(slot)) {
        error!(Checkpoint, "{}", e);
        std::process::abort();
    }
}

/// The IPC checkpoint: a failed save is an error reply, not a session
/// death — the live state survives a failed snapshot.
pub(crate) fn checkpoint_slot(slot: u32) -> Result<()> {
    checkpoint::checkpoint(checkpoint::SnapshotSlot(slot))
}

#[unsafe(no_mangle)]
pub extern "C" fn valo_restore() {
    valo_restore_slot(0);
}

#[unsafe(no_mangle)]
pub extern "C" fn valo_restore_slot(slot: u32) {
    if let Err(e) = checkpoint::restore(checkpoint::SnapshotSlot(slot)) {
        error!(Restore, "{}", e);
        std::process::abort();
    }
}
