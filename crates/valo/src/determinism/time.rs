use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use rustix::time::ClockId;

use valo_ipc::config::{Framerate, TimeConfig};

use crate::warn;

pub static CLOCK: VirtualClock = VirtualClock {
    uncontrolled: AtomicBool::new(false),

    framerate_num: AtomicU32::new(60),
    framerate_denom: AtomicU32::new(1),

    realtime_start_ns: AtomicU64::new(1_000_000_000),

    ticks_ns: AtomicU64::new(0),
    remainder: AtomicU64::new(0),
    slept_ns: AtomicU64::new(0),
};

// 0 is rejected by systemd's now().
const MONOTONIC_START_NS: u64 = 1_000_000_000;

pub struct VirtualClock {
    uncontrolled: AtomicBool,

    framerate_num: AtomicU32,
    framerate_denom: AtomicU32,

    realtime_start_ns: AtomicU64,

    ticks_ns: AtomicU64,
    /// Sub-nanosecond remainder of the boundaries so far, in units of `1 / framerate_num` ns.
    remainder: AtomicU64,
    /// Time spent sleeping by the frame boundary thread
    slept_ns: AtomicU64,
}

impl VirtualClock {
    pub fn set_config(&self, config: &TimeConfig) {
        self.framerate_num
            .store(config.framerate.num.get(), Ordering::Relaxed);
        self.framerate_denom
            .store(config.framerate.denom.get(), Ordering::Relaxed);
        self.realtime_start_ns.store(
            config
                .realtime_start
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
                .try_into()
                .unwrap(),
            Ordering::Relaxed,
        );
        self.uncontrolled
            .store(config.uncontrolled_time, Ordering::Relaxed);
    }

    pub fn config(&self) -> TimeConfig {
        TimeConfig {
            uncontrolled_time: self.uncontrolled.load(Ordering::Relaxed),
            framerate: Framerate {
                num: self
                    .framerate_num
                    .load(Ordering::Relaxed)
                    .try_into()
                    .unwrap(),
                denom: self
                    .framerate_denom
                    .load(Ordering::Relaxed)
                    .try_into()
                    .unwrap(),
            },
            realtime_start: SystemTime::UNIX_EPOCH
                + Duration::from_nanos(self.realtime_start_ns.load(Ordering::Relaxed)),
        }
    }

    pub fn advance_frame(&self) {
        if self.uncontrolled.load(Ordering::Relaxed) {
            return;
        }

        let (frame_ns, remainder) = self.next_frame();
        self.remainder.store(remainder, Ordering::Relaxed);

        let slept_ns = self.slept_ns.load(Ordering::Relaxed);
        let used_ns = slept_ns.min(frame_ns);

        self.slept_ns.store(slept_ns - used_ns, Ordering::Relaxed);
        self.ticks_ns
            .fetch_add(frame_ns - used_ns, Ordering::Relaxed);
    }

    /// Passes `ns` of virtual time for a sleep of the frame thread.
    pub fn sleep(&self, ns: u64) {
        if self.uncontrolled.load(Ordering::Relaxed) {
            return;
        }
        let slept_ns = self.slept_ns.fetch_add(ns, Ordering::Relaxed) + ns;
        self.ticks_ns.fetch_add(ns, Ordering::Relaxed);
        if slept_ns > self.next_frame().0 {
            warn!(
                Frame,
                "frame thread slept {slept_ns} ns within one frame; the overhang shortens the frames after it"
            );
        }
    }

    /// The next frame's length in nanoseconds, and the `remainder` after it.
    fn next_frame(&self) -> (u64, u64) {
        let num = u64::from(self.framerate_num.load(Ordering::Relaxed));
        let denom = u64::from(self.framerate_denom.load(Ordering::Relaxed));
        let scaled = 1_000_000_000 * denom + self.remainder.load(Ordering::Relaxed);
        (scaled / num, scaled % num)
    }

    pub fn is_controlled(&self) -> bool {
        !self.uncontrolled.load(Ordering::Relaxed)
    }

    /// `real now + (deadline - virtual now)` on `clock`, and `deadline - virtual now` in
    /// nanoseconds; `None` when `clock` is not virtual or `deadline` is invalid.
    pub fn real_deadline(
        &self,
        clock: libc::clockid_t,
        deadline: &libc::timespec,
    ) -> Option<(libc::timespec, u64)> {
        let ahead_ns = timespec_ns(deadline)?.saturating_sub(self.now_ns(clock)?);
        let real_now = rustix::time::clock_gettime(real_clock(clock)?);
        let real = Duration::new(real_now.tv_sec as u64, real_now.tv_nsec as u32)
            + Duration::from_nanos(ahead_ns);
        let real = libc::timespec {
            tv_sec: real.as_secs() as libc::time_t,
            tv_nsec: real.subsec_nanos() as libc::c_long,
        };
        Some((real, ahead_ns))
    }

    /// The virtual reading of `clock`, or `None` when uncontrolled
    pub fn now_ns(&self, clock: libc::clockid_t) -> Option<u64> {
        if self.uncontrolled.load(Ordering::Relaxed) {
            return None;
        }
        let start = match clock {
            libc::CLOCK_MONOTONIC | libc::CLOCK_MONOTONIC_RAW | libc::CLOCK_MONOTONIC_COARSE => {
                MONOTONIC_START_NS
            }
            libc::CLOCK_REALTIME | libc::CLOCK_REALTIME_COARSE => {
                self.realtime_start_ns.load(Ordering::Relaxed)
            }
            _ => return None,
        };
        Some(start + self.ticks_ns.load(Ordering::Relaxed))
    }
}

const NS_PER_SEC: u64 = 1_000_000_000;

/// `None` for a negative or unnormalized timespec.
pub fn timespec_ns(ts: &libc::timespec) -> Option<u64> {
    let secs = u64::try_from(ts.tv_sec).ok()?;
    let nanos = u64::try_from(ts.tv_nsec)
        .ok()
        .filter(|&nanos| nanos < NS_PER_SEC)?;
    secs.checked_mul(NS_PER_SEC)?.checked_add(nanos)
}

fn real_clock(clock: libc::clockid_t) -> Option<ClockId> {
    match clock {
        libc::CLOCK_MONOTONIC | libc::CLOCK_MONOTONIC_RAW | libc::CLOCK_MONOTONIC_COARSE => {
            Some(ClockId::Monotonic)
        }
        libc::CLOCK_REALTIME | libc::CLOCK_REALTIME_COARSE => Some(ClockId::Realtime),
        _ => None,
    }
}

pub fn real_monotonic() -> Duration {
    let ts = rustix::time::clock_gettime(ClockId::Monotonic);
    Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
}
