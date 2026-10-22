// TODO(ai-review): generated, not yet audited
//! The pointer the game polls: the fed position while input is controlled, since the real pointer
//! follows whatever the user does outside the game.

use std::ffi::{c_int, c_uint, c_ulong, c_void};

use crate::override_sym;

unsafe extern "C" {
    fn XDefaultRootWindow(display: *mut c_void) -> c_ulong;
}

override_sym!(
    XQueryPointer(
        display: *mut c_void,
        window: c_ulong,
        root: *mut c_ulong,
        child: *mut c_ulong,
        root_x: *mut c_int,
        root_y: *mut c_int,
        win_x: *mut c_int,
        win_y: *mut c_int,
        mask: *mut c_uint
    ) -> c_int
    |real| {
        let Some([x, y]) = crate::input_feed::pointer() else {
            return real(display, window, root, child, root_x, root_y, win_x, win_y, mask);
        };
        // SAFETY: display is the game's open Xlib display, and XQueryPointer requires every
        // return pointer to be valid for writes.
        unsafe {
            *root = XDefaultRootWindow(display);
            *child = 0;
            *root_x = x;
            *root_y = y;
            *win_x = x;
            *win_y = y;
            *mask = 0;
        }
        1
    }
);
