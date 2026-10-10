//! Deterministic randomness:
//! Drawn from a xorshift64* stream

use std::ffi::CStr;
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use rustix::fs::MemfdFlags;
use rustix::io::Errno;

use valo_ipc::config::RandomnessConfig;

const DEVICE_BYTES: u64 = 1 << 20;

pub static RANDOM: RandomSource = RandomSource {
    uncontrolled: AtomicBool::new(false),
    state: AtomicU64::new(1),
};

pub struct RandomSource {
    uncontrolled: AtomicBool,
    state: AtomicU64,
}

impl RandomSource {
    pub fn set_config(&self, config: &RandomnessConfig) {
        self.uncontrolled
            .store(config.uncontrolled_randomness, Ordering::Relaxed);
    }

    pub fn config(&self) -> RandomnessConfig {
        RandomnessConfig {
            uncontrolled_randomness: self.uncontrolled.load(Ordering::Relaxed),
        }
    }

    pub fn is_controlled(&self) -> bool {
        !self.uncontrolled.load(Ordering::Relaxed)
    }

    pub fn fill(&self, out: &mut [u8]) {
        fill_with(out, || self.next());
    }

    /// Whether opening `path` must be served from the stream.
    pub fn should_hook_random_device(&self, path: &CStr) -> bool {
        self.is_controlled() && matches!(path.to_bytes(), b"/dev/urandom" | b"/dev/random")
    }

    /// Stand in for an opened randomness device
    pub fn open_device(&self, cloexec: bool) -> Result<OwnedFd, Errno> {
        let flags = if cloexec {
            MemfdFlags::CLOEXEC
        } else {
            MemfdFlags::empty()
        };
        let fd = rustix::fs::memfd_create("valo-urandom", flags)?;
        // a zero state would stay zero
        let mut state = self.next() | 1;
        let mut page = [0u8; 4096];
        for offset in (0..DEVICE_BYTES).step_by(page.len()) {
            fill_with(&mut page, || {
                state = step(state);
                output(state)
            });
            if rustix::io::pwrite(&fd, &page, offset)? != page.len() {
                return Err(Errno::IO);
            }
        }
        Ok(fd)
    }

    fn next(&self) -> u64 {
        let previous = self
            .state
            .update(Ordering::Relaxed, Ordering::Relaxed, step);
        output(step(previous))
    }
}

fn fill_with(out: &mut [u8], mut next: impl FnMut() -> u64) {
    for chunk in out.chunks_mut(8) {
        chunk.copy_from_slice(&next().to_le_bytes()[..chunk.len()]);
    }
}

fn step(mut state: u64) -> u64 {
    state ^= state >> 12;
    state ^= state << 25;
    state ^= state >> 27;
    state
}

fn output(state: u64) -> u64 {
    state.wrapping_mul(0x2545_f491_4f6c_dd1d)
}
