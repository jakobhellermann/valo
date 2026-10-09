// TODO(ai-review): generated, not yet audited
//! Aggregated logging of sleep/wait calls.

use std::cell::Cell;
use std::ffi::c_int;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering, fence};

use crate::hooks::is_native;

const LOG_SLEEP_STATS: bool = false;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub(crate) enum SleepKind {
    Usleep,
    Nanosleep,
    ClockNanosleep,
    Poll,
    Select,
    Pselect,
    EpollWait,
    SchedYield,
    PollChk,
    Ppoll,
    PpollChk,
    EpollPwait,
    EpollPwait2,
}

impl SleepKind {
    const COUNT: usize = Self::EpollPwait2 as usize + 1;

    fn name(self) -> &'static str {
        match self {
            Self::Usleep => "usleep",
            Self::Nanosleep => "nanosleep",
            Self::ClockNanosleep => "clock_nanosleep",
            Self::Poll => "poll",
            Self::Select => "select",
            Self::Pselect => "pselect",
            Self::EpollWait => "epoll_wait",
            Self::SchedYield => "sched_yield",
            Self::PollChk => "__poll_chk",
            Self::Ppoll => "ppoll",
            Self::PpollChk => "__ppoll_chk",
            Self::EpollPwait => "epoll_pwait",
            Self::EpollPwait2 => "epoll_pwait2",
        }
    }

    fn from_id(id: u32) -> Self {
        const ALL: [SleepKind; SleepKind::COUNT] = [
            SleepKind::Usleep,
            SleepKind::Nanosleep,
            SleepKind::ClockNanosleep,
            SleepKind::Poll,
            SleepKind::Select,
            SleepKind::Pselect,
            SleepKind::EpollWait,
            SleepKind::SchedYield,
            SleepKind::PollChk,
            SleepKind::Ppoll,
            SleepKind::PpollChk,
            SleepKind::EpollPwait,
            SleepKind::EpollPwait2,
        ];
        ALL[id as usize]
    }
}

struct Slot {
    claimed: AtomicBool,
    fn_id: AtomicU32,
    tid: AtomicU32,
    calls: AtomicU64,
    /// Requested durations (sleeps) or timeouts (waits), summed.
    total_ns: AtomicU64,
    /// Wait calls that returned on timeout (ret == 0)
    timeouts: AtomicU64,
}

impl Slot {
    #[allow(clippy::declare_interior_mutable_const)]
    const EMPTY: Slot = Slot {
        claimed: AtomicBool::new(false),
        fn_id: AtomicU32::new(0),
        tid: AtomicU32::new(0),
        calls: AtomicU64::new(0),
        total_ns: AtomicU64::new(0),
        timeouts: AtomicU64::new(0),
    };
}

/// (function, thread) buckets.
const MAX_SLOTS: usize = 2048;
static SLOTS: [Slot; MAX_SLOTS] = [const { Slot::EMPTY }; MAX_SLOTS];

/// Pairs that did not fit into the slot table.
static OVERFLOW: AtomicU64 = AtomicU64::new(0);

/// Cumulative call count across all slots; gates dumping on activity.
static TOTAL_CALLS: AtomicU64 = AtomicU64::new(0);
static LAST_DUMP_CALLS: AtomicU64 = AtomicU64::new(0);
static LAST_DUMP_MS: AtomicU64 = AtomicU64::new(0);
static DUMPING: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// Cache of the slot index per function for this thread
    // (usize::MAX = not resolved yet).
    static SLOT_CACHE: Cell<[usize; SleepKind::COUNT]> =
        const { Cell::new([usize::MAX; SleepKind::COUNT]) };
    static TID: Cell<u32> = const { Cell::new(0) };
}

const DUMP_INTERVAL_MS: u64 = 1000;

/// Duration of a `clock_nanosleep` request: relative as-is, absolute
/// converted against the target clock.
pub(crate) fn clock_nanosleep_duration(
    clock_id: libc::clockid_t,
    flags: c_int,
    req: *const libc::timespec,
) -> u64 {
    // SAFETY: `req` is a valid timespec per the call contract.
    let req = unsafe { *req };
    if flags & libc::TIMER_ABSTIME == 0 {
        return req.tv_sec as u64 * 1_000_000_000 + req.tv_nsec as u64;
    }
    (req.tv_sec as i128 * 1_000_000_000 + req.tv_nsec as i128)
        .saturating_sub(clock_now_ns(clock_id) as i128)
        .max(0) as u64
}

