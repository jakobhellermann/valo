use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use valo_ipc::config::{Framerate, TimeConfig};

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

        let num = u64::from(self.framerate_num.load(Ordering::Relaxed));
        let denom = u64::from(self.framerate_denom.load(Ordering::Relaxed));
        let scaled = 1_000_000_000 * denom + self.remainder.load(Ordering::Relaxed);

        let frame_ns = scaled / num;
        self.remainder.store(scaled % num, Ordering::Relaxed);

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
        self.slept_ns.fetch_add(ns, Ordering::Relaxed);
        self.ticks_ns.fetch_add(ns, Ordering::Relaxed);
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

pub fn real_monotonic() -> Duration {
    let ts = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
}
