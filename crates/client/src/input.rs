// TODO(ai-review): generated, not yet audited
//! Game-window hotkeys on a separate X connection.
//!
//! Registers KEY_PRESS/KEY_RELEASE on the target's top-level window, so the
//! X server delivers a parallel copy of its key events to us while the game
//! keeps receiving its own (no grabbing). Keycodes are translated to keysyms
//! via GetKeyboardMapping, so hotkeys are layout-independent. This mirrors
//! libTAS, which detects hotkeys in its program process the same way
//! (`GameEventsXcb`), except we poll lazily instead of per-frame.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    ChangeWindowAttributesAux, ConnectionExt, EventMask, KeyButMask, MapState, Window,
};
use x11rb::rust_connection::RustConnection;

/// Actions triggered by game-window hotkeys. Extend as needed; the module
/// itself stays generic over this.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotkeyAction {
    /// Create a savestate in the given slot: Shift+F<N>.
    Save(u32),
    /// Load the savestate in the given slot: F<N>.
    Load(u32),
}

/// Events emitted by the hotkey listener. The listener thread itself
/// never prints — TUI mode has no stderr to spare — everything goes
/// through this callback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HotkeyEvent {
    /// Attached to the game window after (re)discovery.
    Attached(Window),
    /// A hotkey fired.
    Action(HotkeyAction),
    /// Unrecoverable error; the listener thread has exited.
    Fatal(String),
}

/// First X function-key keysym (XK_F1); F<N> maps to slot N.
const XK_F1: u32 = 0xffbe;
const F_KEY_COUNT: u32 = 15;

fn slot_from_keysym(ks: u32) -> Option<u32> {
    if ks >= XK_F1 && ks - XK_F1 < F_KEY_COUNT {
        Some(ks - XK_F1 + 1)
    } else {
        None
    }
}

/// Poll interval while watching for key events.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// Poll interval while waiting for the game window to (re)appear.
const DISCOVER_INTERVAL: Duration = Duration::from_millis(200);

/// Spawn the hotkey listener thread. It attaches to the game window the lib
/// reports, selects key events on it, and calls `on_event` for attachment
/// status, hotkey presses (auto-repeat suppressed) and fatal errors.
///
/// The thread exits on unrecoverable X errors; hotkeys are a convenience,
/// not a guarantee.
pub fn spawn_thread<F>(game_window: GameWindow, on_event: F)
where
    F: Fn(HotkeyEvent) + Send + 'static,
{
    thread::Builder::new()
        .name("game-hotkeys".into())
        .spawn(move || {
            if let Err(e) = listen(&game_window, &on_event) {
                on_event(HotkeyEvent::Fatal(format!("{e:#}")));
            }
        })
        .expect("spawn hotkey thread");
}

fn listen<F>(game_window: &AtomicU32, on_event: &F) -> Result<()>
where
    F: Fn(HotkeyEvent),
{
    let (conn, _) = x11rb::connect(None).context("connect to X")?;
    let keymap = Keymap::load(&conn)?;

    // keycode -> currently held, so auto-repeat fires once per press.
    let mut held: Vec<u8> = Vec::new();

    // Discovery loop: (re)attach after the window is created or destroyed
    // (fullscreen switches recreate windows).
    'discover: loop {
        let window = loop {
            match viewable_game_window(&conn, game_window) {
                Some(w) => break w,
                None => thread::sleep(DISCOVER_INTERVAL),
            }
        };
        on_event(HotkeyEvent::Attached(window));

        let mask = ChangeWindowAttributesAux::new().event_mask(
            EventMask::KEY_PRESS | EventMask::KEY_RELEASE | EventMask::STRUCTURE_NOTIFY,
        );
        let attach = conn.change_window_attributes(window, &mask);
        let attach = match attach {
            Err(_) => return Ok(()), // connection died
            Ok(cookie) => cookie.check(),
        };
        if attach.is_err() {
            thread::sleep(DISCOVER_INTERVAL);
            continue 'discover;
        }

        loop {
            let event = match conn.poll_for_event() {
                Ok(e) => e,
                Err(_) => return Ok(()), // connection died; give up quietly
            };
            let Some(event) = event else {
                thread::sleep(POLL_INTERVAL);
                continue;
            };
            match event {
                x11rb::protocol::Event::KeyPress(e) => {
                    if let Some(action) = keymap.action(e.detail, e.state, &mut held) {
                        on_event(HotkeyEvent::Action(action));
                    }
                }
                x11rb::protocol::Event::KeyRelease(e) => keymap.release(e.detail, &mut held),
                x11rb::protocol::Event::DestroyNotify(_) => continue 'discover,
                _ => {}
            }
        }
    }
}

/// The game's current top-level X window as reported at frame boundaries; 0 while unknown.
pub type GameWindow = Arc<AtomicU32>;

/// The reported game window once it is viewable. A dead connection or a window that vanished
/// yields None.
pub(crate) fn viewable_game_window(
    conn: &RustConnection,
    game_window: &AtomicU32,
) -> Option<Window> {
    let window = game_window.load(Ordering::Relaxed);
    if window == 0 {
        return None;
    }
    let attrs = conn.get_window_attributes(window).ok()?.reply().ok()?;
    (attrs.map_state == MapState::VIEWABLE).then_some(window)
}

/// keycode -> keysyms table built from GetKeyboardMapping.
struct Keymap {
    first_keycode: u8,
    width: usize,
    keysyms: Vec<u32>,
}

impl Keymap {
    fn load(conn: &RustConnection) -> Result<Self> {
        let setup = conn.setup();
        let min = setup.min_keycode;
        let max = setup.max_keycode;
        let reply = conn
            .get_keyboard_mapping(min, max - min + 1)
            .context("get keyboard mapping request")?
            .reply()
            .context("get keyboard mapping")?;
        Ok(Self {
            first_keycode: min,
            width: reply.keysyms_per_keycode as usize,
            keysyms: reply.keysyms,
        })
    }

    /// The action if `keycode` maps to an F-key, marking it held; None
    /// while already held (auto-repeat) or not an F-key. Shift decides
    /// between save and load.
    fn action(&self, keycode: u8, state: KeyButMask, held: &mut Vec<u8>) -> Option<HotkeyAction> {
        let slot = self.keysyms(keycode).find_map(slot_from_keysym)?;
        if held.contains(&keycode) {
            return None; // auto-repeat
        }
        held.push(keycode);
        let save = state.contains(KeyButMask::SHIFT);
        Some(if save {
            HotkeyAction::Save(slot)
        } else {
            HotkeyAction::Load(slot)
        })
    }

    fn release(&self, keycode: u8, held: &mut Vec<u8>) {
        held.retain(|kc| *kc != keycode);
    }

    fn keysyms(&self, keycode: u8) -> impl Iterator<Item = u32> + '_ {
        let base = (keycode as usize - self.first_keycode as usize) * self.width;
        self.keysyms
            .get(base..base + self.width)
            .into_iter()
            .flatten()
            .copied()
    }
}
