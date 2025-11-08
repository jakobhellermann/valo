use std::cell::SyncUnsafeCell;
use std::mem::MaybeUninit;
use std::ops::Range;
use std::ptr::addr_of_mut;

use rustix::mm;

use crate::Result;
use crate::alt_stack::ALT_STACK_SIZE;
use crate::utils::PAGE_SIZE;

static GLOBAL_STATE: SyncUnsafeCell<MaybeUninit<GlobalState>> =
    SyncUnsafeCell::new(MaybeUninit::uninit());

#[derive(Default)]
pub struct GlobalState {
    pub reserved: *mut ReservedMemory,
}
unsafe impl Send for GlobalState {}
unsafe impl Sync for GlobalState {}

#[repr(C)]
pub struct ReservedMemory {
    pub alt_stack: [u8; ALT_STACK_SIZE],
}

pub unsafe fn set_global_state(global_state: GlobalState) {
    unsafe {
        *GLOBAL_STATE.get() = MaybeUninit::new(global_state);
    }
}
pub unsafe fn get_global_state() -> &'static mut GlobalState {
    unsafe { (*GLOBAL_STATE.get()).assume_init_mut() }
}

impl GlobalState {
    pub fn new() -> Result<GlobalState> {
        let len = std::mem::size_of::<ReservedMemory>().next_multiple_of(PAGE_SIZE);
        let reserved = unsafe {
            let addr = mm::mmap_anonymous(
                std::ptr::null_mut(),
                len + 2 * PAGE_SIZE,
                mm::ProtFlags::empty(),
                mm::MapFlags::PRIVATE,
            )?;
            let inner = addr.add(PAGE_SIZE);
            mm::mprotect(
                inner,
                len,
                mm::MprotectFlags::READ | mm::MprotectFlags::WRITE,
            )?;
            libc::memset(inner, 0, len);
            inner
        };
        Ok(GlobalState {
            reserved: reserved.cast(),
        })
    }

    pub fn alt_stack_memory(&self) -> Range<*const ()> {
        let addr = unsafe { addr_of_mut!((*self.reserved).alt_stack) }.cast();
        addr..unsafe { addr.byte_add(ALT_STACK_SIZE) }
    }
}
