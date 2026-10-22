//! The agent channel: opaque requests from the client to an agent thread inside the target, one
//! reply each, exchanged while the frame thread holds a boundary.
//!
//! All state is plain static memory, so a restore rewinds it together with the agent thread.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use rustix::thread::futex;

use crate::determinism::time::real_monotonic;
use crate::{error, thread_tracker};

/// How long a held boundary waits for the agent's reply.
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

struct Exchange {
    /// Published by the boundary, taken by the agent.
    request: Option<Vec<u8>>,
    reply: Option<Vec<u8>>,
    /// A request was published and its reply is still awaited.
    open: bool,
}

static EXCHANGE: Mutex<Exchange> = Mutex::new(Exchange {
    request: None,
    reply: None,
    open: false,
});
/// Bumped per published request; the futex word the agent waits on.
static REQUESTS: AtomicU32 = AtomicU32::new(0);
/// Bumped per reply; the futex word the boundary waits on.
static REPLIES: AtomicU32 = AtomicU32::new(0);
static REGISTERED: AtomicBool = AtomicBool::new(false);

fn exchange() -> MutexGuard<'static, Exchange> {
    EXCHANGE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Hands `request` to the agent and waits for its reply, at most [`REPLY_TIMEOUT`] of real time.
pub fn request(request: Vec<u8>) -> Result<Vec<u8>, String> {
    request_within(request, REPLY_TIMEOUT)
}

fn request_within(request: Vec<u8>, timeout: Duration) -> Result<Vec<u8>, String> {
    if !REGISTERED.load(Ordering::SeqCst) {
        return Err("no agent: nothing in the target called valo_agent_register".into());
    }
    let mut seen = REPLIES.load(Ordering::SeqCst);
    {
        let mut exchange = exchange();
        exchange.request = Some(request);
        exchange.reply = None;
        exchange.open = true;
    }
    signal(&REQUESTS);

    let start = real_monotonic();
    loop {
        {
            let mut exchange = exchange();
            if let Some(reply) = exchange.reply.take() {
                exchange.open = false;
                return Ok(reply);
            }
            if real_monotonic() - start > timeout {
                exchange.request = None;
                exchange.open = false;
                return Err(format!("agent did not reply within {timeout:?}"));
            }
        }
        wait_for_change(&REPLIES, seen, Some(Duration::from_millis(100)))?;
        seen = REPLIES.load(Ordering::SeqCst);
    }
}

/// Announces an agent, so requests wait for it instead of failing. Call it before the first frame
/// boundary, or a client asking on that boundary sees no agent.
#[unsafe(no_mangle)]
pub extern "C" fn valo_agent_register() {
    REGISTERED.store(true, Ordering::SeqCst);
}

/// Blocks until the client sends a request and copies it into `buf`. Returns the request's
/// length; a request longer than `cap` stays queued, so the caller retries with a larger buffer.
/// Returns -1 on the frame thread, which would deadlock the boundary it waits for.
///
/// # Safety
/// `buf` is valid for writes of `cap` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn valo_agent_recv(buf: *mut u8, cap: usize) -> isize {
    if thread_tracker::is_main() {
        error!(Ipc, "valo_agent_recv called on the frame thread");
        return -1;
    }
    loop {
        let seen = REQUESTS.load(Ordering::SeqCst);
        {
            let mut exchange = exchange();
            if let Some(request) = &exchange.request {
                let len = request.len();
                if len <= cap {
                    // SAFETY: buf is valid for cap >= len bytes per the contract, and cannot
                    // overlap the request's own allocation.
                    unsafe { std::ptr::copy_nonoverlapping(request.as_ptr(), buf, len) };
                    exchange.request = None;
                }
                return len as isize;
            }
        }
        if let Err(e) = wait_for_change(&REQUESTS, seen, None) {
            error!(Ipc, "valo_agent_recv: {e}");
            return -1;
        }
    }
}

