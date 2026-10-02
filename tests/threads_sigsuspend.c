// TODO(ai-review): generated, not yet audited
//# link_flags: -pthread
// sigsuspend replaces the thread's signal mask for the duration of the
// wait. An app waiting on a full mask (sigfillset) would block valo's
// suspend signal for the whole wait, so the hook must strip valo's signals
// from the wait mask. Red without the strip: the checkpoint hangs forever
// waiting for the thread to park.
#include <pthread.h>
#include <signal.h>
#include <assert.h>
#include <stdatomic.h>
#include "testlib.h"

static atomic_int started;

static void *thread_main(void *arg) {
    (void)arg;
    atomic_store(&started, 1);
    sigset_t all;
    sigfillset(&all);
    sigsuspend(&all); // waits until a signal is delivered

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
