// Verify heap memory is restored across checkpoint/restore.
#include <assert.h>
#include <stdlib.h>
#include <string.h>
#include "testlib.h"

int main() {
    char *buf = malloc(4096);
    memset(buf, 'A', 4096);

    tprintf("pre: %c\n", buf[0]);
    valo_checkpoint();
    tprintf("post: %c\n", buf[0]);
    assert(buf[0] == 'A');

    memset(buf, 'B', 4096);
    tprintf("mutated: %c\n", buf[0]);
    assert(buf[0] == 'B');

    valo_restore();
}
