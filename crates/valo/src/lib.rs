#![warn(clippy::undocumented_unsafe_blocks)]
#![feature(sync_unsafe_cell, io_const_error, maybe_uninit_as_bytes)]

mod alt_stack;
mod checkpoint;
mod frame;
mod global_state;
mod hooks;
mod ipc;
pub mod log;
mod procfs_maps;
mod sigsegv;
mod stackio;
mod sys;
mod utils;

use crate::global_state::GlobalState;
use crate::utils::PAGE_SIZE;

pub type Result<T, E = std::io::Error> = std::result::Result<T, E>;

#[cfg(feature = "assert-no-alloc")]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

#[used]
#[cfg_attr(target_os = "linux", unsafe(link_section = ".init_array"))]
static INIT: extern "C" fn() = _init;

extern "C" fn _init() {
    if let Err(e) = init() {
        panic!("Failed to initialize valo: {e}");
    }
}

fn init() -> Result<()> {
    log::init();
    // SAFETY: since this is an initializer there is unique access
    unsafe { GlobalState::set(GlobalState::new()?) };
    checkpoint::init_thread()?;
    sigsegv::install()?;
    ipc::init()?;

    info!(Init, "valo initialized");
    Ok(())
}

#[unsafe(no_mangle)]
pub extern "C" fn valo_checkpoint() {
    // SAFETY: same precondition
    if let Err(e) = unsafe { checkpoint::checkpoint() } {
        error!(Checkpoint, "{}", e);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn valo_restore() {
    // SAFETY: same precondition
    if let Err(e) = unsafe { checkpoint::restore() } {
        error!(Restore, "{}", e);
    }
}
