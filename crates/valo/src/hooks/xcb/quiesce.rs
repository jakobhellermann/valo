//! Raw-xcb connection quiescence around thread freezes.
//!
//! The Xlib displays are quiesced around checkpoint/restore freezes by
//! their own lock: XLockDisplay keeps threads out of Xlib calls, XSync
//! drains every request and reply. Raw xcb connections have no such
//! lock, and a freeze can catch their users in two unrescuable states:
//! inside an xcb call holding the connection lock — the rewound thread
//! never releases it, and the connection is locked forever — and
//! mid-message, whose continuation the discarded timeline consumes, so
//! the restored reader parks on bytes the server will never send again.
//!
//! The barrier closes both holes: while it is up, threads entering the
//! interposed xcb entry points park outside any connection state, and
//! the quiescing thread waits for in-flight entry points to drain
//! before flushing and roundtripping every tracked connection to a
//! message boundary. The barrier stays up across the freeze, so the
//! freeze can only catch threads parked at it; it clears before the
//! threads resume. Blocking waiters that do not drain in time are
//! logged and frozen as they are — no worse than without the barrier,
//! and visible in the log.

use std::ffi::{c_char, c_void};
use std::os::fd::BorrowedFd;
use std::os::raw::{c_int, c_uint};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use rustix::event::{PollFd, PollFlags};

use crate::determinism::time::real_ns;

use super::{GAME_XCB_CONNS, LIB_XCB, xcb_connection_t, xcb_generic_error_t};
use crate::hooks::{is_native, link_function, run_native};
use crate::{override_sym, warn};

/// Wait for in-flight interposed entry points to leave before touching a
/// connection. Generous against a local server's roundtrip latency.
const DRAIN_TIMEOUT_MS: u64 = 100;
/// Bound for one connection's quiesce roundtrip. The quiesce must never
/// wedge a checkpoint on a stalled connection.
const ROUNDTRIP_TIMEOUT_MS: u64 = 250;

static BARRIER: AtomicBool = AtomicBool::new(false);
/// Interposed xcb entry points currently running on other threads. The
/// quiesce waits for this to reach zero; threads parked at the barrier do
/// not count.
static ACTIVE: AtomicU32 = AtomicU32::new(0);
static PARK_MUTEX: Mutex<()> = Mutex::new(());
static PARK_CV: Condvar = Condvar::new();

/// Park a thread entering an interposed xcb entry point while the barrier
/// is up. Pairs with [`exit`]; the caller must not touch any connection
/// between this call and the following [`exit`].
pub(crate) fn enter() {
    // valo's own calls run in native mode and must pass through: libxcb
    // reaches interposed entry points through its own GOT, so the quiesce's
    // roundtrips re-enter these hooks from inside the real functions
    if is_native() {
        return;
    }
    loop {
        ACTIVE.fetch_add(1, Ordering::SeqCst);
        if !BARRIER.load(Ordering::SeqCst) {
            return;
        }
        // parked threads do not count as active, or the quiesce would
        // wait for threads that wait for it
        ACTIVE.fetch_sub(1, Ordering::SeqCst);
        let guard = PARK_MUTEX.lock().unwrap();
        if !BARRIER.load(Ordering::SeqCst) {
            continue;
        }
        let (guard, _timeout) = PARK_CV
            .wait_timeout(guard, Duration::from_millis(50))
            .unwrap();
        drop(guard);
    }
}

pub(crate) fn exit() {
    if is_native() {
        return;
    }
    ACTIVE.fetch_sub(1, Ordering::SeqCst);
}

