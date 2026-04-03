// Verify anonymous mmap memory is restored.
#include <assert.h>
#include <string.h>
#include <sys/mman.h>
#include "testlib.h"

int main() {
    void *p = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                   MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    assert(p != MAP_FAILED);
    memset(p, 0x42, 4096);

    tprintf("pre: %02x\n", ((unsigned char *)p)[0]);
    valo_checkpoint();
    tprintf("post: %02x\n", ((unsigned char *)p)[0]);
    assert(((unsigned char *)p)[0] == 0x42);

    memset(p, 0xff, 4096);
    tprintf("modified: %02x\n", ((unsigned char *)p)[0]);

    valo_restore();
}
