//! xcb event entry hooks

use std::os::raw::c_int;

use super::{
    LIB_XCB, XcbEvent, xcb_connection_t, xcb_extension_t, xcb_generic_event_t, xcb_special_event_t,
};
use crate::debug::xcb::Described;
use crate::{debug, input_feed, override_sym, trace};

use super::quiesce::{enter, exit};

override_sym!(xcb_poll_for_event(conn: *mut xcb_connection_t) -> *mut xcb_generic_event_t, lib: LIB_XCB, |real| {
    enter();
    let event = input_feed::poll_event(conn, || real(conn));
    exit();
    // SAFETY: libxcb or the virtual queue returned the event.
    trace!(Hooks, "xcb_poll_for_event conn={conn:p} -> {}", Described(unsafe { XcbEvent::from_ptr(event) }));
    event
});

override_sym!(xcb_wait_for_event(conn: *mut xcb_connection_t) -> *mut xcb_generic_event_t, lib: LIB_XCB, |real| {
    enter();
    debug!(
        Hooks,
        "xcb_wait_for_event enter conn={conn:p}",
    );
    let event = input_feed::wait_event(conn, || real(conn));
    exit();
    // SAFETY: libxcb or the virtual queue returned the event.
    debug!(Hooks, "xcb_wait_for_event conn={conn:p} -> {}", Described(unsafe { XcbEvent::from_ptr(event) }));
    event
});

override_sym!(xcb_poll_for_queued_event(conn: *mut xcb_connection_t) -> *mut xcb_generic_event_t, lib: LIB_XCB, |real| {
    enter();
    let event = input_feed::poll_event(conn, || real(conn));
    exit();
    // SAFETY: libxcb or the virtual queue returned the event.
    trace!(Hooks, "xcb_poll_for_queued_event conn={conn:p} -> {}", Described(unsafe { XcbEvent::from_ptr(event) }));
    event
});

override_sym!(xcb_flush(conn: *mut xcb_connection_t) -> c_int, lib: LIB_XCB, |real| { enter(); let r = real(conn); exit(); r });

override_sym!(xcb_wait_for_special_event(conn: *mut xcb_connection_t, se: *mut xcb_special_event_t) -> *mut xcb_generic_event_t, lib: LIB_XCB, |real| {
    enter();
    trace!(Hooks, "xcb_wait_for_special_event enter conn={conn:p} se={se:p}");
    let event = real(conn, se);
    exit();
    trace!(
        Hooks,
        "xcb_wait_for_special_event returned conn={conn:p} se={se:p} event={event:p}",
    );
    event
});

override_sym!(xcb_poll_for_special_event(conn: *mut xcb_connection_t, se: *mut xcb_special_event_t) -> *mut xcb_generic_event_t, lib: LIB_XCB, |real| {
    enter();
    let event = real(conn, se);
    exit();
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
