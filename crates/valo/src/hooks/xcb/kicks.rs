// TODO(ai-review): generated, not yet audited
//! Unblock WSI special-event queues whose consumers are parked across a
//! restore (see `kick_stale_present_queues`).

use std::sync::Mutex;
use std::sync::atomic::AtomicU64;

use super::{GAME_XCB_CONNS, xcb_connection_t};

/// (connection, window, event_id) of every PresentSelectInput registration.
/// After a restore, these pairs let us request a real Present event from the
/// X server for queues whose consumer is parked (see kick_stale_present_queues).
static PRESENT_SELECT_INPUTS: Mutex<Vec<(usize, u32, u32)>> = Mutex::new(Vec::new());

/// Kick budget armed by the swapchain-destroy hook and consumed by the
/// pthread_join hook. The destroy writes its invalidated swapchain status as
/// its first steps and only then joins the (possibly parked) event manager —
/// so once a join runs under this budget, one injection lands after the
/// status write, which is the only one that unblocks the join. Kicking at
/// destroy *entry* races the status write: the manager would consume the
/// event early, see status >= 0, and re-park.
static PENDING_KICKS: AtomicU64 = AtomicU64::new(0);

/// Arm kicks that fire on subsequent pthread_join calls.
pub fn schedule_present_kicks(ticks: u64) {
    PENDING_KICKS.fetch_add(ticks, std::sync::atomic::Ordering::SeqCst);
}

/// Called from the pthread_join hook; consumes one kick budget unit and
/// kicks while one is available.
pub(crate) fn consume_pending_kick() {
    let mut budget = PENDING_KICKS.load(std::sync::atomic::Ordering::SeqCst);
    loop {
        if budget == 0 {
            return;
        }
        match PENDING_KICKS.compare_exchange_weak(
            budget,
            budget - 1,
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
        ) {
            Ok(_) => break,
            Err(current) => budget = current,
        }
    }
    kick_stale_present_queues();
}

/// Called from the xcb_present_select_input hook: remember the registration
/// so kicks can find its queue later.
pub fn record_select_input(conn: *mut xcb_connection_t, window: u32, eid: u32) {
    if let Ok(mut inputs) = PRESENT_SELECT_INPUTS.lock() {
        inputs.push((conn as usize, window, eid));
    }
}

/// Wake every PresentSelectInput queue whose consumer may be parked. After a
/// restore, a swapchain's event manager can be parked in
/// `xcb_wait_for_special_event` on the promise of events whose originals the
/// discarded timeline already consumed — the swapchain destroy then hangs
/// joining that manager, because the manager only re-checks the (invalidated)
/// swapchain status after the wait returns. Injecting a minimal event into
/// the parked queue makes the manager return once; it then sees the
/// invalidated status and lets the join proceed.
pub fn kick_stale_present_queues() {
    let Ok(inputs) = PRESENT_SELECT_INPUTS.lock() else {
        return;
    };
    for (conn, _window, eid) in inputs.iter() {
        let conn = *conn as *mut xcb_connection_t;
        if !GAME_XCB_CONNS
            .iter()
            .any(|tracked| tracked == conn as *mut ())
        {
            continue;
        }
        crate::trace!(Hooks, "kick: inject special event conn={conn:p} eid={eid}");
        // SAFETY: conn is tracked and live; eid comes from the app's own
        // PresentSelectInput call. Runs in normal (non-signal) context, which
        // the C helper requires because it allocates.
        unsafe { valo_xcb_kick_special_event(conn, *eid) };
    }
}

unsafe extern "C" {
    /// # Safety
    /// `conn` must be a live xcb connection with no concurrent access beyond
    /// libxcb's own iolock; `eid` must be an id from a tracked
    /// PresentSelectInput registration. Allocates — normal context only.
    fn valo_xcb_kick_special_event(conn: *mut xcb_connection_t, eid: u32);
}
