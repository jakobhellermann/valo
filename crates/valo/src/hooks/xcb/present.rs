//! xcb present hooks.

use std::ffi::c_void;
use std::os::raw::c_char;

use super::kicks::record_select_input;
use super::{xcb_connection_t, xcb_void_cookie_t};
use crate::{debug, override_sym, window_image};

const LIB_XCB_PRESENT: *const c_char = c"libxcb-present.so.0".as_ptr();

override_sym!(xcb_present_select_input(conn: *mut xcb_connection_t, eid: u32, window: u32, event_mask: u32) -> xcb_void_cookie_t, lib: LIB_XCB_PRESENT, |real| {
    let cookie = real(conn, eid, window, event_mask);
    record_select_input(conn, window, eid);
    debug!(Hooks, "present_select_input conn={conn:p} eid={eid} window={window:#x} mask={event_mask:#x} seq={}", cookie.sequence);

    cookie
});

override_sym!(xcb_present_pixmap(conn: *mut xcb_connection_t, window: u32, pixmap: u32, serial: u32, valid: u32, update: u32, x_off: i16, y_off: i16, target_crtc: u32, wait_fence: u32, idle_fence: u32, options: u32, target_msc: u64, divisor: u64, remainder: u64, notifies_len: u32, notifies: *const c_void) -> xcb_void_cookie_t, lib: LIB_XCB_PRESENT, |real| {
    window_image::record_present(window, pixmap);
    real(conn, window, pixmap, serial, valid, update, x_off, y_off, target_crtc, wait_fence, idle_fence, options, target_msc, divisor, remainder, notifies_len, notifies)
});

override_sym!(xcb_present_pixmap_checked(conn: *mut xcb_connection_t, window: u32, pixmap: u32, serial: u32, valid: u32, update: u32, x_off: i16, y_off: i16, target_crtc: u32, wait_fence: u32, idle_fence: u32, options: u32, target_msc: u64, divisor: u64, remainder: u64, notifies_len: u32, notifies: *const c_void) -> xcb_void_cookie_t, lib: LIB_XCB_PRESENT, |real| {
    window_image::record_present(window, pixmap);
    real(conn, window, pixmap, serial, valid, update, x_off, y_off, target_crtc, wait_fence, idle_fence, options, target_msc, divisor, remainder, notifies_len, notifies)
});
