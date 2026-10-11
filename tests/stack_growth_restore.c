//# link_flags: -pthread
//# max_restores: 2
// The main stack is the process's only VM_GROWSDOWN mapping and must
// survive a restore without being re-created. The save happens with the
// natural startup stack; before the restore the main thread descends
// below the saved stack bottom, and the deterministic replay after the
// restore re-descends the same depth - the write below the restored
// bottom must fault the stack in, not die.
#define _GNU_SOURCE
#include <pthread.h>
#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "testlib.h"

static volatile int g_idx = 3;
static char *stack_bottom;

// The current bottom of the [stack] mapping from /proc/self/maps.
// pthread_getattr_np is useless here: for the initial thread it reports
// the rlimit-sized potential stack, not the pages grown so far.
static char *current_stack_bottom(void) {
    FILE *maps = fopen("/proc/self/maps", "r");
    assert(maps != NULL);
    char line[256];
    char *bottom = NULL;
    while (fgets(line, sizeof(line), maps) != NULL) {
        if (strstr(line, "[stack]") != NULL) {
            bottom = (char *)strtoull(line, NULL, 16);
            break;
        }
    }
    fclose(maps);
    assert(bottom != NULL);
    return bottom;
}

// ~1 KiB live frames; the pad is accessed at a runtime-unknown index so
// the compiler cannot shrink it away.
static __attribute__((noinline)) void descend(void) {
    volatile char pad[1024];
    pad[g_idx] = 1;
    if ((uintptr_t)&pad[0] > (uintptr_t)stack_bottom - 512 * 1024) {
        descend();
    }
    pad[g_idx + 1] = 2;
}

int main(void) {
    stack_bottom = current_stack_bottom();

    valo_checkpoint();
    descend();
    tprintf("main stack regrew\n");

    valo_restore();
    return 2;
}
