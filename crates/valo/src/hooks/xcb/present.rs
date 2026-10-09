//! xcb present hooks.

use std::os::raw::c_char;

use super::{xcb_connection_t, xcb_void_cookie_t};
use crate::{debug, override_sym};

const LIB_XCB_PRESENT: *const c_char = c"libxcb-present.so.0".as_ptr();

override_sym!(xcb_present_select_input(conn: *mut xcb_connection_t, eid: u32, window: u32, event_mask: u32) -> xcb_void_cookie_t, lib: LIB_XCB_PRESENT, |real| {
    let cookie = real(conn, eid, window, event_mask);
    debug!(Hooks, "present_select_input conn={conn:p} eid={eid} window={window:#x} mask={event_mask:#x} seq={}", cookie.sequence);

    cookie
});