/// Bring every tracked raw xcb connection to a message boundary and hold
/// the barrier up across the freeze. The caller suspends the threads and
/// then calls [`release`] before they resume.
pub fn quiesce() {
    BARRIER.store(true, Ordering::SeqCst);

    // Real time, not the virtual clock: the freeze holds all threads, so
    // virtual time stands still at a boundary. Legitimately parked
    // waiters (a thread blocked in XNextEvent) may outlive the drain —
    // they hold no connection state once every connection is quiet: no
    // half-read reply, no buffered bytes, nothing half-written.
    let deadline = real_ns() + DRAIN_TIMEOUT_MS * 1_000_000;
    let mut sleep_nanos = 100_000u64;
    loop {
        let active = ACTIVE.load(Ordering::SeqCst);
        if active == 0 {
            break;
        }
        if connections_quiet() {
            break;
        }
        if real_ns() >= deadline {
            warn!(
                Hooks,
                "xcb quiesce: {active} in-flight xcb calls did not drain within {DRAIN_TIMEOUT_MS} ms"
            );
            break;
        }
        // backoff between reads; never through the hooked sleep paths
        let ts = rustix::time::Timespec {
            tv_sec: 0,
            tv_nsec: sleep_nanos as i64,
        };
        let _ = run_native(|| rustix::thread::nanosleep(&ts));
        sleep_nanos = (sleep_nanos * 2).min(2_000_000);
    }

    for conn in GAME_XCB_CONNS.iter() {
        let conn = conn as *mut xcb_connection_t;
        if crate::hooks::xlib::xfixup::is_display_backed(conn.cast()) {
            // its display was quiesced by the Xlib lock-and-sync
            continue;
        }
        // SAFETY: the connection is tracked and live, and the barrier
        // keeps other threads out of every interposed entry point.
        unsafe { quiesce_conn(conn) };
    }
}

/// Let threads parked at the barrier proceed. Call after the freeze, before
/// the threads resume.
pub fn release() {
    BARRIER.store(false, Ordering::SeqCst);
    PARK_CV.notify_all();
}
type HasErrorFn = unsafe extern "C" fn(*mut xcb_connection_t) -> c_int;
type FlushFn = unsafe extern "C" fn(*mut xcb_connection_t) -> c_int;
type GetFdFn = unsafe extern "C" fn(*mut xcb_connection_t) -> c_int;
type GetInputFocusFn = unsafe extern "C" fn(*mut xcb_connection_t) -> Cookie;
type PollReply64Fn = unsafe extern "C" fn(
    *mut xcb_connection_t,
    u64,
    *mut *mut c_void,
    *mut *mut xcb_generic_error_t,
) -> c_int;

#[repr(C)]
struct Cookie {
    sequence: u32,
}

static XCB_CONNECTION_HAS_ERROR: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static XCB_FLUSH: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static XCB_GET_FILE_DESCRIPTOR: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static XCB_GET_INPUT_FOCUS: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static XCB_POLL_FOR_REPLY64: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

fn linked(slot: &AtomicPtr<c_void>, name: &[u8]) -> *mut c_void {
    link_function(slot, name.as_ptr().cast::<c_char>(), std::ptr::null()).unwrap()
}

