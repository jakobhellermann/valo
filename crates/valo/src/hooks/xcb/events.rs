//! xcb event entry hooks

use std::os::raw::c_int;

use super::{
    LIB_XCB, XcbEvent, xcb_connection_t, xcb_extension_t, xcb_generic_event_t, xcb_special_event_t,
};
use crate::debug::xcb::Described;
use crate::{debug, override_sym, trace};

override_sym!(xcb_poll_for_event(conn: *mut xcb_connection_t) -> *mut xcb_generic_event_t, lib: LIB_XCB, |real| {
    let event = real(conn);
    // SAFETY: libxcb returned the event.
    trace!(Hooks, "xcb_poll_for_event conn={conn:p} -> {}", Described(unsafe { XcbEvent::from_ptr(event) }));
    event
});

override_sym!(xcb_wait_for_event(conn: *mut xcb_connection_t) -> *mut xcb_generic_event_t, lib: LIB_XCB, |real| {
    debug!(
        Hooks,
        "xcb_wait_for_event enter conn={conn:p}",
    );
    let event = real(conn);
    // SAFETY: libxcb returned the event.
    debug!(Hooks, "xcb_wait_for_event conn={conn:p} -> {}", Described(unsafe { XcbEvent::from_ptr(event) }));
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
    if event.is_null() {
        trace!(Hooks, "xcb_poll_for_special_event conn={conn:p} se={se:p} -> none");
    } else {
        // SAFETY: libxcb returned the event.
        debug!(Hooks, "xcb_poll_for_special_event conn={conn:p} se={se:p} -> {}", Described(unsafe { XcbEvent::from_ptr(event) }));
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
