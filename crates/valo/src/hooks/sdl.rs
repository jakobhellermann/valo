#![allow(unused)]
use std::ffi::c_uint;
use std::os::raw::c_int;

use crate::override_sym;

#[unsafe(no_mangle)]
override_sym!(SDL_RenderPresent(renderer: *const ()) |real| {
    crate::frame::frame_boundary(move || real(renderer));
});

override_sym!(SDL_DYNAPI_entry(
    _apiver: c_uint,
    _table: *const (),
    _table_size: c_uint
) -> c_int |real| {
    todo!();
});
