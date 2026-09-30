use std::sync::atomic::{AtomicPtr, Ordering};

/// Fixed-capacity, lock-free, signal-handler-safe collection of pointers.
pub struct AtomicPtrVec<const N: usize> {
    ptrs: [AtomicPtr<()>; N],
}

impl<const N: usize> AtomicPtrVec<N> {
    pub const fn new() -> Self {
        Self {
            // SAFETY: AtomicPtr<()> with null is all zeros
            ptrs: unsafe { std::mem::zeroed() },
        }
    }

    pub fn push(&self, ptr: *const ()) -> Result<(), ()> {
        for slot in &self.ptrs {
            if slot
                .compare_exchange(
                    std::ptr::null_mut(),
                    ptr as *mut (),
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                )
                .is_ok()
            {
                return Ok(());
            }
        }
        Err(())
    }

    pub fn remove(&self, ptr: *const ()) -> bool {
        if ptr.is_null() {
            return false;
        }

        self.ptrs.iter().any(|slot| {
            slot.compare_exchange(
                ptr as *mut (),
                std::ptr::null_mut(),
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
        })
    }

    pub fn len(&self) -> usize {
        self.iter().count()
    }

    pub fn iter(&self) -> impl Iterator<Item = *mut ()> + '_ {
        self.ptrs
            .iter()
            .map(|p| p.load(Ordering::SeqCst))
            .filter(|p| !p.is_null())
    }
}

// SAFETY: only stores raw pointers atomically
unsafe impl<const N: usize> Send for AtomicPtrVec<N> {}
// SAFETY: all access is through atomics
unsafe impl<const N: usize> Sync for AtomicPtrVec<N> {}
