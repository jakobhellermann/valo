//# link_flags: -pthread
//# max_restores: 2
// A live thread must replay from its saved context after restore.
#include <pthread.h>
#include <assert.h>
#include <stdatomic.h>
#include <stdint.h>
#include "testlib.h"

static atomic_int started;
static atomic_int phase;
static atomic_int result[2];

static void *thread_main(void *arg) {
    intptr_t id = (intptr_t)arg;
    volatile int stack_value = 40 + id;

    // Wait at a known point so the checkpoint captures the live thread stack.
    thread_ready(&started);
    while (atomic_load_explicit(&phase, memory_order_acquire) == 0) {}

    ++stack_value;
    atomic_store_explicit(&result[id], stack_value, memory_order_release);
    while (atomic_load_explicit(&phase, memory_order_acquire) == 1) {}
    return NULL;
}

int main(void) {
    pthread_t threads[2];
    for (intptr_t i = 0; i < 2; ++i) {
        assert(pthread_create(&threads[i], NULL, thread_main, (void *)i) == 0);
    }
    wait_for_threads(&started, 2);

    // Save while both threads still have their original stack values.
    valo_checkpoint();
    assert(atomic_load(&result[0]) == 0 && atomic_load(&result[1]) == 0);

    // After restore, both threads must replay this phase from saved contexts.
    release_threads(&phase);
    while (atomic_load_explicit(&result[0], memory_order_acquire) != 41) {}
    while (atomic_load_explicit(&result[1], memory_order_acquire) != 42) {}
    tprintf("threads: %d %d\n", atomic_load(&result[0]), atomic_load(&result[1]));

    // Each restore brings both threads back to the phase==0 wait.
    // The third call exits via VALO_MAX_RESTORES.
    valo_restore();
    return 2;
}
