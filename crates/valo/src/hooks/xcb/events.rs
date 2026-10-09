//! xcb event entry hooks

use std::os::raw::c_int;

use super::{LIB_XCB, xcb_connection_t, xcb_extension_t, xcb_generic_event_t, xcb_special_event_t};
use crate::{debug, override_sym, trace};

override_sym!(xcb_poll_for_event(conn: *mut xcb_connection_t) -> *mut xcb_generic_event_t, lib: LIB_XCB, |real| {
    let event = real(conn);
    let (code, name) = debug::xcb::describe_event(event);
    if code == 0 {
        trace!(Hooks, "xcb_poll_for_event conn={conn:p} -> none");
    } else {
        trace!(
            Hooks,
            "xcb_poll_for_event conn={conn:p} -> {name} ({code})",
        );
    }
    event
});

override_sym!(xcb_wait_for_event(conn: *mut xcb_connection_t) -> *mut xcb_generic_event_t, lib: LIB_XCB, |real| {
    debug!(
        Hooks,
        "xcb_wait_for_event enter conn={conn:p}",
    );
    let event = real(conn);
    let (code, name) = debug::xcb::describe_event(event);
    debug!(Hooks, "xcb_wait_for_event conn={conn:p} -> {name} ({code})");
    event
});

override_sym!(xcb_flush(conn: *mut xcb_connection_t) -> c_int, lib: LIB_XCB, |real| { real(conn) });

override_sym!(xcb_wait_for_special_event(conn: *mut xcb_connection_t, se: *mut xcb_special_event_t) -> *mut xcb_generic_event_t, lib: LIB_XCB, |real| {
    trace!(Hooks, "xcb_wait_for_special_event enter conn={conn:p} se={se:p}");
    let event = real(conn, se);
    trace!(
        Hooks,
        "xcb_wait_for_special_event returned conn={conn:p} se={se:p} event={event:p}",
    );
    event
});

override_sym!(xcb_poll_for_special_event(conn: *mut xcb_connection_t, se: *mut xcb_special_event_t) -> *mut xcb_generic_event_t, lib: LIB_XCB, |real| {
    let event = real(conn, se);
    let (code, name) = debug::xcb::describe_event(event);
    if code == 0 {
        trace!(Hooks, "xcb_poll_for_special_event conn={conn:p} se={se:p} -> none");
    } else {
        debug!(Hooks, "xcb_poll_for_special_event conn={conn:p} se={se:p} -> {name} ({code})");
    }
    event
});

override_sym!(xcb_register_for_special_xge(conn: *mut xcb_connection_t, ext: *mut xcb_extension_t, eid: u32, stamp: *mut u32) -> *mut xcb_special_event_t, lib: LIB_XCB, |real| {
    let se = real(conn, ext, eid, stamp);
    debug!(Hooks, "xcb_register_for_special_xge conn={conn:p} ext={ext:p} eid={eid} -> {se:p}");
    se
});

override_sym!(xcb_unregister_for_special_event(conn: *mut xcb_connection_t, se: *mut xcb_special_event_t), lib: LIB_XCB, |real| {
    debug!(Hooks, "xcb_unregister_for_special_event conn={conn:p} se={se:p}");
    real(conn, se)
});
