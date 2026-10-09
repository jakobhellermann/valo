// TODO(ai-review): generated, not yet audited
//# link_mode: preload
// CLOCK_MONOTONIC_RAW is virtual from process start: before the first frame
// boundary it reads the monotonic epoch and stands still, also across real
// waits, since only frames and waits valo accounts for advance the clock.
#include <assert.h>
#include <linux/futex.h>
#include <stdint.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>
#include "testlib.h"

static long long clock_ns(clockid_t clock) {
    struct timespec ts;
    clock_gettime(clock, &ts);
    return ts.tv_sec * 1000000000LL + ts.tv_nsec;
}

int main() {
    uint32_t word = 0;
    const struct timespec timeout = {0, 1000000};

    long long start = clock_ns(CLOCK_MONOTONIC_RAW);
    // a raw futex timeout is invisible to valo: real time passes, virtual time does not
    for (int i = 0; i < 10; i++) {
        syscall(SYS_futex, &word, FUTEX_WAIT_PRIVATE, 0, &timeout, NULL, 0);
    }
    long long after = clock_ns(CLOCK_MONOTONIC_RAW);
    long long monotonic = clock_ns(CLOCK_MONOTONIC);

    tprintf("raw start=%lldns after waits=%lldns monotonic=%lldns\n", start, after, monotonic);
    assert(start == 0);
    assert(after == start);
    assert(monotonic == start);
    return 0;
}
