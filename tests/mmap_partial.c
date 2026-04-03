// Verify mmap where only some pages are written.
// Tests that untouched pages are handled correctly.
#include <assert.h>
#include <string.h>
#include <sys/mman.h>
#include "testlib.h"

#define NPAGES 16
#define SIZE (NPAGES * 4096)

int main() {
    unsigned char *p = mmap(NULL, SIZE, PROT_READ | PROT_WRITE,
                            MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    assert(p != MAP_FAILED);

    // Only write to pages 0, 5, and 15 — leave the rest untouched
    memset(p + 0 * 4096, 0xAA, 4096);
    memset(p + 5 * 4096, 0xBB, 4096);
    memset(p + 15 * 4096, 0xCC, 4096);

    tprintf("pre: p0=%02x p5=%02x p8=%02x p15=%02x\n",
            p[0], p[5 * 4096], p[8 * 4096], p[15 * 4096]);

    valo_checkpoint();

    tprintf("post: p0=%02x p5=%02x p8=%02x p15=%02x\n",
            p[0], p[5 * 4096], p[8 * 4096], p[15 * 4096]);
    assert(p[0] == 0xAA);
    assert(p[5 * 4096] == 0xBB);
    assert(p[8 * 4096] == 0x00);  // untouched page
    assert(p[15 * 4096] == 0xCC);

    // Overwrite everything
    memset(p, 0xFF, SIZE);
    tprintf("overwritten\n");

    valo_restore();
}
