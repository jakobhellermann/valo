// TODO(ai-review): generated, not yet audited
//# link_flags: -pthread
// Threads inherit the creator's signal mask at pthread_create. The
// creator's block can predate our hook or bypass it, so the child may
// start with the suspend signal blocked. valo's start-time unblock must
// clear it — and must survive valo's own hook interposition.
// The raw syscall below is the worst case: a block our hook never sees.
#include <pthread.h>
#include <signal.h>
#include <assert.h>
#include <stdatomic.h>
#include <sys/syscall.h>
#include <unistd.h>
#include "testlib.h"

static atomic_int started;

static void *thread_main(void *arg) {
    (void)arg;
    atomic_store(&started, 1);
    for (;;) {} // alive at no particular point — must still be suspendable
}

int main(void) {
    // Block the suspend signal via raw syscall: a block our hook never
    // sanitizes, so the child inherits it at creation.
    sigset_t mask;
    sigemptyset(&mask);
    sigaddset(&mask, SIGXFSZ);
    assert(syscall(SYS_rt_sigprocmask, SIG_BLOCK, &mask, NULL, 8) == 0);

    pthread_t thread;
    assert(pthread_create(&thread, NULL, thread_main, NULL) == 0);
    wait_for_threads(&started, 1);

    // Red: hangs forever. The child inherited the block and valo's own
    // start-time unblock was neutralized by its own sigmask hook.
    valo_checkpoint();
    tprintf("checkpoint ok\n");
    return 0;
}
