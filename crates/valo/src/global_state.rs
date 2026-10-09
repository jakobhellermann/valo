use std::cell::SyncUnsafeCell;

use std::mem::MaybeUninit;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};

use rustix::mm;

use crate::Result;

use crate::alt_stack::ALT_STACK_SIZE;
use crate::checkpoint::fd_sync::keepalive::FdKeepaliveRegistry;
use crate::thread_tracker::MAX_THREADS;
use crate::token::{Exclusive, ExclusiveCell};
use crate::utils::PAGE_SIZE;
use valo_ipc::config::Config;

static GLOBAL_STATE: SyncUnsafeCell<MaybeUninit<GlobalState>> =
    SyncUnsafeCell::new(MaybeUninit::zeroed());

pub struct GlobalState {
    pub reserved: &'static ReservedMemory,
    pub restore_in_progress: AtomicBool,
    pub snapshot_slot: AtomicU32,

    pub frame_count: AtomicU64,
    /// 0 means unlimited
    pub max_restores: AtomicU64,
    /// 0 means disabled
    pub checkpoint_frame: AtomicU64,
    /// 0 means disabled
    pub restore_frame: AtomicU64,
    /// Fixed at init from the handshake config.
    pub use_gpu: AtomicBool,
    /// Prevent drawing.
    pub skip_draw: AtomicBool,
}

impl GlobalState {
    /// # Safety
    /// Must have unique access
    pub unsafe fn set(global_state: GlobalState) {
        // SAFETY: has unique access
        unsafe {
            *GLOBAL_STATE.get() = MaybeUninit::new(global_state);
        }
    }
    pub fn get() -> &'static GlobalState {
        // SAFETY: Since `set` was called in the .init_array, this will be initialized afterwards
        // Only immutable references are handed out.
        unsafe { (*GLOBAL_STATE.get()).assume_init_mut() }
    }

    pub fn reserved_shared() -> &'static ReservedShared {
        &GlobalState::get().reserved.shared
    }

    pub fn parked_threads(exclusive: &mut Exclusive) -> &mut ParkedThreads {
        &mut GlobalState::get()
            .reserved
            .exclusive
            .get(exclusive)
            .parked_threads
    }

    pub fn fd_keepalives(exclusive: &mut Exclusive) -> &mut FdKeepaliveRegistry {
        &mut GlobalState::get()
            .reserved
            .exclusive
            .get(exclusive)
            .fd_keepalives
    }

    pub fn set_restore_in_progress(&self, value: bool) {
        self.restore_in_progress.store(value, Ordering::SeqCst);
    }

    pub fn restore_in_progress(&self) -> bool {
        self.restore_in_progress.load(Ordering::SeqCst)
    }

    pub fn apply_config(&self, config: &Config) {
        self.max_restores
            .store(config.frames.max_restores, Ordering::Relaxed);
        self.checkpoint_frame
            .store(config.frames.checkpoint_frame, Ordering::Relaxed);
        self.restore_frame
            .store(config.frames.restore_frame, Ordering::Relaxed);
        self.use_gpu.store(config.use_gpu, Ordering::Relaxed);
        self.skip_draw.store(config.skip_draw, Ordering::Relaxed);
        crate::log::set_level(config.log.level.into());
    }
}

/// Threads that parked during the current restore's suspension pass. The
/// `suspend_others` pass records them before the memory rewind, and
/// `recreate_missing` consumes the list inside the restore signal handler:
/// a recorded slot is alive and resumes through the park gate instead of
/// being cloned again. Slot indexes stay valid across the rewind because
/// slot assignment is part of the rewound table's checkpoint state. Plain
/// fields: writer (before the raise) and reader (restore handler) are the
/// same thread, so program order orders the accesses.
#[repr(C)]
pub struct ParkedThreads {
    /// Number of valid entries in `slots`.
    pub count: usize,
    pub slots: [u32; MAX_THREADS],
}

/// Memory excluded from checkpoint snapshots: only state that must survive
/// restores unchanged lives here. Everything that rewinds with process memory
/// (thread table, saved contexts) lives in normal statics instead.
#[repr(C)]
pub struct ReservedMemory {
    /// In use by the restore signal handler while memory is being loaded.
    pub alt_stack: SyncUnsafeCell<[u8; ALT_STACK_SIZE]>,
    pub shared: ReservedShared,
    pub exclusive: ExclusiveCell<ReservedExclusive>,
}

pub struct ReservedShared {
    /// Monotonic thread id counter.
    pub next_thread_id: AtomicU64,
    /// Live kernel tid of the thread occupying each tracker slot, recorded
    /// when a restore recreates a thread. A later restore rewinds the glibc
    /// descriptor (ordinary memory) back to the dead checkpoint tid, so the
    /// suspend pass restamps the descriptor from here before it signals or
    /// probes through it; without the restamp, a live recreation that has
    /// not been scheduled since the rewind looks dead and would be cloned
    /// again.
    pub live_tids: [LiveTidRecord; MAX_THREADS],
}

#[repr(C)]
pub struct LiveTidRecord {
    /// ThreadId of the occupant the tid belongs to; guards the record
    /// against a slot reused by a later pthread.
    pub thread_id: AtomicU64,
    /// Kernel tid of the occupying thread.
    pub tid: AtomicI32,
}

pub struct ReservedExclusive {
    /// Accumulates across restores and must therefore not be rewound.
    pub restore_count: u64,
    /// Threads that parked in the current restore's suspend pass (see the
    /// struct docs): the "is alive" list recorded before the rewind.
    pub parked_threads: ParkedThreads,
    /// Duplicates of path-less non-restorable file descriptors
    pub fd_keepalives: FdKeepaliveRegistry,
}

impl GlobalState {
    pub fn new() -> Result<GlobalState> {
        let len = std::mem::size_of::<ReservedMemory>().next_multiple_of(PAGE_SIZE);
        let reserved = {
            // SAFETY: ptr is null
            let addr = unsafe {
                mm::mmap_anonymous(
                    std::ptr::null_mut(),
                    len + 2 * PAGE_SIZE,
                    mm::ProtFlags::empty(),
                    mm::MapFlags::PRIVATE,
                )?
            };
            // SAFETY: inbounds
            let inner = unsafe { addr.add(PAGE_SIZE) };
            // SAFETY: inner has unique provenance for the size
            unsafe {
                mm::mprotect(
                    inner,
                    len,
                    mm::MprotectFlags::READ | mm::MprotectFlags::WRITE,
                )?;
                libc::memset(inner, 0, len);
            }
            // SAFETY: inner points to allocated memory that fits a ReservedMemory
            // which is never unmapped.
            unsafe { &*inner.cast::<ReservedMemory>() }
        };

        reserved.shared.next_thread_id.store(1, Ordering::Relaxed);

        Ok(GlobalState {
            reserved,
            restore_in_progress: AtomicBool::new(false),
            snapshot_slot: AtomicU32::new(0),
            frame_count: AtomicU64::new(0),
            max_restores: AtomicU64::new(0),
            checkpoint_frame: AtomicU64::new(0),
            restore_frame: AtomicU64::new(0),
            use_gpu: AtomicBool::new(false),
            skip_draw: AtomicBool::new(false),
        })
    }

    pub fn alt_stack_memory(&self) -> Range<*const ()> {
        let addr = self.reserved.alt_stack.get().cast();
        addr..addr.wrapping_byte_add(ALT_STACK_SIZE)
    }
}
