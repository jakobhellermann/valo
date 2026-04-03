// TODO(ai-review): just hacked together. needs proper implementation
use std::os::raw::c_char;

#[unsafe(no_mangle)]
pub extern "C" fn XOpenDisplay(display_name: *const c_char) -> *const () {
    let ptr = crate::sym!(c"XOpenDisplay");
    let fn_ptr =
        unsafe { std::mem::transmute::<*mut (), extern "C" fn(*const c_char) -> *const ()>(ptr) };

    dbg!(display_name);

    fn_ptr(display_name)
}
