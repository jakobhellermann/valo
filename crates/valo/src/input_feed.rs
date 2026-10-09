//! Scripted keyboard feeding into the game's xcb event stream.
//!
//! The full keyboard state arrives per frame boundary (`SetInputs`); the
//! diff against the last fed state becomes xcb KeyPress/KeyRelease events
//! on every tracked connection.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::Ordering;

use valo_ipc::messages::InputState;

use crate::global_state::GlobalState;
use crate::hooks::xcb::{GAME_XCB_CONNS, queue, xcb_connection_t};

/// Frame-boundary-local feeding state: the state set by the IPC turn and
/// the diff base of the last fed frame. Only touched from the frame
/// boundary chain (set_state during the IPC turn, feed_frame after it),
/// on one thread in order — the Mutex only makes the static Sync.
static FEED: Mutex<FeedState> = Mutex::new(FeedState {
    pending: None,
    prev: None,
});

struct FeedState {
    /// Latest client-set state; None until the first SetInputs.
    pending: Option<InputState>,
    /// State as of the last fed frame (diff base).
    prev: Option<InputState>,
}

/// Top-level windows of the game, most recently mapped first; events are
/// addressed to the front.
static WINDOWS: Mutex<VecDeque<u32>> = Mutex::new(VecDeque::new());

/// Scripted input mode active (Config::input.uncontrolled_input off).
pub(crate) fn enabled() -> bool {
    !GlobalState::get()
        .uncontrolled_input
        .load(Ordering::Relaxed)
}

/// Store a client-sent state; fed at the next frame boundary.
pub fn set_state(state: InputState) {
    FEED.lock().unwrap().pending = Some(state);
}

/// Track a newly created top-level window (parent is the root window).
pub(crate) fn track_create(window: u32) {
    WINDOWS.lock().unwrap().push_back(window);
    crate::debug!(Hooks, "input feed: tracking game window {window:#x}");
}

/// Make the window the current one (mapped, most recently first). Windows
/// first seen in a MapNotify start out current.
pub(crate) fn track_promote(window: u32) {
    let mut windows = WINDOWS.lock().unwrap();
    if let Some(pos) = windows.iter().position(|w| *w == window) {
        windows.remove(pos);
    } else {
        crate::debug!(Hooks, "input feed: window {window:#x} became current");
    }
    windows.push_front(window);
}

/// Forget a destroyed window; the next remaining one becomes current.
pub(crate) fn track_destroy(window: u32) {
    let mut windows = WINDOWS.lock().unwrap();
    if let Some(pos) = windows.iter().position(|w| *w == window) {
        windows.remove(pos);
    }
}

fn current_window() -> u32 {
    WINDOWS.lock().unwrap().front().copied().unwrap_or(0)
}

/// Frame boundary, after the IPC turn: synthesize events for the state
/// change and queue them on every tracked connection. Statics are process
/// memory, so a restore rolls the feed state back together with the game.
/// Without a game window nothing is committed — the diff catches up at
/// the first windowed feed.
pub(crate) fn feed_frame() {
    if !enabled() {
        return;
    }
    let (state, changes) = {
        let feed = FEED.lock().unwrap();
        let Some(state) = feed.pending else {
            return;
        };
        let changes = diff_events(feed.prev.as_ref(), &state);
        (state, changes)
    };
    let window = current_window();
    if window == 0 {
        crate::trace!(Hooks, "input feed: no game window, skipping");
        return;
    }
    let prev_was_none = FEED.lock().unwrap().prev.is_none();
    FEED.lock().unwrap().prev = Some(state);
    let first_feed = prev_was_none;
    if changes.is_empty() && !first_feed {
        return;
    }

    let time = crate::determinism::time::virtual_monotonic_ms() as u32;
    for conn in GAME_XCB_CONNS.iter() {
        let conn = conn as *mut xcb_connection_t;
        // The controlled mode also filters real FocusIn, so the game would
        // never consider itself focused and drop the fed keys. One synthetic
        // FocusIn per window targets the game that stays focused for its
        // whole scripted life (real FocusOut is filtered too).
        if first_feed {
            let raw = malloc_event(&build_focus_in_event(window));
            if !raw.is_null() {
                queue::push_back(conn, raw);
            }
        }
        for change in &changes {
            // Each connection needs its own copy: the game frees every
            // returned event.
            let raw = malloc_event(&build_key_event(*change, window, state.mods, time));
            if !raw.is_null() {
                queue::push_back(conn, raw);
            }
        }
    }
    crate::debug!(
        Hooks,
        "input feed: {} events queued on {} connections",
        changes.len() + first_feed as usize,
        GAME_XCB_CONNS.len()
    );
}

