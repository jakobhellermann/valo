//# link_flags: -pthread
//# max_restores: 1
// TODO(ai-review): generated, not yet audited
// A thread that died after the checkpoint must be recreated on restore and
// replay from its saved context. The die signal handler exits the thread
// with a raw SYS_exit: no glibc exit path, no TLS access.
#include <assert.h>
#include <pthread.h>
#include <signal.h>
#include <stdatomic.h>
#include <sys/syscall.h>
#include <string.h>
#include <unistd.h>
#include "testlib.h"

static atomic_int worker_ready;
static atomic_int phase;
static atomic_int result;
static atomic_int kill_taken;
static pid_t birth_tid;

static void die_handler(int sig) {
    (void)sig;
    // Every copy acknowledges the kill; only the original thread (birth tid)
    // honors it. Recreate() stamps the fresh tid into the restored glibc
    // descriptor, so pthread_kill reaches a recreated worker too.
    atomic_store_explicit(&kill_taken, 1, memory_order_release);
    if (syscall(SYS_gettid) != birth_tid) {
        return;
    }
    syscall(SYS_exit, 0);
}

static void *worker(void *arg) {
    (void)arg;
    birth_tid = syscall(SYS_gettid);
    volatile int stack_value = 40;
    atomic_fetch_add_explicit(&worker_ready, 1, memory_order_release);
    while (atomic_load_explicit(&phase, memory_order_acquire) == 0) {}
    stack_value += 1;
    atomic_store_explicit(&result, stack_value, memory_order_release);
    while (atomic_load_explicit(&phase, memory_order_acquire) == 1) {}
    return NULL;
}


int main() {
    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = die_handler;
    assert(sigaction(SIGUSR1, &sa, NULL) == 0);

    pthread_t worker_thread;
    assert(pthread_create(&worker_thread, NULL, worker, NULL) == 0);
    wait_for_threads(&worker_ready, 1);

    valo_checkpoint();

    // Both timelines run this block: the first pass with the original
    // worker, the rewound pass with the recreated one. The replay
    // (result == 41) is the recreate proof.
    atomic_store_explicit(&phase, 1, memory_order_release);
    while (atomic_load_explicit(&result, memory_order_acquire) != 41) {}
    tprintf("worker replayed\n");

    // The worker dies from the die signal between the checkpoint and the
    // restore; the restore recreates it on its restored stack. On the
    // rewound timeline the recreated worker takes the kill but survives
    // (birth tid guard), so liveness (not delivery) picks the branch: the
    // original pass sees one task left, the rewound pass joins. Both copies
    // acknowledge the kill promptly, so only the death confirmation needs
    // a bound - it guards against scheduler stalls, not against the death
    // itself.
    pthread_kill(worker_thread, SIGUSR1);
    while (atomic_load_explicit(&kill_taken, memory_order_acquire) == 0) {}
    int worker_gone = 0;
    for (int i = 0; i < 50; ++i) {
        if (pthread_kill(worker_thread, 0) != 0) {
            worker_gone = 1;
            break;
        }
        usleep(1000);
    }
    if (!worker_gone) {
        // The recreated worker must finish through glibc so pthread_join can
        // observe its kernel exit, not just the replayed user code.
        atomic_store_explicit(&phase, 2, memory_order_release);
        assert(pthread_join(worker_thread, NULL) == 0);
        tprintf("joined recreated worker\n");
    }

    valo_restore();
}
