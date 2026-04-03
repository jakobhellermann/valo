use std::ffi::{CStr, c_char};
use std::os::raw::c_int;

use crate::override_sym;

override_sym!(dlopen(file: *const c_char, mode: c_int) {
    let _name = match file.is_null() {
        true => <&CStr>::default(),
        false => unsafe { CStr::from_ptr(file) }
    };

    // dbg!(name);
});

/*#[unsafe(no_mangle)]
pub extern "C" fn dlopen(file: *const c_char, mode: c_int) -> *const () {
    let ptr = crate::sym!(c"dlopen");
    let fn_ptr = unsafe {
        std::mem::transmute::<*mut (), extern "C" fn(*const c_char, c_int) -> *const ()>(ptr)
    };

    let name = if file.is_null() {
        None
    } else {
        Some(unsafe { CStr::from_ptr(file) })
    };
    dbg!(name.unwrap_or_default());

    fn_ptr(file, mode)
} */
