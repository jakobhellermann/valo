// TODO(ai-review): just hacked together. needs proper implementation
use std::os::raw::c_char;

use crate::override_sym;

override_sym!(XOpenDisplay(display_name: *const c_char) -> *const () |real| {
    let ret = real(display_name);
    eprintln!("[valo] XOpenDisplay hooked: {ret:?}");
    ret
});
