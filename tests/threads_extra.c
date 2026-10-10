//# link_flags: -pthread
//# max_restores: 1
// TODO(ai-review): generated, not yet audited
// A thread created after the checkpoint must be terminated on restore:
// the restored process keeps only the threads the savestate knows about.
#include <assert.h>
#include <pthread.h>
#include <stdatomic.h>
#include <unistd.h>
#include "testlib.h"

static atomic_int worker_ready;
static atomic_int extra_ready;
static atomic_int phase;
static atomic_int extra_stop;
static atomic_int extra_writes;
static atomic_int result;

static void *worker(void *arg) {
    (void)arg;
    volatile int stack_value = 40;
    atomic_fetch_add_explicit(&worker_ready, 1, memory_order_release);
    while (atomic_load_explicit(&phase, memory_order_acquire) == 0) {}
    stack_value += 1;
    atomic_store_explicit(&result, stack_value, memory_order_release);
    while (atomic_load_explicit(&phase, memory_order_acquire) == 1) {}
    return NULL;
}

static void *extra(void *arg) {
    (void)arg;
    atomic_fetch_add_explicit(&extra_ready, 1, memory_order_release);
    while (!atomic_load_explicit(&extra_stop, memory_order_acquire)) {
        atomic_fetch_add_explicit(&extra_writes, 1, memory_order_relaxed);
    }
    return NULL;
}


int main() {
    pthread_t worker_thread;
    assert(pthread_create(&worker_thread, NULL, worker, NULL) == 0);
    wait_for_threads(&worker_ready, 1);

    valo_checkpoint();

    // Rewound timeline: the extra thread of the previous pass must be gone.
    usleep(100 * 1000);
    assert(task_count() == 2);
    assert(atomic_load_explicit(&extra_writes, memory_order_acquire) == 0);

    // Spawn a thread the savestate does not know about.
    pthread_t extra_thread;
    assert(pthread_create(&extra_thread, NULL, extra, NULL) == 0);
    wait_for_threads(&extra_ready, 1);

    // The worker must still replay from its saved context.
    atomic_store_explicit(&phase, 1, memory_order_release);
    while (atomic_load_explicit(&result, memory_order_acquire) != 41) {}
    tprintf("worker replayed, extra alive\n");

    valo_restore();
}
