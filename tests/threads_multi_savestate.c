//# link_flags: -pthread
//# max_restores: 1
// Loading A after saving B must replay the thread's earlier stack value.
#include <assert.h>
#include <pthread.h>
#include <stdatomic.h>
#include "testlib.h"

static atomic_int started;
static atomic_int phase;
static atomic_int result;

static void *thread_main(void *arg) {
    (void)arg;
    volatile int stack_value = 40;
    atomic_store(&started, 1);

    for (;;) {
        int command;
        while ((command = atomic_load(&phase)) == 0) {}
        stack_value += command;
        atomic_store(&result, stack_value);
        while (atomic_load(&phase) == command) {}
    }
}

static void run_phase(int command, int expected) {
    atomic_store(&phase, command);
    while (atomic_load(&result) == 0) {}
    assert(atomic_load(&result) == expected);
    tprintf("phase %d: %d\n", command, atomic_load(&result));
    atomic_store(&phase, 0);
    atomic_store(&result, 0);
}

int main(void) {
    pthread_t thread;
    assert(pthread_create(&thread, NULL, thread_main, NULL) == 0);
    while (atomic_load(&started) == 0) {}

    valo_checkpoint_slot(3);
    run_phase(1, 41);

    valo_checkpoint_slot(17);
    run_phase(2, 43);

    valo_restore_slot(3);
    return 1;
}