/// Keycode changes between two states, releases before presses.
fn diff_events(prev: Option<&InputState>, next: &InputState) -> Vec<KeyChange> {
    let (mut releases, mut presses) = (Vec::new(), Vec::new());
    for keycode in 0..=u8::MAX {
        let held = keycode_held(next, keycode);
        let was_held = prev.is_some_and(|p| keycode_held(p, keycode));
        if held != was_held {
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
    }
    releases.append(&mut presses);
    releases
}

fn keycode_held(state: &InputState, keycode: u8) -> bool {
    state.keys[keycode as usize / 8] & (1 << (keycode % 8)) != 0
}

#[derive(Clone, Copy)]
struct KeyChange {
    keycode: u8,
    pressed: bool,
}

/// xcb key event layout (36 bytes: 32 on the wire plus the trailing
/// full_sequence field). The full_sequence field is what libX11's xcb glue
/// reads to attribute the event to a request window — a bogus value makes
/// it abort with "Unknown sequence number while processing queue". Zero
/// passes the window check, like libTAS's generated events.
#[repr(C)]
struct KeyEvent {
    response_type: u8,
    /// keycode
    detail: u8,
    sequence: u16,
    time: u32,
    root: u32,
    /// window the event is delivered to
    event: u32,
    child: u32,
    root_x: i16,
    root_y: i16,
    event_x: i16,
    event_y: i16,
    /// modifier mask
    state: u16,
    same_screen: u8,
    pad0: u8,
    full_sequence: u32,
}

/// xcb focus event layout (36 bytes: 32 on the wire plus the trailing
/// full_sequence field, see [`KeyEvent`]).
#[repr(C)]
struct FocusEvent {
    response_type: u8,
    /// NotifyNonlinear: focus granted from outside the window hierarchy,
    /// as a WM does for a top-level window.
    detail: u8,
    sequence: u16,
    event: u32,
    /// NotifyNormal.
    mode: u8,
    pad0: [u8; 3],
    pad1: [u8; 20],
    full_sequence: u32,
}

fn build_focus_in_event(window: u32) -> FocusEvent {
    FocusEvent {
        response_type: 9,
        detail: 2,
        sequence: 0,
        event: window,
        mode: 0,
        pad0: [0; 3],
        pad1: [0; 20],
        full_sequence: 0,
    }
}

fn build_key_event(change: KeyChange, window: u32, mods: u16, time: u32) -> KeyEvent {
    KeyEvent {
        response_type: if change.pressed { 2 } else { 3 },
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
        state: mods,
        same_screen: 1,
        pad0: 0,
        full_sequence: 0,
    }
}

/// Copies the event into a fresh libc-malloc'd buffer. The queue (and later
/// the game, which `free()`s every returned event) takes ownership.
fn malloc_event<T>(event: &T) -> *mut crate::hooks::xcb::xcb_generic_event_t {
    debug_assert_eq!(std::mem::size_of::<T>(), 36);
    // SAFETY: see doc comment — the allocation is handed to the game, which
    // frees it with free(), so it must come from libc's allocator.
    unsafe {
        let p = libc::malloc(std::mem::size_of::<T>()).cast::<u8>();
        if !p.is_null() {
            std::ptr::copy_nonoverlapping(event as *const T as *const u8, p, 36);
        }
        p as *mut crate::hooks::xcb::xcb_generic_event_t
    }
}
