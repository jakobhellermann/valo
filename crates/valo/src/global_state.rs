use std::cell::SyncUnsafeCell;

use std::mem::MaybeUninit;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use rustix::mm;

use crate::Result;

use crate::alt_stack::ALT_STACK_SIZE;
use crate::token::ExclusiveCell;
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
        crate::log::set_level(config.log.level.into());
    }
}

/// Memory excluded from checkpoint snapshots: only state that must survive
/// restores unchanged lives here. Everything that rewinds with process memory
/// (thread table, saved contexts) lives in normal statics instead.
#[repr(C)]
pub struct ReservedMemory {
    /// In use by the restore signal handler while memory is being loaded.
    pub alt_stack: SyncUnsafeCell<[u8; ALT_STACK_SIZE]>,
    pub exclusive: ExclusiveCell<ReservedExclusive>,
}

pub struct ReservedExclusive {
    /// Accumulates across restores and must therefore not be rewound.
    pub restore_count: u64,
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

        Ok(GlobalState {
            reserved,
            restore_in_progress: AtomicBool::new(false),
            snapshot_slot: AtomicU32::new(0),
            frame_count: AtomicU64::new(0),
            max_restores: AtomicU64::new(0),
            checkpoint_frame: AtomicU64::new(0),
            restore_frame: AtomicU64::new(0),
            use_gpu: AtomicBool::new(false),
        })
    }

    pub fn alt_stack_memory(&self) -> Range<*const ()> {
        let addr = self.reserved.alt_stack.get().cast();
        addr..addr.wrapping_byte_add(ALT_STACK_SIZE)
    }
}
