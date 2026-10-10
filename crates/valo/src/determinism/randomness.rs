// TODO(ai-review): generated, not yet audited
//! Deterministic randomness: `getrandom`, `getentropy` and reads of
//! `/dev/urandom`/`/dev/random` all draw from one xorshift64* stream seeded
//! with the virtual realtime epoch. Games seed their RNG from these at startup
//! (Unity reads `/dev/urandom`). The state is a plain static, so it rewinds
//! with every restore.
//!
//! `getentropy` needs its own hook: glibc implements it with the raw syscall.
//! Each device open gets its own pre-filled memfd, which the checkpoint fd
//! sync rewinds like any fd; reading past [`DEVICE_BYTES`] hits EOF.

use std::ffi::CStr;
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use rustix::fs::{MemfdFlags, SeekFrom};
use rustix::io::Errno;

use valo_ipc::config::RandomnessConfig;

use crate::debug;

static UNCONTROLLED_RANDOMNESS: AtomicBool = AtomicBool::new(false);

/// Bytes a substituted device fd holds; games read a few dozen at startup.
pub(crate) const DEVICE_BYTES: usize = 1 << 20;

/// Fills `out` from the stream (`getrandom`, `getentropy`).
pub(crate) fn fill(out: &mut [u8]) {
    for chunk in out.chunks_mut(8) {
        chunk.copy_from_slice(&next().to_le_bytes()[..chunk.len()]);
    }
}

/// Whether opening `path` must be served from the stream.
pub(crate) fn is_device(path: &CStr) -> bool {
    if !deterministic() {
        return false;
    }

    let path = path.to_bytes();
    path == b"/dev/urandom" || path == b"/dev/random"
}

/// Stands in for an opened randomness device: a memfd holding
/// [`DEVICE_BYTES`] of the stream, positioned at its start.
pub(crate) fn open_device(cloexec: bool) -> Result<OwnedFd, Errno> {
    let flags = if cloexec {
        MemfdFlags::CLOEXEC
    } else {
        MemfdFlags::empty()
    };
    let fd = rustix::fs::memfd_create("valo-urandom", flags)?;
    let mut block = [0u8; 4096];
    for _ in 0..DEVICE_BYTES / block.len() {
        fill(&mut block);
        let mut rest = &block[..];
        while !rest.is_empty() {
            let n = rustix::io::write(&fd, rest)?;
            rest = &rest[n..];
        }
    }
    rustix::fs::seek(&fd, SeekFrom::Start(0))?;
    debug!(Hooks, "randomness: device served as a memfd");
    Ok(fd)
}

pub(crate) fn set_config(config: &RandomnessConfig) {
    UNCONTROLLED_RANDOMNESS.store(config.uncontrolled_randomness, Ordering::Relaxed);
    debug!(
        Hooks,
        "det randomness: {}",
        if config.uncontrolled_randomness {
            "uncontrolled"
        } else {
            "deterministic"
        }
    );
}

pub(crate) fn effective() -> RandomnessConfig {
    RandomnessConfig {
        uncontrolled_randomness: UNCONTROLLED_RANDOMNESS.load(Ordering::Relaxed),
    }
}

pub(crate) fn deterministic() -> bool {
    !UNCONTROLLED_RANDOMNESS.load(Ordering::Relaxed)
}

/// Seed: the virtual realtime epoch in seconds (nonzero, as xorshift needs).
static STATE: AtomicU64 = AtomicU64::new(1_700_000_000);

fn next() -> u64 {
    let mut r = STATE.load(Ordering::Relaxed);
    r ^= r >> 12;
    r ^= r << 25;
    r ^= r >> 27;
    STATE.store(r, Ordering::Relaxed);
    r.wrapping_mul(0x2545_f491_4f6c_dd1d)
}
