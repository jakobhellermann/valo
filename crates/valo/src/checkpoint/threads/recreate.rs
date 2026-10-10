// TODO(ai-review): generated, not yet audited
//! Recreate tracked threads whose kernel thread died since the checkpoint.

use std::ffi::c_void;
use std::sync::atomic::Ordering;

use crate::global_state::GlobalState;
use crate::token::Exclusive;
use crate::{abort, sys, thread_tracker};

use super::{saved_context, wait_for_resume};

/// Recreate threads that the savestate knows about but that did not park
/// during this restore's suspension: their kernel threads are gone. Threads
/// that parked — the original thread or a recreation from an earlier restore
/// of the same snapshot — resume from their saved contexts through the park
/// gate instead; cloning them again would duplicate the logical thread.
pub(crate) fn recreate_missing(exclusive: &mut Exclusive) {
    let self_id = thread_tracker::current_id();
    let parked = GlobalState::parked_threads(exclusive);
    let recorded = &parked.slots[..parked.count];
    for (id, slot, _) in thread_tracker::running_threads() {
        if id == self_id {
            continue;
        }
        if recorded.contains(&(slot as u32)) {
            continue;
        }
        recreate(slot);
    }
}

fn recreate(slot: usize) {
    let target = thread_tracker::slot(slot);
    let tid = target.tid.load(Ordering::SeqCst);
    let stack_low = target.stack_low.load(Ordering::SeqCst);

    // The child enters below the saved stack pointer so its start frame
    // cannot clobber the rewound frames above it. 16-byte aligned: after
    // the two trampoline pops and the call, park_recreated starts with the
    // ABI-mandated rsp ≡ 8 (mod 16) at function entry.
    // SAFETY: the context was written at checkpoint and lives for the
    // process lifetime.
    let saved_sp =
        unsafe { (*saved_context(slot)).uc_mcontext.gregs[libc::REG_RSP as usize] } as usize;
    let entry = (saved_sp - 128) & !0xF;

    let clear_child_tid = target.clear_child_tid.load(Ordering::SeqCst);
    if clear_child_tid == 0 {
        abort!(Restore, "thread {tid} has no clear_child_tid address");
    }
    // CLONE_CHILD_CLEARTID wires the kernel exit futex exactly where the
    // original thread registered it. CLONE_PARENT_SETTID stamps the fresh tid
    // into the glibc descriptor's own tid field (not the clear-tid futex,
    // which is a different member of struct pthread): the kernel writes it at
    // clone time, so pthread_kill and pthread_join address the recreated
    // thread like any other pthread, whatever tid it runs on now. Stamping
    // the tid through the kernel avoids hardcoding glibc's private struct
    // pthread layout: only the offset is learned at runtime.
    let mut set_tid: libc::pid_t = tid;
    let pd_tid = target.pthread.load(Ordering::SeqCst) + crate::glibc::tid_offset();
    let mut args = sys::CloneArgs {
        flags: (libc::CLONE_VM
            | libc::CLONE_FS
            | libc::CLONE_FILES
            | libc::CLONE_SIGHAND
            | libc::CLONE_THREAD
            | libc::CLONE_SYSVSEM
            | libc::CLONE_SETTLS
            | libc::CLONE_CHILD_CLEARTID
            | libc::CLONE_PARENT_SETTID) as u64,
        pidfd: 0,
        child_tid: clear_child_tid as u64,
        parent_tid: pd_tid as u64,
        exit_signal: 0,
        stack: stack_low as u64,
        stack_size: (entry - stack_low) as u64,
        tls: target.fs_base.load(Ordering::SeqCst) as u64,
        set_tid: &raw mut set_tid as u64,
        set_tid_size: 1,
        cgroup: 0,
    };
    // The child's rsp starts at entry and pops upward: fn first, then arg.
    // SAFETY: entry and entry+8 lie in the child's reserved stack below the
    // rewound frames; the child pops both values before anything else
    // writes there.
    unsafe {
        (entry as *mut u64).write(park_recreated as *const () as u64);
        (entry as *mut u64).add(1).write(slot as u64);
    }
    // SAFETY: the stack region was mapped by the loader and belongs to this
    // slot; the trampoline never returns to this frame.
    let mut parent = unsafe { sys::clone3_child(&args) };
    if parent < 0 {
        let errno = -parent;
        // EPERM: selecting a specific tid requires CAP_CHECKPOINT_RESTORE
        // (libTAS falls back to writing /proc/sys/kernel/ns_last_pid, which
        // needs privileges too). EEXIST: the kernel recycled the dead tid
        // meanwhile. Both recover the same way - clone with a fresh tid and
        // keep the tracker's signaling target in sync; the kernel stamps the
        // fresh tid into the descriptor (CLONE_PARENT_SETTID) either way, so
        // pthread_kill still reaches the recreation.
        if errno == libc::EPERM as i64 || errno == libc::EEXIST as i64 {
            args.set_tid = 0;
            args.set_tid_size = 0;
            // SAFETY: same contract as above.
            parent = unsafe { sys::clone3_child(&args) };
            if parent > 0 {
                target.tid.store(parent as libc::pid_t, Ordering::SeqCst);
                crate::warn!(
                    Restore,
                    "tid {tid} not preserved (errno {errno}); recreated with tid {parent}"
                );
            }
        }
    }
    if parent < 0 {
        abort!(Restore, "cannot recreate thread {tid}: errno={}", -parent);
    }
    // Record the live tid for the next suspend pass: the memory rewind of a
    // later restore resets the descriptor back to the dead checkpoint tid,
    // and the restamp from this record is what keeps pthread_kill reaching
    // the recreation - deterministically, without relying on it having been
    // scheduled in between.
    let record = &GlobalState::reserved_shared().live_tids[slot];
    record.tid.store(parent as libc::pid_t, Ordering::SeqCst);
    record
        .thread_id
        .store(target.thread_id().raw(), Ordering::SeqCst);
}

extern "C" fn park_recreated(arg: *mut c_void) {
    let state = GlobalState::get();
    // The gate opens in resume_others; the recreated thread is not part of
    // the suspension accounting and must not post the parked semaphore.
    wait_for_resume();
    if state.restore_in_progress() {
        // SAFETY: getcontext wrote this slot at checkpoint; the selected
        // snapshot restored it.
        unsafe { libc::setcontext(saved_context(arg as usize)) };
    }
    abort!("recreated thread resumed outside restore");
}
