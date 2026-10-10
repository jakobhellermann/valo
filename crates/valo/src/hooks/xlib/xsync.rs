//! X display quiescence around thread freezes.
//!
//! Freezing a thread inside an Xlib/xcb call leaves the connection in an
//! inconsistent state: the thread may hold the connection lock, have a
//! request half-written, or sit on a partially read reply — the classes of
//! savestate failures libTAS documents (the frozen-thread-softlock and the
//! sequence-number corruption). Like libTAS, the freeze therefore runs
//! only after every game display was locked and synced: the lock keeps
//! threads out of new Xlib calls, the sync drains every request and reply,
//! so nothing X-related is in flight or mid-call when threads suspend.
//!
//! The lock is released right after the freeze, before any memory is
//! written: no Xlib lock state is carried into the snapshot, so a restore
//! rewinds to consistent, unlocked locks. A restore itself takes the lock
//! the same way but never releases it — the lock owner's timeline is
//! discarded by the rewind, and the rewound locks are already unlocked.
//!
//! Raw xcb connections without a Display are not covered by the Xlib lock;
//! their restore-side handling is the connection fixup.

use std::ffi::{c_char, c_void};
use std::os::raw::c_int;
use std::sync::atomic::AtomicPtr;

use super::xdisplay::GAME_DISPLAYS;
use crate::hooks::{link_function, run_native};
use crate::{abort, debug};

static XLOCKDISPLAY: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static XSYNC: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static XUNLOCKDISPLAY: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

const XLOCKDISPLAY_NAME: &[u8] = b"XLockDisplay\0";
const XSYNC_NAME: &[u8] = b"XSync\0";
const XUNLOCKDISPLAY_NAME: &[u8] = b"XUnlockDisplay\0";

/// `XLockDisplay(Display*)`
unsafe fn lock(display: *mut ()) {
    let f = link_function(
        &XLOCKDISPLAY,
        XLOCKDISPLAY_NAME.as_ptr().cast::<c_char>(),
        std::ptr::null(),
    );
    if f.is_none() {
        abort!(Hooks, "XLockDisplay not resolvable");
    }
    let f: unsafe extern "C" fn(*mut ()) = unsafe { std::mem::transmute(f.unwrap()) };
    // SAFETY: the resolved symbol is the real libX11 XLockDisplay, and the
    // display is live.
    run_native(|| unsafe { f(display) });
}

/// `XSync(Display*, discard = false)` — flushes every request and waits for
/// every reply; queued events are kept.
unsafe fn sync(display: *mut ()) {
    let f = link_function(
        &XSYNC,
        XSYNC_NAME.as_ptr().cast::<c_char>(),
        std::ptr::null(),
    );
    if f.is_none() {
        abort!(Hooks, "XSync not resolvable");
    }
    let f: unsafe extern "C" fn(*mut (), c_int) -> c_int =
        unsafe { std::mem::transmute(f.unwrap()) };
    // SAFETY: the resolved symbol is the real libX11 XSync, and the display
    // is live.
    run_native(|| unsafe { f(display, 0) });
}

/// `XUnlockDisplay(Display*)`
unsafe fn unlock(display: *mut ()) {
    let f = link_function(
        &XUNLOCKDISPLAY,
        XUNLOCKDISPLAY_NAME.as_ptr().cast::<c_char>(),
        std::ptr::null(),
    );
    if f.is_none() {
        abort!(Hooks, "XUnlockDisplay not resolvable");
    }
    let f: unsafe extern "C" fn(*mut ()) = unsafe { std::mem::transmute(f.unwrap()) };
    // SAFETY: the resolved symbol is the real libX11 XUnlockDisplay, and
    // this thread holds the display's lock.
    run_native(|| unsafe { f(display) });
}

/// Locks and syncs every game display. Blocks until threads currently
/// inside Xlib calls finish them; new ones queue on the lock. Must run on
/// the main thread before threads are suspended.
pub(crate) fn lock_and_sync_displays() {
    for display in GAME_DISPLAYS.iter() {
        debug!(Hooks, "locking and syncing display {display:?}");
        // SAFETY: displays in the registry are live Display pointers opened
        // by the game, and this runs single-threaded before the freeze.
        unsafe {
            lock(display);
            sync(display);
        }
    }
}

/// Releases the display locks taken by [`lock_and_sync_displays`], after
/// the freeze and before any memory is written.
pub(crate) fn unlock_displays() {
    for display in GAME_DISPLAYS.iter() {
        // SAFETY: this thread holds the lock on every registry display from
        // the matching lock_and_sync_displays call.
        unsafe { unlock(display) };
    }
}
