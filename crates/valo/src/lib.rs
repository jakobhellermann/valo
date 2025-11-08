#![warn(clippy::undocumented_unsafe_blocks)]
#![feature(
    sync_unsafe_cell,
    io_const_error,
    maybe_uninit_as_bytes,
    maybe_uninit_slice
)]

mod alt_stack;
mod checkpoint;
mod global_state;
mod procfs_maps;
mod stackio;
mod sys;
mod utils;

use crate::global_state::GlobalState;
use crate::utils::{PAGE_SIZE, SendSync};

pub type Result<T, E = std::io::Error> = std::result::Result<T, E>;

#[cfg(feature = "assert-no-alloc")]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

fn init() -> Result<()> {
    unsafe { global_state::set_global_state(GlobalState::new()?) };

    checkpoint::init_thread()?;

    Ok(())
}

#[unsafe(no_mangle)]
pub extern "C" fn valo_init() {
    if let Err(e) = init() {
        eprintln!("{}", e);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn valo_checkpoint() {
    if let Err(e) = checkpoint::checkpoint() {
        eprintln!("{}", e);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn valo_restore() {
    if let Err(e) = checkpoint::restore() {
        eprintln!("{}", e);
    }
}