/// Flush the connection and wait for a GetInputFocus roundtrip within the
/// bound, leaving the connection at a message boundary. Returns true when
/// the roundtrip completed; on timeout the connection stays as it is.
///
/// # Safety
/// `conn` must be a live tracked connection with no concurrent xcb access.
pub unsafe fn roundtrip(conn: *mut xcb_connection_t, timeout_ms: u64) -> bool {
    // SAFETY: the resolved symbols are the real libxcb functions; each
    // call runs in native mode, so libxcb-internal hops through its GOT
    // pass the interposed entry points instead of re-entering the barrier.
    unsafe {
        let has_error: HasErrorFn = std::mem::transmute(linked(
            &XCB_CONNECTION_HAS_ERROR,
            b"xcb_connection_has_error\0",
        ));
        if run_native(|| has_error(conn)) != 0 {
            return true;
        }
        let flush: FlushFn = std::mem::transmute(linked(&XCB_FLUSH, b"xcb_flush\0"));
        run_native(|| flush(conn));

        let get_input_focus: GetInputFocusFn =
            std::mem::transmute(linked(&XCB_GET_INPUT_FOCUS, b"xcb_get_input_focus\0"));
        let cookie = run_native(|| get_input_focus(conn));
        let get_fd: GetFdFn = std::mem::transmute(linked(
            &XCB_GET_FILE_DESCRIPTOR,
            b"xcb_get_file_descriptor\0",
        ));
        let fd = run_native(|| get_fd(conn));
        let poll_reply: PollReply64Fn =
            std::mem::transmute(linked(&XCB_POLL_FOR_REPLY64, b"xcb_poll_for_reply64\0"));

        if fd < 0 {
            return false;
        }
        let deadline = real_ns() + timeout_ms * 1_000_000;
        loop {
            let mut reply: *mut c_void = std::ptr::null_mut();
            let mut error: *mut xcb_generic_error_t = std::ptr::null_mut();
            if run_native(|| poll_reply(conn, cookie.sequence as u64, &mut reply, &mut error)) != 0
            {
                run_native(|| libc::free(reply.cast()));
                run_native(|| libc::free(error.cast()));
                return true;
            }
            if real_ns() >= deadline {
                return false;
            }
            // SAFETY: fd is xcb's own fd for the live connection.
            let fd = BorrowedFd::borrow_raw(fd);
            // like xcb's own wait: a full send buffer leaves the request
            // half-written in the out queue, and with everyone else parked
            // at the barrier only this loop retries the flush
            let mut polls = [PollFd::new(&fd, PollFlags::IN | PollFlags::OUT)];
            let ts = rustix::time::Timespec {
                tv_sec: 0,
                tv_nsec: 10_000_000,
            };
            let _ = run_native(|| rustix::event::poll(&mut polls, Some(&ts)));
            if polls[0]
                .revents()
                .intersects(PollFlags::OUT | PollFlags::HUP | PollFlags::ERR)
            {
                run_native(|| flush(conn));
            }
        }
    }
}

/// Send a reply-bearing request without waiting for its reply. Exported
/// for the C shim's kick: a consumer parked in xcb_conn_wait's poll wakes
/// when the server answers, so the reply's arrival is the wake-up — the
/// caller must not (and never needs to) wait for it. Plain C calls in the
/// shim would resolve to the interposed entry points, hence this export.
///
/// # Safety
/// `conn` must be a live xcb connection whose locks the caller holds.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn valo_xcb_wakeup_request(conn: *mut xcb_connection_t) {
    // SAFETY: the resolved symbols are the real libxcb functions; native
    // mode keeps libxcb's internal GOT hops out of the barrier.
    unsafe {
        let has_error: HasErrorFn = std::mem::transmute(linked(
            &XCB_CONNECTION_HAS_ERROR,
            b"xcb_connection_has_error\0",
        ));
        if run_native(|| has_error(conn)) != 0 {
            return;
        }
        let flush: FlushFn = std::mem::transmute(linked(&XCB_FLUSH, b"xcb_flush\0"));
        run_native(|| flush(conn));
        let get_input_focus: GetInputFocusFn =
            std::mem::transmute(linked(&XCB_GET_INPUT_FOCUS, b"xcb_get_input_focus\0"));
        // the reply is left unretrieved: its sole purpose is to make the
        // socket readable for the parked consumer
        run_native(|| get_input_focus(conn));
    }
}

/// Whether every tracked connection is at a message boundary and has no
/// pending socket data: the in-flight entries counted in ACTIVE can then
/// only be threads parked in a blocking wait, which hold no connection
/// state and freeze safely.
fn connections_quiet() -> bool {
    // SAFETY: the linked symbol is the real libxcb function; connections
    // are tracked and live.
    let get_fd: GetFdFn = unsafe {
        std::mem::transmute(linked(
            &XCB_GET_FILE_DESCRIPTOR,
            b"xcb_get_file_descriptor\0",
        ))
    };
    GAME_XCB_CONNS.iter().all(|conn| {
        let conn = conn as *mut xcb_connection_t;
        if !crate::hooks::xlib::xfixup::is_quiet(conn.cast()) {
            return false;
        }
        let fd = unsafe { run_native(|| get_fd(conn)) };
        if fd < 0 {
            return false;
        }
        // SAFETY: fd is xcb's own fd for the live connection.
        let fd = unsafe { BorrowedFd::borrow_raw(fd) };
        let mut polls = [PollFd::new(&fd, PollFlags::IN)];
        let ts = rustix::time::Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        matches!(
            run_native(|| rustix::event::poll(&mut polls, Some(&ts))),
            Ok(0)
        )
    })
}

