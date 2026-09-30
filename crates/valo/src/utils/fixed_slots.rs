/// Fixed-capacity storage for copyable records. Synchronization and identity
/// belong to the caller; this only manages occupied slots.
pub(crate) struct FixedSlots<T: Copy, const N: usize> {
    slots: [Option<T>; N],
}

impl<T: Copy, const N: usize> FixedSlots<T, N> {
    pub(crate) const fn new() -> Self {
        Self { slots: [None; N] }
    }

    pub(crate) fn insert(&mut self, value: T) -> Result<(), T> {
        let Some(slot) = self.slots.iter_mut().find(|slot| slot.is_none()) else {
            return Err(value);
        };
        *slot = Some(value);
        Ok(())
    }

    pub(crate) fn find_mut(&mut self, mut matches: impl FnMut(&T) -> bool) -> Option<&mut T> {
        self.slots.iter_mut().flatten().find(|value| matches(value))
    }

    pub(crate) fn remove(&mut self, mut matches: impl FnMut(&T) -> bool) -> Option<T> {
        let slot = self
            .slots
            .iter_mut()
            .find(|slot| slot.as_ref().is_some_and(&mut matches))?;
        slot.take()
    }
}
