//# link_flags: -pthread
//# max_restores: 1
// TODO(ai-review): generated, not yet audited
// A recreated thread must come back with its own TLS. The worker's
// __thread value lives in its TLS block (part of the thread's stack
// mapping, so the checkpoint captures it); the recreate must reinstall the
// thread's FS base via CLONE_SETTLS. Without it the recreated thread
// inherits the main thread's FS base and reads the main instance instead.
#include <assert.h>
#include <pthread.h>
#include <signal.h>
#include <stdatomic.h>
#include <sys/syscall.h>
#include <string.h>
#include <unistd.h>
#include "testlib.h"

static __thread int tls_value;

static atomic_int worker_ready;
static atomic_int phase;
static atomic_int result;

static void die_handler(int sig) {
    (void)sig;
    syscall(SYS_exit, 0);
}

static void *worker(void *arg) {
    (void)arg;
    tls_value = 7;
    volatile int stack_value = 40;
    atomic_fetch_add_explicit(&worker_ready, 1, memory_order_release);
    while (atomic_load_explicit(&phase, memory_order_acquire) == 0) {}
    stack_value += 1;
    // The worker's own TLS instance (7) is the recreate proof; the main
    // thread's instance (100) means the FS base was not restored.
    atomic_store_explicit(&result, stack_value + tls_value, memory_order_release);
    while (atomic_load_explicit(&phase, memory_order_acquire) == 1) {}
    return NULL;
}


int main() {
    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = die_handler;
    assert(sigaction(SIGUSR1, &sa, NULL) == 0);

    tls_value = 100;

    pthread_t worker_thread;
    assert(pthread_create(&worker_thread, NULL, worker, NULL) == 0);
    wait_for_threads(&worker_ready, 1);

    valo_checkpoint();

    // Both timelines run this block. On the rewound timeline the worker is
    // the recreated one: its result must still be built from its own TLS
    // instance (41 + 7 = 48), not the main thread's (41 + 100 = 141).
    atomic_store_explicit(&phase, 1, memory_order_release);
    while (atomic_load_explicit(&result, memory_order_acquire) == 0) {}
    assert(atomic_load_explicit(&result, memory_order_acquire) == 48);
    tprintf("tls ok: result=%d\n", atomic_load(&result));

    int killed_ok = pthread_kill(worker_thread, SIGUSR1) == 0;
    if (killed_ok) {
        for (int i = 0; i < 1000 && task_count() != 1; ++i) {
            usleep(1000);
        }
        assert(task_count() == 1);
    }

    valo_restore();
}
