mod atomic_ptr_vec;

use std::ops::Range;

pub(crate) use atomic_ptr_vec::AtomicPtrVec;

#[repr(transparent)]
pub struct UnsafeSendSync<T>(pub T);
// SAFETY: user has to make sure the inner value can be send/sync
unsafe impl<T> Send for UnsafeSendSync<T> {}
// SAFETY: user has to make sure the inner value can be send/sync
unsafe impl<T> Sync for UnsafeSendSync<T> {}

pub const PAGE_SIZE: usize = 4096;

pub fn assert_stack_in_range(range: Range<*const ()>) {
    let local_var = 0;
    let local_addr = &raw const local_var;
    if !range.contains(&local_addr.cast()) {
        todo!();
    }
}

#[cfg(feature = "assert-no-alloc")]
pub fn assert_no_alloc<T, F: FnOnce() -> T>(f: F) -> T {
    assert_no_alloc::assert_no_alloc(f)
}

#[cfg(not(feature = "assert-no-alloc"))]
pub fn assert_no_alloc<T, F: FnOnce() -> T>(f: F) -> T {
    f()
}
