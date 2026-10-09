// TODO(ai-review): generated, not yet audited
//! Top-level window tracking for input feeding.

use std::ffi::c_void;

use super::{LIB_XCB, xcb_connection_t};
use crate::override_sym;

unsafe extern "C" {
    fn xcb_get_setup(conn: *mut xcb_connection_t) -> *const c_void;
    fn xcb_setup_roots_iterator(setup: *const c_void) -> ScreenIterator;
}

override_sym!(xcb_create_window(
    conn: *mut xcb_connection_t,
    depth: u8,
    wid: u32,
    parent: u32,
    x: i16,
    y: i16,
    width: u16,
    height: u16,
    border_width: u16,
    class: u16,
    visual: u32,
    value_mask: u32,
    value_list: *const c_void
) -> super::xcb_void_cookie_t, lib: LIB_XCB, |real| {
    let cookie = real(conn, depth, wid, parent, x, y, width, height, border_width, class, visual, value_mask, value_list);
    if root_window(conn) == Some(parent) {
        crate::input_feed::track_create(wid);
    }
    cookie
});

override_sym!(xcb_create_window_checked(
    conn: *mut xcb_connection_t,
    depth: u8,
    wid: u32,
    parent: u32,
    x: i16,
    y: i16,
    width: u16,
    height: u16,
    border_width: u16,
    class: u16,
    visual: u32,
    value_mask: u32,
    value_list: *const c_void
) -> super::xcb_void_cookie_t, lib: LIB_XCB, |real| {
    let cookie = real(conn, depth, wid, parent, x, y, width, height, border_width, class, visual, value_mask, value_list);
    if root_window(conn) == Some(parent) {
        crate::input_feed::track_create(wid);
    }
    cookie
});

override_sym!(xcb_map_window(conn: *mut xcb_connection_t, window: u32) -> super::xcb_void_cookie_t, lib: LIB_XCB, |real| {
    let cookie = real(conn, window);
    crate::input_feed::track_map(window);
    cookie
});

override_sym!(xcb_map_window_checked(conn: *mut xcb_connection_t, window: u32) -> super::xcb_void_cookie_t, lib: LIB_XCB, |real| {
    let cookie = real(conn, window);
    crate::input_feed::track_map(window);
    cookie
});

override_sym!(xcb_destroy_window(conn: *mut xcb_connection_t, window: u32) -> super::xcb_void_cookie_t, lib: LIB_XCB, |real| {
    let cookie = real(conn, window);
    crate::input_feed::track_destroy(window);
    cookie
});

override_sym!(xcb_destroy_window_checked(conn: *mut xcb_connection_t, window: u32) -> super::xcb_void_cookie_t, lib: LIB_XCB, |real| {
    let cookie = real(conn, window);
    crate::input_feed::track_destroy(window);
    cookie
});

/// Root window of the connection's first screen.
fn root_window(conn: *mut xcb_connection_t) -> Option<u32> {
    // SAFETY: conn is the game's live xcb connection.
    let setup = unsafe { xcb_get_setup(conn) };
    if setup.is_null() {
        return None;
    }
    // SAFETY: `setup` is the connection's setup.
    let iter = unsafe { xcb_setup_roots_iterator(setup) };
    // SAFETY: a non-null `data` points to an xcb_screen_t, whose first field is the root window.
    (!iter.data.is_null()).then(|| unsafe { iter.data.cast::<u32>().read() })
}

/// `xcb_screen_iterator_t` (only the fields needed to read `data`).
#[repr(C)]
struct ScreenIterator {
    data: *const c_void,
    rem: i32,
    index: i32,
}
