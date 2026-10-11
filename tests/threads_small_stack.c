//# link_flags: -pthread
//# max_restores: 2
// The park frame descends to a fixed depth above stack_low, so a thread
// must suspend and replay even while running deeper than half its stack.
// A 48 KiB stack is the smallest the suspend tracker accepts; the restore
// below freezes such a thread at ~26 KiB above stack_low - deeper than
// the park depth plus its frames, well inside the working set of a small
// stack, and beyond where the park alloca must not give up.
#define _GNU_SOURCE
#include <pthread.h>
#include <assert.h>
#include <stdatomic.h>
#include <stdint.h>
#include "testlib.h"

static atomic_int started;
static atomic_int phase;
static atomic_int deep_result;

static void *stack_base;

// Recurses until its frame sits at most 26 KiB above stack_low, then
// parks deep on the phase spin. Level size does not matter: the depth
// is bounded by the live stack pointer, not a level count.
static __attribute__((noinline)) void descend(volatile int depth) {
    volatile char pad[1024];
    pad[0] = (char)depth;
    if (depth == 0 || (uintptr_t)&pad[0] - (uintptr_t)stack_base <= 26 * 1024) {
        atomic_store(&deep_result, 1);
        while (atomic_load(&phase) != 2) {}
        return;
    }
    descend(depth - 1);
    pad[1] = (char)depth;
    while (atomic_load(&phase) != 2) {}
}

static void *thread_main(void *arg) {
    (void)arg;
    pthread_attr_t attr;
    assert(pthread_getattr_np(pthread_self(), &attr) == 0);
    void *base;
    size_t size;
    assert(pthread_attr_getstack(&attr, &base, &size) == 0);
    stack_base = base;
    pthread_attr_destroy(&attr);

    thread_ready(&started);
    while (atomic_load(&phase) == 0) {}
    descend(1000);
    atomic_store(&deep_result, 2);
    return NULL;
}

int main(void) {
    pthread_attr_t attr;
    assert(pthread_attr_init(&attr) == 0);
    assert(pthread_attr_setstacksize(&attr, 48 * 1024) == 0);
    pthread_t thread;
    assert(pthread_create(&thread, &attr, thread_main, NULL) == 0);
    wait_for_threads(&started, 1);

    // Save while the thread is shallow; each replay returns here with
    // phase and deep_result rewound, so the thread descends again and the
    // next restore freezes it deep once more.
    valo_checkpoint();

    release_threads(&phase);
    while (atomic_load(&deep_result) != 1) {}
    tprintf("thread deep on small stack\n");

    valo_restore();
    return 2;
}
