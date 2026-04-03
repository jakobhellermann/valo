// Verify large multi-page allocations are restored correctly.
#include <assert.h>
#include <stdlib.h>
#include <string.h>
#include "testlib.h"

#define SIZE (1024 * 1024)  // 1MB = 256 pages

int main() {
    char *buf = malloc(SIZE);
    // Fill with a pattern: each page starts with its page number
    for (int i = 0; i < SIZE; i += 4096) {
        int page = i / 4096;
        memset(buf + i, page & 0xff, 4096);
    }

    tprintf("pre: [0]=%02x [100]=%02x [255]=%02x\n",
            (unsigned char)buf[0],
            (unsigned char)buf[100 * 4096],
            (unsigned char)buf[255 * 4096]);

    valo_checkpoint();

    // Verify pattern
    for (int i = 0; i < SIZE; i += 4096) {
        int page = i / 4096;
        assert((unsigned char)buf[i] == (page & 0xff));
    }
    tprintf("verified\n");

    // Overwrite everything
    memset(buf, 0xff, SIZE);
    tprintf("overwritten\n");

    valo_restore();
}
