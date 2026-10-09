// TODO(ai-review): generated, not yet audited
//! Per-connection queues of virtual events, served before the game's real xcb events. Each queue
//! has an eventfd that a push writes, so a waiter parked in `poll()` wakes up.

use std::collections::{HashMap, VecDeque};
use std::os::fd::OwnedFd;
use std::sync::{LazyLock, Mutex};

use crate::abort;
use crate::hooks::xcb::{xcb_connection_t, xcb_generic_event_t};

struct EventPtr(*mut xcb_generic_event_t);
// SAFETY: a libc heap allocation, not tied to a thread.
unsafe impl Send for EventPtr {}

struct ConnQueue {
    events: VecDeque<EventPtr>,
    notify: OwnedFd,
}

type ConnectionAddress = usize;

static QUEUES: LazyLock<Mutex<HashMap<ConnectionAddress, ConnQueue>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Takes ownership of the malloc'd `event`.
pub fn push_back(conn: *mut xcb_connection_t, event: *mut xcb_generic_event_t) {
    let mut queues = QUEUES.lock().unwrap();
    let queue = queues
        .entry(conn as ConnectionAddress)
        .or_insert_with(ConnQueue::new);
    queue.events.push_back(EventPtr(event));
    if let Err(e) = rustix::io::write(&queue.notify, &1u64.to_ne_bytes()) {
        abort!(Hooks, "input feed: wake the xcb event waiter: {e}");
    }
}

/// The oldest virtual event, or null.
pub fn pop(conn: *mut xcb_connection_t) -> *mut xcb_generic_event_t {
    QUEUES
        .lock()
        .unwrap()
        .get_mut(&(conn as ConnectionAddress))
        .and_then(|queue| queue.events.pop_front())
        .map_or(std::ptr::null_mut(), |event| event.0)
}

/// A duplicate of the connection's notify eventfd.
pub fn notify_fd(conn: *mut xcb_connection_t) -> OwnedFd {
    let mut queues = QUEUES.lock().unwrap();
    let queue = queues
        .entry(conn as ConnectionAddress)
        .or_insert_with(ConnQueue::new);
    match queue.notify.try_clone() {
        Ok(fd) => fd,
        Err(e) => {
            abort!(Hooks, "input feed: duplicate the notify eventfd: {e}");
        }
    }
}

/// Drops the connection's queue and frees its undelivered events.
pub fn remove(conn: *mut xcb_connection_t) {
    let Some(queue) = QUEUES.lock().unwrap().remove(&(conn as ConnectionAddress)) else {
        return;
    };
    for event in queue.events {
        // SAFETY: queued events are malloc'd and owned by the queue.
        unsafe { libc::free(event.0.cast()) };
    }
}

impl ConnQueue {
    fn new() -> Self {
        match rustix::event::eventfd(0, rustix::event::EventfdFlags::CLOEXEC) {
            Ok(notify) => Self {
                events: VecDeque::new(),
                notify,
            },
            Err(e) => {
                abort!(Hooks, "input feed: create a notify eventfd: {e}");
            }
        }
    }
}
