//! ZST Proof tokens of invariants.

use std::cell::UnsafeCell;
use std::marker::PhantomData;
use std::ops::DerefMut;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::abort;

/// Proof that no other valo code runs concurrently.
pub struct Exclusive {
    _priv: PhantomData<*mut ()>,
}

impl Exclusive {
    /// # Safety
    /// - the caller must be the only thread running valo code
    /// - the value must not outlive that exclusivity.
    pub unsafe fn assume() -> Self {
        Self { _priv: PhantomData }
    }

    /// Enters the single checkpoint/restore session, aborts on reentry.
    pub fn enter_exclusive() -> impl DerefMut<Target = Exclusive> + Drop {
        static SESSION_ACTIVE: AtomicBool = AtomicBool::new(false);

        pub struct ExclusiveSession {
            excl: Exclusive,
        }

        impl Drop for ExclusiveSession {
            fn drop(&mut self) {
                SESSION_ACTIVE.store(false, Ordering::SeqCst);
            }
        }

        impl std::ops::Deref for ExclusiveSession {
            type Target = Exclusive;

            fn deref(&self) -> &Exclusive {
                &self.excl
            }
        }

        impl std::ops::DerefMut for ExclusiveSession {
            fn deref_mut(&mut self) -> &mut Exclusive {
                &mut self.excl
            }
        }

        if SESSION_ACTIVE.swap(true, Ordering::SeqCst) {
            abort!(Checkpoint, "checkpoint/restore already active");
        }

        ExclusiveSession {
            // SAFETY: the flag guarantees this is the only session.
            // Technically unsound, as enter_exclusive may be called
            // and the result be stashed away for later use.
            excl: unsafe { Exclusive::assume() },
        }
    }
}

/// A mutable static whose access requires proof of valo-code exclusivity.
pub struct ExclusiveCell<T> {
    value: UnsafeCell<T>,
}

// SAFETY: mutation goes only through the unforgeable `Exclusive` proof.
unsafe impl<T> Sync for ExclusiveCell<T> {}

impl<T> ExclusiveCell<T> {
    pub const fn new(value: T) -> Self {
        Self {
            value: UnsafeCell::new(value),
        }
    }

    pub fn get<'a>(&self, _proof: &'a mut Exclusive) -> &'a mut T {
        // SAFETY: the proof is unforgeable and held by `&mut`, so no other
        // reference into the cell can exist.
        unsafe { &mut *self.value.get() }
    }
}

/// Proof that every other thread is suspended.
pub struct Frozen {
    _priv: PhantomData<*mut ()>,
}

impl Frozen {
    /// # Safety
    /// - every other thread must be suspended, and the value must not
    ///   outlive their suspension.
    pub unsafe fn assume() -> Self {
        Self { _priv: PhantomData }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_allows_mutation_only_through_the_proof() {
        static CELL: ExclusiveCell<u32> = ExclusiveCell::new(0);
        // SAFETY: the test thread is the only valo code running.
        let mut excl = unsafe { Exclusive::assume() };
        *CELL.get(&mut excl) = 7;
        assert_eq!(*CELL.get(&mut excl), 7);
    }
}
