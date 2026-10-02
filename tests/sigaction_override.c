// TODO(ai-review): generated, not yet audited
//# link_flags: -pthread
// Games install their own signal handlers via sigaction (mono/Unity installs
// a SIGSYS chain handler via mono_set_signal_chaining). valo's checkpoint
// handler must stay the installed SIGSYS handler with SA_ONSTACK, so app
// sigaction requests for valo's private signals must be refused while still
// reporting success and the app's requested view, like libTAS
// signalwrappers.cpp:233.
#include <signal.h>
#include <assert.h>
#include <string.h>
#include "testlib.h"

static int app_handler_ran = 0;

static void app_handler(int signum) {
    (void)signum;
    app_handler_ran = 1;
}

int main(void) {
    int counter = 0;

    // The app installs a handler for the checkpoint signal, like mono does.
    struct sigaction act;
    memset(&act, 0, sizeof(act));
    act.sa_handler = app_handler;
    sigemptyset(&act.sa_mask);
    assert(sigaction(SIGSYS, &act, NULL) == 0);

    // The app must keep seeing its own requested view: its handler installed.
    struct sigaction current;
    assert(sigaction(SIGSYS, NULL, &current) == 0);
    tprintf("app view: %s\n",
            current.sa_handler == app_handler ? "app handler" : "not app handler");

    // valo must still own SIGSYS delivery: the checkpoint runs on the alt
    // stack with valo's handler, and the app handler never sees SIGSYS.
    // Red today (two ways):
    // 1. A handler that swallows SIGSYS: checkpoint silently does nothing
    //    (no snapshot, no error).
    // 2. A chaining handler (mono): valo's handler runs as a plain function
    //    on the normal stack -> assert_stack_in_range panics.
    counter = 1;
    valo_checkpoint();
    counter = 2;

    // The snapshot must have been taken at counter==1; restoring rewinds here.
    counter = 3;
    valo_restore();
    // After restore, counter replays to 3 and exits via VALO_MAX_RESTORES.
    tprintf("after restore: %d\n", counter);
    return 0;
}
