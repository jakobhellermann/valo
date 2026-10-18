// TODO(ai-review): generated, not yet audited
//! Scripted keyboard input: the client's per-frame keyboard state, diffed into xcb key events
//! on every game connection.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use valo_ipc::config::InputConfig;
use valo_ipc::messages::InputState;
use x11rb_protocol::protocol::xproto::{
    FOCUS_IN_EVENT, FocusInEvent, KEY_PRESS_EVENT, KEY_RELEASE_EVENT, KeyButMask, KeyPressEvent,
    NotifyDetail, NotifyMode,
};

use crate::abort;
use crate::hooks::xcb::{GAME_XCB_CONNS, xcb_connection_t, xcb_generic_event_t};

pub mod queue;
mod xcb;

pub use xcb::{poll_event, wait_event};

static UNCONTROLLED: AtomicBool = AtomicBool::new(false);

static FEED: Mutex<Feed> = Mutex::new(Feed {
    pending: None,
    fed: None,
    windows: VecDeque::new(),
});

struct Feed {
    /// The client's latest state.
    pending: Option<InputState>,
    /// The state the game has seen events for.
    fed: Option<InputState>,
    /// Top-level windows of the game, most recently mapped first.
    windows: VecDeque<u32>,
}

pub fn set_config(config: &InputConfig) {
    UNCONTROLLED.store(config.uncontrolled_input, Ordering::Relaxed);
}

pub fn config() -> InputConfig {
    InputConfig {
        uncontrolled_input: UNCONTROLLED.load(Ordering::Relaxed),
    }
}

pub fn is_controlled() -> bool {
    !UNCONTROLLED.load(Ordering::Relaxed)
}

pub fn set_state(state: InputState) {
    FEED.lock().unwrap().pending = Some(state);
}

pub fn track_create(window: u32) {
    FEED.lock().unwrap().windows.push_back(window);
}

pub fn track_map(window: u32) {
    let windows = &mut FEED.lock().unwrap().windows;
    windows.retain(|&w| w != window);
    windows.push_front(window);
}

pub fn track_unmap(window: u32) {
    let windows = &mut FEED.lock().unwrap().windows;
    windows.retain(|&w| w != window);
    windows.push_back(window);
}

pub fn track_destroy(window: u32) {
    FEED.lock().unwrap().windows.retain(|&w| w != window);
}

pub fn current_window() -> Option<u32> {
    FEED.lock().unwrap().windows.front().copied()
}

/// Queues the events for the client's latest state; waits for a game window to address them to.
pub fn feed_frame() {
    if !is_controlled() {
        return;
    }
    let mut feed = FEED.lock().unwrap();
    let (Some(state), Some(&window)) = (feed.pending, feed.windows.front()) else {
        return;
    };
    let first = feed.fed.is_none();
    let changes = diff_events(feed.fed.as_ref(), &state);
    feed.fed = Some(state);
    drop(feed);

    let clock = &crate::determinism::time::CLOCK;
    let now_ns = clock
        .now_ns(libc::CLOCK_MONOTONIC)
        .unwrap_or_else(|| crate::determinism::time::real_monotonic().as_nanos() as u64);
    let time = (now_ns / 1_000_000) as u32;
    for conn in GAME_XCB_CONNS.iter() {
        let conn = conn as *mut xcb_connection_t;
        // Real focus events are filtered too: without one FocusIn the game ignores the keys.
        if first {
            queue::push_back(conn, malloc_event(focus_in_event(window)));
        }
        // the game frees every event it receives, so each connection gets its own copy
        for change in &changes {
            queue::push_back(
                conn,
                malloc_event(key_event(*change, window, state.mods, time)),
            );
        }
    }
}

/// Keycode changes between two states, releases before presses.
fn diff_events(fed: Option<&InputState>, next: &InputState) -> Vec<KeyChange> {
    let (mut releases, mut presses) = (Vec::new(), Vec::new());
    for keycode in 0..=u8::MAX {
        let held = is_held(next, keycode);
        if held == fed.is_some_and(|fed| is_held(fed, keycode)) {
            continue;
        }
        let change = KeyChange {
            keycode,
            pressed: held,
        };
        if held {
            presses.push(change);
        } else {
            releases.push(change);
        }
    }
    releases.append(&mut presses);
    releases
}

fn is_held(state: &InputState, keycode: u8) -> bool {
    state.keys[keycode as usize / 8] & (1 << (keycode % 8)) != 0
}

#[derive(Clone, Copy)]
struct KeyChange {
    keycode: u8,
    pressed: bool,
}

fn focus_in_event(window: u32) -> [u8; 32] {
    <[u8; 32]>::from(FocusInEvent {
        response_type: FOCUS_IN_EVENT,
        detail: NotifyDetail::NONLINEAR,
        sequence: 0,
        event: window,
        mode: NotifyMode::NORMAL,
    })
}

fn key_event(change: KeyChange, window: u32, mods: u16, time: u32) -> [u8; 32] {
    <[u8; 32]>::from(KeyPressEvent {
        response_type: if change.pressed {
            KEY_PRESS_EVENT
        } else {
            KEY_RELEASE_EVENT
        },
        detail: change.keycode,
        sequence: 0,
        time,
        root: 0,
        event: window,
        child: 0,
        root_x: 0,
        root_y: 0,
        event_x: 0,
        event_y: 0,
        state: KeyButMask::from(mods),
        same_screen: true,
    })
}

/// The event as libxcb hands it out: the wire bytes, then a zero `full_sequence`, which Xlib's
/// xcb glue accepts for events it never requested. The game frees it with `free()`.
fn malloc_event(wire: [u8; 32]) -> *mut xcb_generic_event_t {
    // SAFETY: calloc has no preconditions; the zeroed tail is the `full_sequence`.
    let p = unsafe { libc::calloc(1, wire.len() + size_of::<u32>()) }.cast::<u8>();
    if p.is_null() {
        abort!(Hooks, "input feed: out of memory for an xcb event");
    }
    // SAFETY: `p` is a fresh allocation larger than `wire`.
    unsafe { p.copy_from_nonoverlapping(wire.as_ptr(), wire.len()) };
    p as *mut xcb_generic_event_t
}
