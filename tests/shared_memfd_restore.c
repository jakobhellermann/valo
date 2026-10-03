// TODO(ai-review): generated, not yet audited
// A MAP_SHARED mapping backed by a memfd is checkpointed and rewound to its
// checkpoint content.
#define _GNU_SOURCE
#include <assert.h>
#include <fcntl.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>
#include "testlib.h"

#define PAGE 4096
#define PAGES 2
#define SIZE (PAGE * PAGES)

int main() {
    int fd = memfd_create("valo-shm-restore", 0);
    assert(fd >= 0);
    assert(ftruncate(fd, SIZE) == 0);

    unsigned char *p = mmap(NULL, SIZE, PROT_READ | PROT_WRITE,
                            MAP_SHARED, fd, 0);
    assert(p != MAP_FAILED);

    // Fill every page of the mapping ourselves.
    memset(p, 0x42, SIZE);
    tprintf("pre: %02x %02x\n", p[0], p[SIZE - 1]);

    valo_checkpoint();
    tprintf("post: %02x %02x\n", p[0], p[SIZE - 1]);

    // Corrupt every page after the checkpoint.
    memset(p, 0xff, SIZE);
    tprintf("modified: %02x %02x\n", p[0], p[SIZE - 1]);

    valo_restore();
}