/// Flush and roundtrip one connection to a message boundary. A roundtrip
/// that cannot complete is a broken connection, not a slow one: freezing
/// anyway would capture a desynced stream and hand the hang to a later,
/// nondeterministic moment. Fail loudly instead.
///
/// # Safety
/// `conn` must be a live tracked connection with no concurrent xcb access.
unsafe fn quiesce_conn(conn: *mut xcb_connection_t) {
    let timeout = ROUNDTRIP_TIMEOUT_MS;
    // SAFETY: tracked and live, and the barrier keeps other threads out.
    if !unsafe { roundtrip(conn, timeout) } {
        crate::hooks::xlib::xfixup::log_counters("quiesce roundtrip failed");
        crate::abort!(
            Checkpoint,
            "raw xcb connection {conn:p} did not answer the quiesce roundtrip within {timeout} ms"
        );
    }
}

// The interposed entry points below carry the barrier. Everything a guest
// thread can use to put a connection mid-message or hold its lock goes
// through one of these; the event-side entries are barriered in events.rs.

override_sym!(xcb_send_request64(c: *mut xcb_connection_t, flags: c_int, vector: *mut c_void, request: *mut c_void) -> u64, lib: LIB_XCB, |real| {
    enter();
    let seq = real(c, flags, vector, request);
    exit();
    seq
});

override_sym!(xcb_send_request(c: *mut xcb_connection_t, flags: c_int, vector: *mut c_void, request: *mut c_void) -> c_uint, lib: LIB_XCB, |real| {
    enter();
    let seq = real(c, flags, vector, request);
    exit();
    seq
});

override_sym!(xcb_send_request_with_fds64(c: *mut xcb_connection_t, flags: c_int, vector: *mut c_void, request: *mut c_void, num_fds: c_uint, fds: *mut c_int) -> u64, lib: LIB_XCB, |real| {
    enter();
    let seq = real(c, flags, vector, request, num_fds, fds);
    exit();
    seq
});

override_sym!(xcb_send_request_with_fds(c: *mut xcb_connection_t, flags: c_int, vector: *mut c_void, request: *mut c_void, num_fds: c_uint, fds: *mut c_int) -> c_uint, lib: LIB_XCB, |real| {
    enter();
    let seq = real(c, flags, vector, request, num_fds, fds);
    exit();
    seq
});

override_sym!(xcb_wait_for_reply64(c: *mut xcb_connection_t, request: u64, e: *mut *mut xcb_generic_error_t) -> *mut c_void, lib: LIB_XCB, |real| {
    enter();
    let reply = real(c, request, e);
    exit();
    reply
});

override_sym!(xcb_wait_for_reply(c: *mut xcb_connection_t, request: c_uint, e: *mut *mut xcb_generic_error_t) -> *mut c_void, lib: LIB_XCB, |real| {
    enter();
    let reply = real(c, request, e);
    exit();
    reply
});

override_sym!(xcb_poll_for_reply64(c: *mut xcb_connection_t, request: u64, reply: *mut *mut c_void, error: *mut *mut xcb_generic_error_t) -> c_int, lib: LIB_XCB, |real| {
    enter();
    let done = real(c, request, reply, error);
    exit();
    done
});

override_sym!(xcb_poll_for_reply(c: *mut xcb_connection_t, request: c_uint, reply: *mut *mut c_void, error: *mut *mut xcb_generic_error_t) -> c_int, lib: LIB_XCB, |real| {
    enter();
    let done = real(c, request, reply, error);
    exit();
    done
});
