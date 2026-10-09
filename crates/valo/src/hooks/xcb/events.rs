//! xcb event entry hooks.
//!
//! With uncontrolled input on these are pure pass-throughs: the game reads
//! real X events unmodified. With it off (default), the fed-event queue is
//! served first and the real connection is drained through an input-class
//! filter, so only scripted input reaches the game.

use std::os::fd::{AsRawFd, BorrowedFd};
use std::os::raw::c_int;

use rustix::event::{PollFd, PollFlags};
use rustix::time::Timespec;

use super::{
    LIB_XCB, queue, xcb_connection_t, xcb_extension_t, xcb_generic_event_t, xcb_special_event_t,
};
use crate::{debug, input_feed, override_sym, trace};

override_sym!(xcb_poll_for_event(conn: *mut xcb_connection_t) -> *mut xcb_generic_event_t, lib: LIB_XCB, |real| {
    let event = poll_substituted(conn, || real(conn));
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
    let event = wait_substituted(conn, || real(conn));
    let (code, name) = debug::xcb::describe_event(event);
    debug!(
        Hooks,
        "xcb_wait_for_event conn={conn:p} -> {name} ({code})",
    );
    event
});

// Xlib-XCB fetches already-queued events through this entry point, so it
// needs the same substitution as poll.
override_sym!(xcb_poll_for_queued_event(conn: *mut xcb_connection_t) -> *mut xcb_generic_event_t, lib: LIB_XCB, |real| {
    let event = poll_substituted(conn, || real(conn));
    let (code, name) = debug::xcb::describe_event(event);
    trace!(
        Hooks,
        "xcb_poll_for_queued_event conn={conn:p} -> {name} ({code})",
    );
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

/// One event from the queue, else (filtered) one from the real connection.
fn poll_substituted(
    conn: *mut xcb_connection_t,
    poll_real: impl Fn() -> *mut xcb_generic_event_t,
) -> *mut xcb_generic_event_t {
    if !input_feed::enabled() {
        return poll_real();
    }
    let fed = queue::pop(conn);
    if !fed.is_null() {
        return fed;
    }
    drain_real(poll_real)
}

/// Block until an event is available. Uncontrolled: the real blocking
/// wait. Scripted: queue, then a non-blocking filtered drain, then parked
/// in poll() on the connection fd plus the queue's notify fd.
fn wait_substituted(
    conn: *mut xcb_connection_t,
    real_wait: impl Fn() -> *mut xcb_generic_event_t,
) -> *mut xcb_generic_event_t {
    if !input_feed::enabled() {
        return real_wait();
    }

    loop {
        let fed = queue::pop(conn);
        if !fed.is_null() {
            return fed;
        }
        let event = drain_real(|| queue::real_poll_for_event(conn));
        if !event.is_null() {
            return event;
        }
        // A dead connection makes poll() return immediately forever; mirror
        // the real wait, which reports the error as a null event.
        if queue::connection_has_error(conn) != 0 {
            return std::ptr::null_mut();
        }
        park(conn);
    }
}

/// Consume real events until one passes the filter (or the connection is
/// empty). Consumed-and-filtered events are freed here.
fn drain_real(poll_real: impl Fn() -> *mut xcb_generic_event_t) -> *mut xcb_generic_event_t {
    loop {
        let event = poll_real();
        if event.is_null() {
            return event;
        }
        // SAFETY: every xcb event struct starts with the one-byte
        // response_type and is at least 32 bytes; `event` is a non-null xcb
        // event pointer returned by libxcb.
        let code = unsafe { (event as *const u8).read() } & 0x7f;
        if !is_input_class(code) {
            observe_window_event(code, event);
            return event;
        }
        // SAFETY: libxcb allocates returned events with malloc; ownership
        // of filtered events is ours.
        unsafe { libc::free(event.cast()) };
    }
}

/// Input-class event codes: key, button, motion, enter/leave, focus, keymap.
fn is_input_class(code: u8) -> bool {
    matches!(code, 2..=11)
}

/// Track window lifetime from Map/DestroyNotify passing through to the
/// game. Clients that build windows without the hooked create entry points
/// (dlopen'd X stacks resolving symbols per-handle) are invisible to the
/// create hooks, but the notifications they selected for still arrive
/// through this queue.
fn observe_window_event(code: u8, event: *mut xcb_generic_event_t) {
    if code != 19 && code != 18 {
        return;
    }
    // SAFETY: the first fields of every xcb event are fixed; Map/Destroy
    // Notify are 32 bytes, so reading `window` is in bounds for the
    // non-null event libxcb handed us.
    let window = unsafe { (*(event as *mut WindowNotifyEvent)).window };
    if code == 19 {
        crate::input_feed::track_promote(window);
    } else {
        crate::input_feed::track_destroy(window);
    }
}

/// `xcb_map_notify_event_t` / `xcb_destroy_notify_event_t` prefix. Unlike
/// key events these carry no timestamp: event and window follow the
/// sequence directly.
#[repr(C)]
struct WindowNotifyEvent {
    response_type: u8,
    pad0: u8,
    sequence: u16,
    event: u32,
    window: u32,
}

/// Park until the connection fd or the queue's notify fd is readable. The
/// poll/nanosleep overrides are pass-throughs; run_native keeps this
/// internal wait out of the sleep diagnostics.
fn park(conn: *mut xcb_connection_t) {
    let conn_fd = queue::connection_fd(conn);
    let notify_fd = queue::notify_fd(conn);
    if conn_fd < 0 || notify_fd < 0 {
        // Nothing to park on: fall back to a short sleep so the loop
        // still makes progress.
        let ts = Timespec {
            tv_sec: 0,
            tv_nsec: 1_000_000,
        };
        let _ = crate::hooks::run_native(|| rustix::thread::nanosleep(&ts));
        return;
    }
    // SAFETY: conn_fd is xcb's fd for the live connection; notify_fd is
    // owned by its queue entry, freed only at xcb_disconnect.
    let (conn_fd, notify_fd) = unsafe {
        (
            BorrowedFd::borrow_raw(conn_fd),
            BorrowedFd::borrow_raw(notify_fd),
        )
    };
    let mut polls = [
        PollFd::new(&conn_fd, PollFlags::IN),
        PollFd::new(&notify_fd, PollFlags::IN),
    ];
    let _ = crate::hooks::run_native(|| rustix::event::poll(&mut polls, None));
    if polls[1].revents().contains(PollFlags::IN) {
        queue::drain_notify(notify_fd.as_raw_fd());
    }
}
