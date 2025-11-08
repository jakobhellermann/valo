use std::ops::Range;

#[repr(transparent)]
pub struct SendSync<T>(pub T);
unsafe impl<T> Send for SendSync<T> {}
unsafe impl<T> Sync for SendSync<T> {}

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
