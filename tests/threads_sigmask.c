// TODO(ai-review): generated, not yet audited
//# link_flags: -pthread
// Threads may block all signals (e.g. pulseaudio's threaded-ml).
// valo must keep its suspend signal deliverable
#include <pthread.h>
#include <signal.h>
#include <assert.h>
#include <stdatomic.h>
#include "testlib.h"

static atomic_int started;

static void *thread_main(void *arg) {
    (void)arg;
    // block everything in this thread.
    sigset_t all;
    sigfillset(&all);
    assert(pthread_sigmask(SIG_BLOCK, &all, NULL) == 0);
    atomic_store(&started, 1);
    for (;;) {}
}

int main(void) {
    pthread_t thread;
    assert(pthread_create(&thread, NULL, thread_main, NULL) == 0);
    wait_for_threads(&started, 1);

    valo_checkpoint();
    tprintf("checkpoint ok\n");
    return 0;
}
