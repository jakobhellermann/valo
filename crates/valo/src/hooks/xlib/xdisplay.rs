// TODO(ai-review): just hacked together. needs proper implementation
use std::os::raw::{c_char, c_int};

use crate::hooks::xlib::MAX_DISPLAYS;
use crate::utils::AtomicPtrVec;
use crate::{debug, override_sym, warn};

pub static GAME_DISPLAYS: AtomicPtrVec<MAX_DISPLAYS> = AtomicPtrVec::new();

override_sym!(XOpenDisplay(display_name: *const c_char) -> *const () |real| {
    let display = real(display_name);

    if !display.is_null() {
        if GAME_DISPLAYS.push(display).is_err() {
            warn!(Hooks, "too many X displays, ignoring");
        }
        debug!(Hooks, "XOpenDisplay: {display:?} (total: {})", GAME_DISPLAYS.len());
    }

    display
});

override_sym!(XCloseDisplay(display: *const ()) -> c_int |real| {
    GAME_DISPLAYS.remove(display);
    real(display)
});
