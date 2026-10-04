//! Save and restore X11/XCB connection state across checkpoint/restore.
//!
//! After restoring process memory, the X11 Display and XCB connection structs
//! contain stale sequence numbers from checkpoint time. The X server has moved
//! on, so we need to preserve the current (pre-restore) values and write them
//! back after restore.

use super::MAX_DISPLAYS;
use super::xdisplay::GAME_DISPLAYS;
use crate::hooks::xcb::GAME_XCB_CONNS;
use crate::{Result, debug};

include!("generated.rs");

unsafe extern "C" {
    /// # Safety
    /// `display` must be a live Display with no concurrent access.
    /// The output pointers must be writable and `dst` must be writable for
    /// `dst_len` bytes.
    /// The loaded libxcb must match the vendored layout.
    fn valo_x11_save(
        display: *mut (),
        last_request_read: *mut u64,
        request: *mut u64,
        dst: *mut u8,
        dst_len: usize,
    ) -> usize;
    /// # Safety
    /// `display` must be a live Display with no concurrent access.
    /// `src` must be readable for `src_len` bytes.
    /// The loaded libxcb must match the vendored layout.
    fn valo_x11_restore(
        display: *mut (),
        last_request_read: u64,
        request: u64,
        src: *const u8,
        src_len: usize,
    ) -> std::ffi::c_int;
    /// # Safety
    /// `conn` must be a live xcb connection with no concurrent access, and
    /// `dst` must be writable for `dst_len` bytes. The loaded libxcb must
    /// match the vendored layout.
    fn valo_xcb_save(conn: *mut (), dst: *mut u8, dst_len: usize) -> usize;
    /// # Safety
    /// `conn` must be a live xcb connection with no concurrent access, and
    /// `src` must be readable for `src_len` bytes. The loaded libxcb must
    /// match the vendored layout.
    fn valo_xcb_restore(conn: *mut (), src: *const u8, src_len: usize) -> std::ffi::c_int;
    /// # Safety
    /// `display` must be a live Display with no concurrent access; `out` must
    /// be writable. Read-only: never mutates Xlib/XCB state.
    fn valo_x11_debug_counters(display: *mut (), out: *mut XcbCounters);
    /// # Safety
    /// `conn` must be a live xcb connection or null with no concurrent access;
    /// `out` must be writable. Read-only: never mutates XCB state.
    fn valo_xcb_debug_counters(conn: *mut (), out: *mut XcbCounters);
    /// # Safety
    /// `display` must be a live Display or null with no concurrent access.
    /// Read-only getter for the backing xcb connection.
    fn valo_x11_conn(display: *mut ()) -> *mut ();
}

/// TODO(ai-review): generated, not yet audited — TEMP debug instrumentation for
/// the post-restore WSI hang; must mirror `struct valo_xcb_counters` in
/// valo_shim.c. Read-only, signal-handler-safe.
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct XcbCounters {
    has_error: u64,
    xlib_request: u64,
    xlib_last_request_read: u64,
    out_request: u64,
    out_request_written: u64,
    out_writing: u64,
    out_queue_len: u64,
    in_request_read: u64,
    in_request_completed: u64,
    in_request_expected: u64,
    in_reading: u64,
    in_queue_len: u64,
    in_events: u64,
    in_events_tail_target: u64,
    in_current_reply: u64,
    in_replies: u64,
    in_pending_replies: u64,
    in_readers: u64,
    in_special_waiters: u64,
    in_special_events: u64,
    iolock_state: u64,
    iolock_owner: u64,
}

impl std::fmt::Display for XcbCounters {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "err={} xreq={} xreq_rd={} | out.req={} wrtn={} writing={} qlen={} | \
             in.req_rd={} cmp={} exp={} reading={} qlen={} | nodes(events, tail_tgt, cur_reply, \
             replies, pend, readers, special, specials_ev)={:x} {:x} {:x} {:x} {:x} {:x} {:x} {:x} | iolock(lock,owner)={} {}",
            self.has_error,
            self.xlib_request,
            self.xlib_last_request_read,
            self.out_request,
            self.out_request_written,
            self.out_writing,
            self.out_queue_len,
            self.in_request_read,
            self.in_request_completed,
            self.in_request_expected,
            self.in_reading,
            self.in_queue_len,
            self.in_events,
            self.in_events_tail_target,
            self.in_current_reply,
            self.in_replies,
            self.in_pending_replies,
            self.in_readers,
            self.in_special_waiters,
            self.in_special_events,
            self.iolock_state,
            self.iolock_owner,
        )
    }
}

/// Log Xlib/XCB bookkeeping for every tracked display/connection. TEMP debug helper.
pub fn log_counters(when: &str) {
    for (i, conn) in GAME_XCB_CONNS.iter().enumerate() {
        let mut counters = XcbCounters::default();
        // SAFETY: tracked connections are live xcb connection pointers, and
        // this runs with all other threads suspended (no concurrent X11
        // access). `counters` is a valid writable output buffer.
        unsafe { valo_xcb_debug_counters(conn, &mut counters) };
        debug!(Hooks, "xcb-conn[{i}] {conn:p} {when}: {counters}");
    }
    for (i, display) in GAME_DISPLAYS.iter().enumerate() {
        let mut counters = XcbCounters::default();
        // SAFETY: tracked displays are live Xlib Display pointers, and restore
        // runs with all other threads suspended (no concurrent X11 access).
        // `counters` is a valid writable output buffer.
        unsafe { valo_x11_debug_counters(display, &mut counters) };
        crate::trace!(Hooks, "xcb-display[{i}] {when}: {counters}");
    }
}

