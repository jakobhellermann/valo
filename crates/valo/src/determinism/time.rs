// TODO(ai-review): generated, not yet audited
//! Deterministic virtual time: every frame boundary advances the clock by
//! exactly one frame duration (`Config::determinism.time.framerate`), independent of wall
//! time, from hardcoded epochs. Timed waits of the main thread that run out
//! advance it too (see [`add_timed_wait`]), as waiting takes time even on an
//! infinitely fast machine.
//!
//! Virtual: `CLOCK_MONOTONIC`, `CLOCK_MONOTONIC_RAW`, `CLOCK_REALTIME`, the
//! `_COARSE` variants, `gettimeofday`, `time`. Other clock ids (CPU, BOOTTIME)
//! stay real, since kernel waits on them compare against real time.
//! valo-internal reads use [`real_ns`]; the interposed libc path would return
//! virtual time.
//!
//! All state is plain statics, so a restore rewinds it, config included.

use std::ffi::{c_int, c_void};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use valo_ipc::config::{Framerate, TimeConfig};

use crate::{debug, warn};

/// Real time reaches the game unmodified (`Config::determinism.time.uncontrolled_time`).
/// Off (default) = deterministic time mode; checked by the interposed
/// sources in `hooks::time` and by `advance_frame`.
static UNCONTROLLED_TIME: AtomicBool = AtomicBool::new(false);
/// Virtual frame rate `num`/`denom` (`Config::determinism.time.framerate`), sanitized
/// by `set_config`. One boundary advances the clock by `1e9*denom/num` ns;
/// the `60/1` here is the pre-config default for runs without a client.
static FRAMERATE_NUM: AtomicU64 = AtomicU64::new(60);
static FRAMERATE_DENOM: AtomicU64 = AtomicU64::new(1);

/// Per-source read counters (diagnostics: which clocks the game actually
/// reads — an untracked source explains a non-hyperspeed game).
static READS_MONOTONIC: AtomicU64 = AtomicU64::new(0);
static READS_REALTIME: AtomicU64 = AtomicU64::new(0);
static READS_MONOTONIC_RAW: AtomicU64 = AtomicU64::new(0);
static READS_OTHER: AtomicU64 = AtomicU64::new(0);
static READS_GETTIMEOFDAY: AtomicU64 = AtomicU64::new(0);
static READS_TIME: AtomicU64 = AtomicU64::new(0);
static READS_TOTAL: AtomicU64 = AtomicU64::new(0);
static LAST_DUMP_NS: AtomicU64 = AtomicU64::new(0);
static LAST_DUMP_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Virtual ticks since process start, in nanoseconds.
static TICKS_NS: AtomicU64 = AtomicU64::new(0);
/// Fractional-nanosecond carry: `1e9*denom % num` per boundary; a full
/// `num` worth of remainder carries one extra nanosecond.
static FRAC: AtomicU64 = AtomicU64::new(0);

/// Hardcoded startup epochs (never captured from the real clock).
/// MONOTONIC starts at zero; REALTIME at 2023-11-14T22:13:20Z.
const MONOTONIC_EPOCH_NS: u64 = 0;
const REALTIME_EPOCH_NS: u64 = 1_700_000_000 * 1_000_000_000;

/// Upper bound for accepted `framerate` components (sanitization in
/// `set_config`); keeps the carry math within u64.
const FRAMERATE_COMPONENT_MAX: u64 = 1_000_000;

