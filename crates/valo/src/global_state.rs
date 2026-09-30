use std::ffi::CStr;

use std::cell::SyncUnsafeCell;
use std::mem::MaybeUninit;
use std::ops::Range;
use std::ptr::addr_of_mut;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use rustix::mm;

use crate::Result;
use crate::thread_tracker::ThreadRegistry;

fn parse_env_u64(name: &CStr) -> u64 {
    // SAFETY: name is a valid C string, and we don't modify the environment
    let ptr = unsafe { libc::getenv(name.as_ptr()) };
    if ptr.is_null() {
        return 0;
    }
    // SAFETY: getenv returns a valid C string if non-null
    let s = unsafe { CStr::from_ptr(ptr) };
    s.to_str().ok().and_then(|s| s.parse().ok()).unwrap_or(0)
}
use crate::alt_stack::ALT_STACK_SIZE;
use crate::utils::PAGE_SIZE;

static GLOBAL_STATE: SyncUnsafeCell<MaybeUninit<GlobalState>> =
    SyncUnsafeCell::new(MaybeUninit::zeroed());

#[derive(Default)]
pub struct GlobalState {
    pub reserved: *mut ReservedMemory,
    pub restore_in_progress: AtomicBool,
    pub snapshot_slot: AtomicU32,

    pub frame_count: AtomicU64,
    /// 0 means unlimited
    pub max_restores: u64,
    /// 0 means disabled
    pub checkpoint_frame: u64,
    /// 0 means disabled
    pub restore_frame: u64,
}
// SAFETY: only access through `GlobalState::get`
unsafe impl Send for GlobalState {}
// SAFETY: only access through `GlobalState::get`
unsafe impl Sync for GlobalState {}

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
}

#[repr(C)]
pub struct ReservedMemory {
    pub alt_stack: [u8; ALT_STACK_SIZE],
    pub restore_count: AtomicU64,
    pub thread_registry: Mutex<ThreadRegistry>,
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
                addr_of_mut!((*(inner.cast::<ReservedMemory>())).thread_registry)
                    .write(Mutex::new(ThreadRegistry::new()));
            }
            inner
        };
        let max_restores = parse_env_u64(c"VALO_MAX_RESTORES");
        let checkpoint_frame = parse_env_u64(c"VALO_CHECKPOINT_FRAME");
        let restore_frame = parse_env_u64(c"VALO_RESTORE_FRAME");

        Ok(GlobalState {
            reserved: reserved.cast(),
            restore_in_progress: AtomicBool::new(false),
            snapshot_slot: AtomicU32::new(0),
            frame_count: AtomicU64::new(0),
            max_restores,
            checkpoint_frame,
            restore_frame,
        })
    }

    pub fn alt_stack_memory(&self) -> Range<*const ()> {
        // SAFETY: inbounds
        unsafe {
            let addr = addr_of_mut!((*self.reserved).alt_stack).cast();
            addr..addr.byte_add(ALT_STACK_SIZE)
        }
    }
}