struct DisplayState {
    display: *mut (),
    /// The xcb connection backing `display` — may be tracked in `GAME_XCB_CONNS`
    /// as well; connection restore must skip it (applying the io-state fixup
    /// twice would preserve frame-restore-time list heads on the second pass).
    conn: *mut (),
    last_request_read: u64,
    request: u64,
    xcb_conn_buf: [u8; XCB_CONN_BUF_SIZE],
}

struct ConnState {
    conn: *mut (),
    xcb_conn_buf: [u8; XCB_CONN_BUF_SIZE],
}

pub struct X11SavedState {
    displays: [DisplayState; MAX_DISPLAYS],
    count: usize,
    conns: [ConnState; MAX_DISPLAYS],
    conn_count: usize,
}

impl X11SavedState {
    /// Save current X11 connection state. Call BEFORE restoring memory.
    pub fn save() -> Result<Self> {
        #[allow(clippy::large_const_arrays)]
        const EMPTY: DisplayState = DisplayState {
            display: std::ptr::null_mut(),
            conn: std::ptr::null_mut(),
            last_request_read: 0,
            request: 0,
            xcb_conn_buf: [0; XCB_CONN_BUF_SIZE],
        };
        #[allow(clippy::large_const_arrays)]
        const EMPTY_CONN: ConnState = ConnState {
            conn: std::ptr::null_mut(),
            xcb_conn_buf: [0; XCB_CONN_BUF_SIZE],
        };
        let mut state = X11SavedState {
            displays: [EMPTY; MAX_DISPLAYS],
            count: 0,
            conns: [EMPTY_CONN; MAX_DISPLAYS],
            conn_count: 0,
        };

        // TODO(ai-review): generated, not yet audited — TEMP debug
        log_counters("save (live, pre-memcpy-restore)");

        for (i, display) in GAME_DISPLAYS.iter().enumerate() {
            let ds = &mut state.displays[i];
            ds.display = display;
            // SAFETY: tracked displays are live and other threads are
            // suspended; read-only getter.
            ds.conn = unsafe { valo_x11_conn(display) };
            // SAFETY: the output pointers and buffer are valid for the supplied
            // sizes. We assume tracked displays remain live and unused by other
            // threads; this still needs to be enforced by thread suspension.
            let copied = unsafe {
                valo_x11_save(
                    display,
                    &mut ds.last_request_read,
                    &mut ds.request,
                    ds.xcb_conn_buf.as_mut_ptr(),
                    ds.xcb_conn_buf.len(),
                )
            };
            if copied != XCB_CONN_BUF_SIZE {
                return Err(std::io::ErrorKind::InvalidData.into());
            }

            state.count += 1;
        }

        for (i, conn) in GAME_XCB_CONNS.iter().enumerate() {
            let cs = &mut state.conns[i];
            cs.conn = conn;
            // SAFETY: tracked connections are live, and this runs with all
            // other threads suspended (no concurrent X11 access). The buffer
            // is valid for its full length.
            let copied =
                unsafe { valo_xcb_save(conn, cs.xcb_conn_buf.as_mut_ptr(), cs.xcb_conn_buf.len()) };
            if copied != XCB_CONN_BUF_SIZE {
                return Err(std::io::ErrorKind::InvalidData.into());
            }

            state.conn_count += 1;
        }

        Ok(state)
    }

    /// Restore X11 connection state. Call AFTER restoring memory.
    pub fn restore(&self) -> Result<()> {
        // TODO: Handle displays opened or closed since the checkpoint.
        let mut count = 0;
        for (i, display) in GAME_DISPLAYS.iter().enumerate() {
            if i >= self.count || display != self.displays[i].display {
                return Err(std::io::ErrorKind::InvalidData.into());
            }
            count += 1;
        }
        if count != self.count {
            return Err(std::io::ErrorKind::InvalidData.into());
        }

        for (i, conn) in GAME_XCB_CONNS.iter().enumerate() {
            if i >= self.conn_count || conn != self.conns[i].conn {
                return Err(std::io::ErrorKind::InvalidData.into());
            }
        }

        // A connection restored through its Display must not get the io-state
        // fixup twice: the second pass would read the already-restored
        // (frame-restore-time) list heads and "preserve" those dangling
        // pointers instead of the checkpoint-time ones.
        let display_conns = self.displays.iter().take(self.count).map(|ds| ds.conn);

        for ds in self.displays.iter().take(self.count) {
            // SAFETY: the buffer is readable for the generated length and display
            // identity was checked above. We still assume no concurrent X11 use.
            let status = unsafe {
                valo_x11_restore(
                    ds.display,
                    ds.last_request_read,
                    ds.request,
                    ds.xcb_conn_buf.as_ptr(),
                    XCB_CONN_BUF_SIZE,
                )
            };
            if status != 0 {
                return Err(std::io::ErrorKind::InvalidData.into());
            }
        }

        for cs in self.conns.iter().take(self.conn_count) {
            if display_conns.clone().any(|dc| dc == cs.conn) {
                continue;
            }
            // SAFETY: the buffer is readable for the generated length and
            // connection identity was checked above. We still assume no
            // concurrent X11 use (all other threads are suspended at their
            // checkpoint-time stacks).
            let status =
                unsafe { valo_xcb_restore(cs.conn, cs.xcb_conn_buf.as_ptr(), XCB_CONN_BUF_SIZE) };
            if status != 0 {
                return Err(std::io::ErrorKind::InvalidData.into());
            }
        }

        // TODO(ai-review): generated, not yet audited — TEMP debug
        log_counters("restored (io state from save, lists/locks from checkpoint)");
        Ok(())
    }
}