/// Real (untracked) monotonic nanoseconds via the raw syscall — safe for
/// valo-internal use and unaffected by the interposition in `hooks::time`.
pub fn real_ns() -> u64 {
    let ts = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

/// Apply `Config::determinism.time`. Zero or oversized `framerate` components fall
/// back to the `60/1` default (the serde default of a missing `Framerate`
/// struct is `0/0` and lands here too).
pub(crate) fn set_config(config: &TimeConfig) {
    let (num, denom) = match (config.framerate.num, config.framerate.denom) {
        (num, denom)
            if num > 0
                && num <= FRAMERATE_COMPONENT_MAX
                && denom > 0
                && denom <= FRAMERATE_COMPONENT_MAX =>
        {
            (num, denom)
        }
        _ => {
            warn!(
                Hooks,
                "det time: invalid framerate {}/{}, falling back to 60/1",
                config.framerate.num,
                config.framerate.denom
            );
            (60, 1)
        }
    };
    UNCONTROLLED_TIME.store(config.uncontrolled_time, Ordering::Relaxed);
    FRAMERATE_NUM.store(num, Ordering::Relaxed);
    FRAMERATE_DENOM.store(denom, Ordering::Relaxed);
    debug!(
        Hooks,
        "det time: {} framerate {num}/{denom} (monotonic epoch {MONOTONIC_EPOCH_NS}ns, realtime epoch {}s)",
        if config.uncontrolled_time {
            "uncontrolled"
        } else {
            "deterministic"
        },
        REALTIME_EPOCH_NS / 1_000_000_000
    );
}

/// The currently effective `Config::determinism.time` (for the handshake echo).
pub(crate) fn effective() -> TimeConfig {
    TimeConfig {
        uncontrolled_time: UNCONTROLLED_TIME.load(Ordering::Relaxed),
        framerate: Framerate {
            num: FRAMERATE_NUM.load(Ordering::Relaxed),
            denom: FRAMERATE_DENOM.load(Ordering::Relaxed),
        },
    }
}

/// Deterministic (virtual) time active — `Config::determinism.time.uncontrolled_time`
/// off.
pub(crate) fn deterministic() -> bool {
    !UNCONTROLLED_TIME.load(Ordering::Relaxed)
}

/// One frame boundary: advance the virtual clock by one frame duration
/// (with fractional-nanosecond carry). No-op when uncontrolled.
pub(crate) fn advance_frame() {
    if !deterministic() {
        return;
    }
    let num = FRAMERATE_NUM.load(Ordering::Relaxed);
    let denom = FRAMERATE_DENOM.load(Ordering::Relaxed);
    // Nanoseconds per `num` frames: per boundary the whole part, plus the
    // remainder accumulated in `FRAC` until it carries a nanosecond.
    let total = 1_000_000_000 * denom;
    let add = total / num;
    let frac_inc = total % num;
    let mut frac = FRAC.fetch_add(frac_inc, Ordering::Relaxed) + frac_inc;
    let mut carry = 0;
    while frac >= num {
        frac -= num;
        carry += 1;
    }
    FRAC.store(frac, Ordering::Relaxed);
    TICKS_NS.fetch_add(add + carry, Ordering::Relaxed);
}

/// Interposed `clock_gettime`: `Some(rv)` when the read was served
/// virtually, `None` to fall through to the real call.
pub(crate) fn clock_gettime(clock_id: libc::clockid_t, tp: *mut libc::timespec) -> Option<c_int> {
    count_read(clock_id);
    if tp.is_null() {
        return None;
    }
    if !deterministic() {
        return None;
    }
    let ns = match clock_id {
        libc::CLOCK_MONOTONIC | libc::CLOCK_MONOTONIC_RAW | libc::CLOCK_MONOTONIC_COARSE => {
            virtual_monotonic_ns()
        }
        libc::CLOCK_REALTIME | libc::CLOCK_REALTIME_COARSE => virtual_realtime_ns(),
        _ => return None,
    };
    write_timespec(tp, ns);
    Some(0)
}

/// Interposed `gettimeofday`: `Some(rv)` when served virtually, `None` to
/// fall through to the real call.
pub(crate) fn gettimeofday(tv: *mut libc::timeval, _tz: *mut c_void) -> Option<c_int> {
    READS_TOTAL.fetch_add(1, Ordering::Relaxed);
    READS_GETTIMEOFDAY.fetch_add(1, Ordering::Relaxed);
    maybe_dump_reads();
    if tv.is_null() {
        return None;
    }
    if deterministic() {
        let ns = virtual_realtime_ns();
        // SAFETY: `tv` points to a valid timeval per the call contract.
        unsafe {
            (*tv).tv_sec = (ns / 1_000_000_000) as libc::time_t;
            (*tv).tv_usec = ((ns % 1_000_000_000) / 1000) as libc::suseconds_t;
        }
        return Some(0);
    }
    None
}

/// Interposed `time`: `Some(rv)` when served virtually, `None` to fall
/// through to the real call.
pub(crate) fn time(t: *mut libc::time_t) -> Option<libc::time_t> {
    READS_TOTAL.fetch_add(1, Ordering::Relaxed);
    READS_TIME.fetch_add(1, Ordering::Relaxed);
    maybe_dump_reads();
    if !deterministic() {
        return None;
    }
    let secs = virtual_realtime_ns() / 1_000_000_000;
    if !t.is_null() {
        // SAFETY: `t` is a valid out pointer; the null case is checked here.
        unsafe { *t = secs as libc::time_t };
    }
    Some(secs as libc::time_t)
}

fn virtual_monotonic_ns() -> u64 {
    MONOTONIC_EPOCH_NS + TICKS_NS.load(Ordering::Relaxed)
}

fn virtual_realtime_ns() -> u64 {
    REALTIME_EPOCH_NS + TICKS_NS.load(Ordering::Relaxed)
}

fn write_timespec(tp: *mut libc::timespec, ns: u64) {
    // SAFETY: `tp` points to a valid timespec per the call contract.
    unsafe {
        (*tp).tv_sec = (ns / 1_000_000_000) as libc::time_t;
        (*tp).tv_nsec = (ns % 1_000_000_000) as libc::c_long;
    }
}

fn count_read(clock_id: libc::clockid_t) {
    READS_TOTAL.fetch_add(1, Ordering::Relaxed);
    match clock_id {
        libc::CLOCK_MONOTONIC => {
            READS_MONOTONIC.fetch_add(1, Ordering::Relaxed);
        }
        libc::CLOCK_REALTIME => {
            READS_REALTIME.fetch_add(1, Ordering::Relaxed);
        }
        libc::CLOCK_MONOTONIC_RAW => {
            READS_MONOTONIC_RAW.fetch_add(1, Ordering::Relaxed);
        }
        _ => {
            READS_OTHER.fetch_add(1, Ordering::Relaxed);
        }
    }
    maybe_dump_reads();
}

fn maybe_dump_reads() {
    let total = READS_TOTAL.load(Ordering::Relaxed);
    if total == LAST_DUMP_TOTAL.load(Ordering::Relaxed) {
        return;
    }
    let now = real_ns();
    if now.saturating_sub(LAST_DUMP_NS.load(Ordering::Relaxed)) < 1_000_000_000 {
        return;
    }
    LAST_DUMP_NS.store(now, Ordering::Relaxed);
    LAST_DUMP_TOTAL.store(total, Ordering::Relaxed);
    debug!(
        Hooks,
        "timeget: total={total} monotonic={} realtime={} monotonic_raw={} other={} gettimeofday={} time={}",
        READS_MONOTONIC.load(Ordering::Relaxed),
        READS_REALTIME.load(Ordering::Relaxed),
        READS_MONOTONIC_RAW.load(Ordering::Relaxed),
        READS_OTHER.load(Ordering::Relaxed),
        READS_GETTIMEOFDAY.load(Ordering::Relaxed),
        READS_TIME.load(Ordering::Relaxed),
    );
}
