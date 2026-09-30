mod atomic_ptr_vec;
mod track_run;

use std::ops::Range;

pub(crate) use atomic_ptr_vec::AtomicPtrVec;
#[allow(unused_imports)]
pub(crate) use track_run::TrackRun;

#[repr(transparent)]
pub struct UnsafeSendSync<T>(pub T);
// SAFETY: user has to make sure the inner value can be send/sync
unsafe impl<T> Send for UnsafeSendSync<T> {}
// SAFETY: user has to make sure the inner value can be send/sync
unsafe impl<T> Sync for UnsafeSendSync<T> {}

pub const PAGE_SIZE: usize = 4096;

/// Round down to the start of the containing page.
pub(crate) fn align_page_down(off: u64) -> u64 {
    off & !(PAGE_SIZE as u64 - 1)
}

/// Round up to the start of the next page (0 stays 0).
pub(crate) fn align_page_up(off: u64) -> u64 {
    off.div_ceil(PAGE_SIZE as u64)
        .saturating_mul(PAGE_SIZE as u64)
}

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