/// Now on the given clock via the raw syscall (libc's clock_gettime is
/// interposed by the time hooks). Unknown clock ids read as zero.
fn clock_now_ns(clock_id: libc::clockid_t) -> u64 {
    use rustix::time::{ClockId, DynamicClockId, clock_gettime, clock_gettime_dynamic};
    let ts = match clock_id {
        libc::CLOCK_MONOTONIC => clock_gettime(ClockId::Monotonic),
        libc::CLOCK_REALTIME => clock_gettime(ClockId::Realtime),
        libc::CLOCK_BOOTTIME => match clock_gettime_dynamic(DynamicClockId::Boottime) {
            Ok(ts) => ts,
            Err(_) => return 0,
        },
        _ => return 0,
    };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

pub(crate) fn record(kind: SleepKind, ns: u64, timed_out: bool) {
    if is_native() || !LOG_SLEEP_STATS {
        return;
    }
    TOTAL_CALLS.fetch_add(1, Ordering::Relaxed);

    let tid = current_tid();
    let idx = SLOT_CACHE.with(|cache| {
        let mut slots = cache.get();
        let mut idx = slots[kind as usize];
        if idx == usize::MAX {
            idx = find_or_claim_slot(kind as u32, tid);
            slots[kind as usize] = idx;
            cache.set(slots);
        }
        idx
    });
    if idx == usize::MAX {
        OVERFLOW.fetch_add(1, Ordering::Relaxed);
    } else {
        let slot = &SLOTS[idx];
        slot.calls.fetch_add(1, Ordering::Relaxed);
        slot.total_ns.fetch_add(ns, Ordering::Relaxed);
        if timed_out {
            slot.timeouts.fetch_add(1, Ordering::Relaxed);
        }
    }
    maybe_dump();
}

fn current_tid() -> u32 {
    TID.with(|cell| {
        let tid = cell.get();
        if tid != 0 {
            return tid;
        }
        let tid = rustix::thread::gettid().as_raw_pid() as u32;
        cell.set(tid);
        tid
    })
}

fn now_ms() -> u64 {
    crate::determinism::time::real_ns() / 1_000_000
}

fn find_or_claim_slot(fn_id: u32, tid: u32) -> usize {
    let mut free = usize::MAX;
    for (i, slot) in SLOTS.iter().enumerate() {
        if slot.claimed.load(Ordering::Acquire) {
            if slot.fn_id.load(Ordering::Relaxed) == fn_id
                && slot.tid.load(Ordering::Relaxed) == tid
            {
                return i;
            }
        } else if free == usize::MAX {
            free = i;
        }
    }
    if free == usize::MAX {
        return usize::MAX;
    }
    let slot = &SLOTS[free];
    if slot
        .claimed
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        // Lost the race; retry once through the matching path next call.
        return usize::MAX;
    }
    slot.fn_id.store(fn_id, Ordering::Relaxed);
    slot.tid.store(tid, Ordering::Relaxed);
    fence(Ordering::Release);
    free
}

fn maybe_dump() {
    let calls = TOTAL_CALLS.load(Ordering::Relaxed);
    if calls == LAST_DUMP_CALLS.load(Ordering::Relaxed) {
        return;
    }
    let now = now_ms();
    if now.saturating_sub(LAST_DUMP_MS.load(Ordering::Relaxed)) < DUMP_INTERVAL_MS {
        return;
    }
    if DUMPING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    LAST_DUMP_MS.store(now, Ordering::Relaxed);
    LAST_DUMP_CALLS.store(calls, Ordering::Relaxed);
    let main_tid = crate::thread_tracker::main_thread_tid();
    let overflow = OVERFLOW.load(Ordering::Relaxed);
    crate::debug!(Hooks, "sleepstats: --- begin (overflow {overflow}) ---");
    for slot in SLOTS.iter() {
        if !slot.claimed.load(Ordering::Acquire) {
            continue;
        }
        let calls = slot.calls.load(Ordering::Relaxed);
        if calls == 0 {
            continue;
        }
        let fn_id = slot.fn_id.load(Ordering::Relaxed);
        let tid = slot.tid.load(Ordering::Relaxed);
        let total_ns = slot.total_ns.load(Ordering::Relaxed);
        let timeouts = slot.timeouts.load(Ordering::Relaxed);
        let avg_us = total_ns.checked_div(calls).unwrap_or(0) / 1000;
        let marker = if tid == main_tid { " [main]" } else { "" };
        crate::debug!(
            Hooks,
            "sleepstats: {} tid={} calls={calls} total={:.1}ms avg={avg_us}us timeouts={timeouts}{marker}",
            SleepKind::from_id(fn_id).name(),
            tid,
            total_ns as f64 / 1_000_000.0,
        );
    }
    crate::debug!(Hooks, "sleepstats: --- end ---");
    DUMPING.store(false, Ordering::Release);
}
