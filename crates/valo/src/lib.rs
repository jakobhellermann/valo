#![warn(clippy::undocumented_unsafe_blocks)]
#![feature(
    sync_unsafe_cell,
    io_const_error,
    maybe_uninit_as_bytes,
    alloc_error_hook,
    extern_types
)]

mod alt_stack;
mod checkpoint;
mod debug;
mod frame;
mod glibc;
mod global_state;
mod hooks;
mod ipc;
pub mod log;
mod procfs;
mod sigsegv;
mod sys;
mod thread_tracker;
mod token;
mod utils;

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
    log::init();
    install_panic_hook();
    // SAFETY: since this is an initializer there is unique access
    unsafe { GlobalState::set(GlobalState::new()?) };
    thread_tracker::init_main();
    checkpoint::threads::init_main();
    checkpoint::init_thread()?;
    sigsegv::install()?;
    ipc::init()?;
    checkpoint::gpu::init()?;

    info!(Init, "valo initialized");
    structured::events::init();
    Ok(())
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