/// Replies to the request last returned by [`valo_agent_recv`]. Returns -1 when no request
/// awaits a reply: it was answered already, or the boundary gave up waiting.
///
/// # Safety
/// `buf` is valid for reads of `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn valo_agent_send(buf: *const u8, len: usize) -> i32 {
    {
        let mut exchange = exchange();
        if !exchange.open || exchange.request.is_some() || exchange.reply.is_some() {
            return -1;
        }
        // SAFETY: buf is valid for len bytes per the contract.
        let reply = unsafe { std::slice::from_raw_parts(buf, len) };
        exchange.reply = Some(reply.to_vec());
    }
    signal(&REPLIES);
    0
}

/// Waits until `word` leaves `seen` or `timeout` passes. Relative futex timeouts are measured by
/// the kernel in real time, unaffected by the virtual clock.
fn wait_for_change(word: &AtomicU32, seen: u32, timeout: Option<Duration>) -> Result<(), String> {
    let timeout = timeout.map(|t| rustix::time::Timespec {
        tv_sec: t.as_secs() as i64,
        tv_nsec: i64::from(t.subsec_nanos()),
    });
    match futex::wait(word, futex::Flags::PRIVATE, seen, timeout.as_ref()) {
        Ok(()) => Ok(()),
        Err(e)
            if e == rustix::io::Errno::AGAIN
                || e == rustix::io::Errno::TIMEDOUT
                || e == rustix::io::Errno::INTR =>
        {
            Ok(())
        }
        Err(e) => Err(format!("futex wait failed: {e}")),
    }
}

fn signal(word: &AtomicU32) {
    word.fetch_add(1, Ordering::SeqCst);
    if let Err(e) = futex::wake(word, futex::Flags::PRIVATE, u32::MAX) {
        error!(Ipc, "futex wake failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    fn recv(cap: usize) -> (isize, Vec<u8>) {
        let mut buf = vec![0u8; cap];
        // SAFETY: buf holds cap bytes.
        let len = unsafe { valo_agent_recv(buf.as_mut_ptr(), cap) };
        buf.truncate((len.max(0) as usize).min(cap));
        (len, buf)
    }

    fn send(reply: &[u8]) -> i32 {
        // SAFETY: reply is a valid slice.
        unsafe { valo_agent_send(reply.as_ptr(), reply.len()) }
    }

    /// One test: the channel is process-global state.
    #[test]
    fn exchanges_requests_with_the_agent_thread() {
        assert_eq!(
            request(b"early".to_vec()),
            Err("no agent: nothing in the target called valo_agent_register".into())
        );
        assert_eq!(send(b"unasked"), -1);

        let (delay_tx, delay_rx) = mpsc::channel::<Duration>();
        let (late_tx, late_rx) = mpsc::channel::<i32>();
        valo_agent_register();
        std::thread::spawn(move || {
            let mut cap = 4;
            loop {
                let (len, request) = recv(cap);
                assert!(len >= 0);
                if len as usize > cap {
                    cap = len as usize;
                    continue;
                }
                let delay = delay_rx.recv().unwrap();
                std::thread::sleep(delay);
                let reply: Vec<u8> = request.iter().rev().copied().collect();
                let sent = send(&reply);
                if !delay.is_zero() {
                    late_tx.send(sent).unwrap();
                }
                assert_eq!(send(b"twice"), -1);
            }
        });

        delay_tx.send(Duration::ZERO).unwrap();
        assert_eq!(request(b"abc".to_vec()), Ok(b"cba".to_vec()));
        // longer than the agent's first buffer: it retries with the reported length
        delay_tx.send(Duration::ZERO).unwrap();
        assert_eq!(request(b"abcdefgh".to_vec()), Ok(b"hgfedcba".to_vec()));

        delay_tx.send(Duration::from_millis(300)).unwrap();
        assert_eq!(
            request_within(b"slow".to_vec(), Duration::from_millis(50)),
            Err("agent did not reply within 50ms".into())
        );
        assert_eq!(late_rx.recv().unwrap(), -1);

        delay_tx.send(Duration::ZERO).unwrap();
        assert_eq!(request(b"after".to_vec()), Ok(b"retfa".to_vec()));
    }
}
