use std::ffi::c_uint;
use std::os::raw::c_int;

#[unsafe(no_mangle)]
pub extern "C" fn SDL_RenderPresent(renderer: *const ()) {
    let ptr = crate::sym!(c"SDL_RenderPresent");
    let fn_ptr = unsafe { std::mem::transmute::<*mut (), extern "C" fn(*const ())>(ptr) };
    crate::frame::frame_boundary(move || fn_ptr(renderer));
}

#[unsafe(no_mangle)]
pub extern "C" fn SDL_DYNAPI_entry(
    _apiver: c_uint,
    _table: *const (),
    _table_size: c_uint,
) -> c_int {
    let ptr = crate::sym!(c"SDL_DYNAPI_entry");
    let _fn_ptr = unsafe {
        std::mem::transmute::<*mut (), extern "C" fn(c_uint, *const (), c_uint) -> c_int>(ptr)
    };

    todo!();
}
