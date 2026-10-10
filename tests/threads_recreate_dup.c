//# link_flags: -pthread
//# max_restores: 3
// TODO(ai-review): generated, not yet audited
// A thread that dies after the checkpoint is recreated by the first restore.
// The recreated copy must be the only one across every later restore from
// the same slot. The copy parks during each restore's suspend pass, so
// cloning it again would leave two kernel threads running the same logical
// thread (same stack, same TLS) - the Hollow Knight crash signature.
//
// Pass 0 (original timeline): the worker honors the kill flag and raw-exits;
//   restore #1 recreates it and stamps its fresh tid into the glibc
//   descriptor.
// Pass 1: the recreation survives (birth tid guard); restore #2 signals it
//   through the stamped descriptor and resumes it - the restore's rewind
//   then resets the stamped tid back to the dead checkpoint value.
// Pass 2: restore #3 must still reach the copy: a thread that cannot be
//   signaled through its descriptor anymore looks dead to the suspend scan,
//   gets skipped, and is cloned again - a duplicate.
// Pass 3: task_count() must still be 2 (main + one copy).
//
// Buggy output ends with "tasks=3": the third restore cloned a duplicate
// of the live copy because the suspend scan saw a dead tid in its glibc
// descriptor. The worker decides its own death (birth tid guard), so the
// test does not depend on pthread_kill semantics from the guest side.
#include <assert.h>
#include <pthread.h>
#include <signal.h>
#include <stdatomic.h>
#include <sys/syscall.h>
#include <unistd.h>
#include "testlib.h"

static atomic_int worker_ready;
static atomic_int phase;
static pid_t birth_tid;
static atomic_int kill_flag;
static atomic_int kill_taken;

// The loop body is identical for every copy of this logical thread: it only
// reads shared state and calls gettid, so even two duplicate copies running
// the same stack and TLS stay benign and the buggy run still reaches the
// deterministic task_count() check below.
static void *worker(void *arg) {
    (void)arg;
    birth_tid = syscall(SYS_gettid);
    atomic_fetch_add_explicit(&worker_ready, 1, memory_order_release);
    while (atomic_load_explicit(&phase, memory_order_acquire) == 0) {
        // Every copy acknowledges the kill flag; only the original thread
        // (birth tid) raw-exits, a recreation stays alive so the next restore
        // faces a live copy.
        if (atomic_load_explicit(&kill_flag, memory_order_acquire) != 0) {
            atomic_store_explicit(&kill_taken, 1, memory_order_release);
            if (syscall(SYS_gettid) == birth_tid) {
                syscall(SYS_exit, 0);
            }
        }
    }
    return NULL;
}


int main() {
    pthread_t worker_thread;
    assert(pthread_create(&worker_thread, NULL, worker, NULL) == 0);
    wait_for_threads(&worker_ready, 1);

    valo_checkpoint();

    // Pass 0: the original worker raw-exits on the kill flag. Rewound passes:
    // the recreation takes the kill but stays alive (birth tid guard). Both
    // take the flag promptly, so waiting on the acknowledgment is fast and
    // only the death confirmation needs a bound.
    atomic_store_explicit(&kill_flag, 1, memory_order_release);
    while (atomic_load_explicit(&kill_taken, memory_order_acquire) == 0) {}
    // Death is imminent once the kill was taken; the 50-iteration bound only
    // guards against scheduler stalls, so pass 0 confirms within a few
    // iterations and the rewound passes run out harmlessly.
    for (int i = 0; i < 50 && pthread_kill(worker_thread, 0) == 0; ++i) {
        usleep(1000);
    }
    tprintf("tasks=%d\n", task_count());

    valo_restore();
}
