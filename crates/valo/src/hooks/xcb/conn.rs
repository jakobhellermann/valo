//! xcb connection tracking.

use std::ffi::{c_char, c_int};

use super::{LIB_XCB, xcb_auth_info_t, xcb_connection_t};
use crate::utils::AtomicPtrVec;
use crate::{debug, override_sym, warn};

pub const MAX_CONNECTIONS: usize = 4;
pub static GAME_XCB_CONNS: AtomicPtrVec<MAX_CONNECTIONS> = AtomicPtrVec::new();

override_sym!(xcb_connect(display_name: *const c_char, screen: *mut c_int) -> *mut xcb_connection_t, lib: LIB_XCB, |real| {
    let conn = real(display_name, screen);
    // libxcb returns a non-NULL connection even on failure (failure is
    // reported via xcb_connection_has_error); still guard against NULL.
    if !conn.is_null() {
        if GAME_XCB_CONNS.push(conn.cast()).is_err() {
            warn!(Hooks, "too many xcb connections, ignoring {conn:p}");
        }
        debug!(Hooks, "xcb_connect: {conn:p} (total: {})", GAME_XCB_CONNS.len());
    }
    conn
});

override_sym!(xcb_disconnect(conn: *mut xcb_connection_t), lib: LIB_XCB, |real| {
    GAME_XCB_CONNS.remove(conn.cast());
    super::queue::remove(conn);
    debug!(Hooks, "xcb_disconnect: {conn:p} (total: {})", GAME_XCB_CONNS.len());
    real(conn)
});

override_sym!(xcb_connect_to_fd(fd: c_int, auth_info: *mut xcb_auth_info_t) -> *mut xcb_connection_t, lib: LIB_XCB, |real| {
    let conn = real(fd, auth_info);
    debug!(Hooks, "xcb_connect_to_fd fd={fd} -> {conn:p}");
    conn
});
