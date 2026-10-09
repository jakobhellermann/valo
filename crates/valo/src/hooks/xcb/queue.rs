//! Lib-owned event queues substituting the game's xcb event reads.
//!
//! Fed events are malloc'd here (the game `free()`s everything returned by
//! `xcb_poll_for_event` and friends — libxcb contract), pushed per connection,
//! and served by the event-entry hooks before anything real. Waiters park in
//! `poll()` on the connection fd plus a per-queue eventfd so a push from
//! another thread (the frame boundary) wakes them without spinning.

use std::collections::{HashMap, VecDeque};
use std::ffi::c_void;
use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd};
use std::os::raw::c_int;
use std::sync::atomic::AtomicPtr;
use std::sync::{LazyLock, Mutex};

use super::{LIB_XCB, xcb_connection_t, xcb_generic_event_t};
use crate::hooks::link_function;

unsafe extern "C" {
    fn xcb_get_file_descriptor(conn: *mut xcb_connection_t) -> c_int;
    fn xcb_connection_has_error(conn: *mut xcb_connection_t) -> i32;
}

/// Per-connection fed-event backlog. The event pointers are libc heap
/// addresses without thread affinity; the wrapper carries them across the
/// mutex.
struct EventPtr(*mut xcb_generic_event_t);
// SAFETY: plain heap allocation addresses.
unsafe impl Send for EventPtr {}

struct ConnQueue {
    deque: VecDeque<EventPtr>,
    /// eventfd(0): written on push, polled by waiters. None if the
    /// allocation failed (waiters then degrade to timed polling).
    notify: Option<OwnedFd>,
}

/// Queues keyed by connection address. Entries are created lazily on first
/// push/wait and dropped (with their events freed) on xcb_disconnect.
static QUEUES: LazyLock<Mutex<HashMap<usize, ConnQueue>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn lock_queues() -> std::sync::MutexGuard<'static, HashMap<usize, ConnQueue>> {
    QUEUES.lock().unwrap_or_else(|p| {
        crate::warn!(Hooks, "queue mutex poisoned, recovering");
        p.into_inner()
    })
}

/// Push a malloc'd event to the back of the connection's queue and wake a
/// parked waiter. Ownership of the event moves into the queue.
pub(crate) fn push_back(conn: *mut xcb_connection_t, event: *mut xcb_generic_event_t) {
    let mut map = lock_queues();
    let queue = map.entry(conn as usize).or_insert_with(ConnQueue::new);
    queue.deque.push_back(EventPtr(event));
    queue.notify_waiters();
}

/// Pop the oldest fed event, or null.
pub(crate) fn pop(conn: *mut xcb_connection_t) -> *mut xcb_generic_event_t {
    lock_queues()
        .get_mut(&(conn as usize))
        .and_then(|queue| queue.deque.pop_front())
        .map_or(std::ptr::null_mut(), |e| e.0)
}

/// eventfd of the connection's queue, creating the queue if needed.
/// `-1` if the eventfd could not be created (feeding degrades to
/// poll-timeout delivery).
pub(crate) fn notify_fd(conn: *mut xcb_connection_t) -> c_int {
    lock_queues()
        .entry(conn as usize)
        .or_insert_with(ConnQueue::new)
        .notify
        .as_ref()
        .map_or(-1, |fd| fd.as_raw_fd())
}

/// Drop the connection's queue, freeing any undelivered events. Called from
/// the xcb_disconnect hook before the real disconnect.
pub(crate) fn remove(conn: *mut xcb_connection_t) {
    let Some(mut queue) = lock_queues().remove(&(conn as usize)) else {
        return;
    };
    for event in queue.deque.drain(..) {
        // SAFETY: queue events are libc-malloc'd here and owned by the queue.
        unsafe { libc::free(event.0.cast()) };
    }
}

/// Drains a woken notify fd so the next `poll()` parks again.
pub(crate) fn drain_notify(fd: c_int) {
    // SAFETY: fd comes from this connection's queue entry, which lives
    // until xcb_disconnect.
    let fd = unsafe { BorrowedFd::borrow_raw(fd) };
    let mut buf = [0u8; 8];
    let _ = rustix::io::read(fd, &mut buf);
}

impl ConnQueue {
    fn new() -> Self {
        Self {
            deque: VecDeque::new(),
            notify: rustix::event::eventfd(0, rustix::event::EventfdFlags::CLOEXEC).ok(),
        }
    }

    fn notify_waiters(&self) {
        if let Some(fd) = &self.notify {
            let _ = rustix::io::write(fd, &1u64.to_ne_bytes());
        }
    }
}

/// Not hooked — plain extern resolves against libxcb at call time.
pub(crate) fn connection_fd(conn: *mut xcb_connection_t) -> c_int {
    // SAFETY: conn is a live game connection.
    unsafe { xcb_get_file_descriptor(conn) }
}

/// Real `xcb_poll_for_event`, bypassing our own override — used by the
/// wait hook to drain the connection without blocking.
pub(crate) fn real_poll_for_event(conn: *mut xcb_connection_t) -> *mut xcb_generic_event_t {
    type Fn = unsafe extern "C" fn(*mut xcb_connection_t) -> *mut xcb_generic_event_t;
    static SYM: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
    let f = link_function(&SYM, c"xcb_poll_for_event".as_ptr().cast(), LIB_XCB);
    // SAFETY: symbol from libxcb with xcb's own signature.
    f.map(|f| unsafe { std::mem::transmute::<*mut c_void, Fn>(f)(conn) })
        .unwrap_or(std::ptr::null_mut())
}

/// Not hooked — plain extern resolves against libxcb at call time.
pub(crate) fn connection_has_error(conn: *mut xcb_connection_t) -> i32 {
    // SAFETY: conn is a live game connection.
    unsafe { xcb_connection_has_error(conn) }
}
