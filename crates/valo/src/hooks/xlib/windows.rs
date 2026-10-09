// TODO(ai-review): generated, not yet audited
//! Top-level window tracking for input feeding; Xlib creates windows by raw requests the xcb
//! hooks never see.

use std::ffi::{c_int, c_uint, c_ulong, c_void};

use crate::override_sym;

unsafe extern "C" {
    fn XDefaultRootWindow(display: *mut c_void) -> c_ulong;
}

fn is_root(display: *mut c_void, parent: c_ulong) -> bool {
    // SAFETY: display is the game's open Xlib display.
    parent != 0 && parent == unsafe { XDefaultRootWindow(display) }
}

override_sym!(
    XCreateWindow(
        display: *mut c_void,
        parent: c_ulong,
        x: c_int,
        y: c_int,
        width: c_uint,
        height: c_uint,
        border_width: c_uint,
        depth: c_int,
        class: c_int,
        visual: *const c_void,
        valuemask: c_ulong,
        attributes: *const c_void
    ) -> c_ulong
    |real| {
        let window = real(
            display,
            parent,
            x,
            y,
            width,
            height,
            border_width,
            depth,
            class,
            visual,
            valuemask,
            attributes,
        );
        if is_root(display, parent) {
            crate::input_feed::track_create(window as u32);
        }
        window
    }
);

override_sym!(
    XCreateSimpleWindow(
        display: *mut c_void,
        parent: c_ulong,
        x: c_int,
        y: c_int,
        width: c_uint,
        height: c_uint,
        border_width: c_uint,
        border: c_ulong,
        background: c_ulong
    ) -> c_ulong
    |real| {
        let window = real(
            display,
            parent,
            x,
            y,
            width,
            height,
            border_width,
            border,
            background,
        );
        if is_root(display, parent) {
            crate::input_feed::track_create(window as u32);
        }
        window
    }
);

override_sym!(
    XMapWindow(display: *mut c_void, window: c_ulong) -> c_int
    |real| {
        let status = real(display, window);
        crate::input_feed::track_map(window as u32);
        status
    }
);

override_sym!(
    XMapRaised(display: *mut c_void, window: c_ulong) -> c_int
    |real| {
        let status = real(display, window);
        crate::input_feed::track_map(window as u32);
        status
    }
);

override_sym!(
    XDestroyWindow(display: *mut c_void, window: c_ulong) -> c_int
    |real| {
        let status = real(display, window);
        crate::input_feed::track_destroy(window as u32);
        status
    }
);
