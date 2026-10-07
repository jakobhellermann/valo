//! Threads handling for checkpoint/restore

#[derive(Default)]
pub struct SavedThreads();

pub(crate) fn suspend_others(_expected: Option<&SavedThreads>) {
    // TODO
}

pub(crate) fn recreate_missing() {
    // TODO
}

pub(crate) fn resume_others() {
    // TODO
}

pub(crate) fn saved_threads() -> SavedThreads {
    // TODO
    SavedThreads()
}
