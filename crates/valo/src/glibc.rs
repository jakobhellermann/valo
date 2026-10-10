// TODO(ai-review): generated, not yet audited
//! glibc's private `struct pthread`: the offset of its `tid` field, learned
//! at runtime instead of hardcoding a layout valo does not control.

use std::sync::atomic::{AtomicUsize, Ordering};

use crate::{abort, sys};

/// Offset of the `tid` field inside glibc's private `struct pthread`, in
/// bytes from the descriptor base (which equals the pthread_t and the TLS
/// base). Recreated threads get their fresh tid stamped there through
/// CLONE_PARENT_SETTID, so pthread_kill and pthread_join address them like
/// any other pthread. Computed once in threads::init_main, before any other
/// thread exists. The static lives in rewound memory, but the snapshot was
/// taken after init, so the restored value is always the computed one.
static TID_OFFSET: AtomicUsize = AtomicUsize::new(0);

/// First candidate for the tid offset on current x86_64 glibc builds
/// (180 pid_t members); only used as the checked guess before scanning.
const TID_OFFSET_GUESS: usize = 180 * std::mem::size_of::<libc::pid_t>();

pub(crate) fn tid_offset() -> usize {
    // Written once by init_main before any other thread exists.
    let known = TID_OFFSET.load(Ordering::Relaxed);
    if known != 0 {
        return known;
    }
    // SAFETY: pthread_self identifies this live thread, so the descriptor it
    // returns is fully mapped for the scan below.
    let (pthread, tid) = sys::current_thread_ids();
    let pd = pthread as *const libc::pid_t;
    let mut offset = TID_OFFSET_GUESS;
    // SAFETY: the descriptor of a live thread extends well past 720 bytes,
    // and no other thread exists yet that could tear the read.
    if unsafe {
        pd.add(TID_OFFSET_GUESS / std::mem::size_of::<libc::pid_t>())
            .read()
    } != tid
    {
        offset = 0;
        for i in 0..500usize {
            // SAFETY: bounded to 2000 bytes, which current glibc descriptors
            // exceed; a smaller descriptor would fault here before anything
            // else can go wrong.
            if unsafe { pd.add(i).read() } == tid {
                offset = i * std::mem::size_of::<libc::pid_t>();
                break;
            }
        }
    }
    if offset == 0 {
        abort!(
            Init,
            "cannot locate the tid field in glibc's pthread descriptor"
        );
    }
    TID_OFFSET.store(offset, Ordering::Relaxed);
    offset
}

/// Write `tid` into the `tid` field of the descriptor at `pd`.
///
/// # Safety
/// - `pd` must be the base of a live glibc thread descriptor (`pthread_t`),
///   so `pd + tid_offset()` is its writable `tid` field.
pub(crate) unsafe fn write_tid(pd: usize, tid: libc::pid_t) {
    // SAFETY: per the contract above the field is writable, and the aligned
    // pid_t store cannot tear.
    unsafe { ((pd + tid_offset()) as *mut libc::pid_t).write_volatile(tid) };
}
