//! ZST Proof tokens of invariants.

use std::cell::UnsafeCell;
use std::marker::PhantomData;

/// Proof that no other valo code runs concurrently.
pub struct Exclusive {
    _priv: PhantomData<*mut ()>,
}

impl Exclusive {
    /// # Safety
    /// - the caller must be the only thread running valo code, and the
    ///   value must not outlive that exclusivity.
    pub unsafe fn assume() -> Self {
        Self { _priv: PhantomData }
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
