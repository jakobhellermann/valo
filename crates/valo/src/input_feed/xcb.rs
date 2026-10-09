// TODO(ai-review): generated, not yet audited
//! With controlled input the virtual queue is served first and real input-class events are
//! dropped, so only scripted input reaches the game.

use std::ffi::c_void;
use std::ops::RangeInclusive;
use std::os::fd::{AsFd, BorrowedFd};
use std::os::raw::c_int;
use std::sync::atomic::AtomicPtr;

use rustix::event::{PollFd, PollFlags};
use rustix::io::Errno;
use x11rb_protocol::protocol::Event;
use x11rb_protocol::protocol::xproto::{
    DESTROY_NOTIFY_EVENT, KEY_PRESS_EVENT, KEYMAP_NOTIFY_EVENT, MAP_NOTIFY_EVENT,
    UNMAP_NOTIFY_EVENT,
};

use super::queue;
use crate::abort;
use crate::hooks::link_function;
use crate::hooks::xcb::{LIB_XCB, XcbEvent, xcb_connection_t, xcb_generic_event_t};

unsafe extern "C" {
    fn xcb_get_file_descriptor(conn: *mut xcb_connection_t) -> c_int;
    fn xcb_connection_has_error(conn: *mut xcb_connection_t) -> i32;
}

const INPUT_EVENTS: RangeInclusive<u8> = KEY_PRESS_EVENT..=KEYMAP_NOTIFY_EVENT;

pub fn poll_event(
    conn: *mut xcb_connection_t,
    poll_real: impl Fn() -> *mut xcb_generic_event_t,
) -> *mut xcb_generic_event_t {
    let event = poll_substituted(conn, poll_real);
    track_window(event);
    event
}

pub fn wait_event(
    conn: *mut xcb_connection_t,
    wait_real: impl Fn() -> *mut xcb_generic_event_t,
) -> *mut xcb_generic_event_t {
    let event = wait_substituted(conn, wait_real);
    track_window(event);
    event
}

/// One event from the queue, else the next real one that passes the filter.
fn poll_substituted(
    conn: *mut xcb_connection_t,
    poll_real: impl Fn() -> *mut xcb_generic_event_t,
) -> *mut xcb_generic_event_t {
    if !super::is_controlled() {
        return poll_real();
    }
    let queued = queue::pop(conn);
    if !queued.is_null() {
        return queued;
    }
    next_unfiltered(poll_real)
}

/// The real wait cannot be used: it neither returns virtual events nor skips filtered ones.
fn wait_substituted(
    conn: *mut xcb_connection_t,
    wait_real: impl Fn() -> *mut xcb_generic_event_t,
) -> *mut xcb_generic_event_t {
    if !super::is_controlled() {
        return wait_real();
    }
    loop {
        let event = poll_substituted(conn, || real_poll_for_event(conn));
        if !event.is_null() {
            return event;
        }
        // the real wait reports a broken connection as a null event too
        if connection_has_error(conn) {
            return std::ptr::null_mut();
        }
        park(conn);
    }
}

/// Drops and frees real events until one passes the filter or none is left.
fn next_unfiltered(poll_real: impl Fn() -> *mut xcb_generic_event_t) -> *mut xcb_generic_event_t {
    loop {
        let event = poll_real();
        // SAFETY: libxcb returned the event.
        let Some(view) = (unsafe { XcbEvent::from_ptr(event) }) else {
            return event;
        };
        if !INPUT_EVENTS.contains(&view.code()) {
            return event;
        }
        // SAFETY: libxcb returns malloc'd events owned by the caller.
        unsafe { libc::free(event.cast()) };
    }
}

/// Windows created past the create hooks, as by dlopen'd X stacks, still show up in the
/// notifications the game selected.
fn track_window(event: *mut xcb_generic_event_t) {
    // SAFETY: libxcb or the virtual queue returned the event.
    let Some(view) = (unsafe { XcbEvent::from_ptr(event) }) else {
        return;
    };
    if !matches!(
        view.code(),
        MAP_NOTIFY_EVENT | UNMAP_NOTIFY_EVENT | DESTROY_NOTIFY_EVENT
    ) {
        return;
    }
    match view.parse() {
        Ok(Event::MapNotify(e)) => super::track_map(e.window),
        Ok(Event::UnmapNotify(e)) => super::track_unmap(e.window),
        Ok(Event::DestroyNotify(e)) => super::track_destroy(e.window),
        Ok(other) => {
            abort!(Hooks, "input feed: window notify parsed as {other:?}");
        }
        Err(e) => {
            abort!(Hooks, "input feed: parse a window notify: {e:?}");
        }
    }
}

/// Blocks until the connection or the virtual queue has something.
fn park(conn: *mut xcb_connection_t) {
    // SAFETY: xcb's fd of the live connection, open as long as the connection.
    let conn_fd = unsafe { BorrowedFd::borrow_raw(connection_fd(conn)) };
    let notify = queue::notify_fd(conn);
    let mut polls = [
        PollFd::new(&conn_fd, PollFlags::IN),
        PollFd::new(&notify, PollFlags::IN),
    ];
    match crate::hooks::run_native(|| rustix::event::poll(&mut polls, None)) {
        Ok(_) | Err(Errno::INTR) => {}
        Err(e) => {
            abort!(Hooks, "input feed: poll the xcb connection: {e}");
        }
    }
    if polls[1].revents().contains(PollFlags::IN) {
        let mut count = [0u8; 8];
        if let Err(e) = rustix::io::read(notify.as_fd(), &mut count) {
            abort!(Hooks, "input feed: drain the notify eventfd: {e}");
        }
    }
}

fn connection_fd(conn: *mut xcb_connection_t) -> c_int {
    // SAFETY: conn is a live game connection.
    unsafe { xcb_get_file_descriptor(conn) }
}

fn connection_has_error(conn: *mut xcb_connection_t) -> bool {
    // SAFETY: conn is a live game connection.
    unsafe { xcb_connection_has_error(conn) != 0 }
}

/// libxcb's own `xcb_poll_for_event`, past the hook.
fn real_poll_for_event(conn: *mut xcb_connection_t) -> *mut xcb_generic_event_t {
    type Fn = unsafe extern "C" fn(*mut xcb_connection_t) -> *mut xcb_generic_event_t;
    static SYM: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
    let Some(f) = link_function(&SYM, c"xcb_poll_for_event".as_ptr().cast(), LIB_XCB) else {
        abort!(Hooks, "input feed: libxcb has no xcb_poll_for_event");
    };
    // SAFETY: the symbol is libxcb's xcb_poll_for_event.
    unsafe { std::mem::transmute::<*mut c_void, Fn>(f)(conn) }
}
