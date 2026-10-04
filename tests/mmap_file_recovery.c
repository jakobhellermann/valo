// A file-backed page that diverged after the checkpoint must be recovered
// from the mapped file: the checkpoint classified the clean page as
// file-backed, so restore re-reads the file instead of the savestate.
#include <assert.h>
#include <fcntl.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>
#include "testlib.h"

#define PAGE 4096

int main() {
    int fd = open("/tmp/valo-test-file-map.bin", O_RDWR | O_CREAT | O_TRUNC, 0600);
    assert(fd >= 0);
    unsigned char first[PAGE], second[PAGE];
    memset(first, 0x11, PAGE);
    memset(second, 0x33, PAGE);
    assert(write(fd, first, PAGE) == PAGE);
    assert(write(fd, second, PAGE) == PAGE);

    unsigned char *p = mmap(NULL, 2 * PAGE, PROT_READ | PROT_WRITE, MAP_PRIVATE, fd, 0);
    assert(p != MAP_FAILED);

    // Diverge page 1 before the checkpoint: it must come from the savestate
    // (0x44) after restore, not from the file (0x33).
    p[PAGE] = 0x44;

    // Read-fault page 0 so the checkpoint sees it present and file-backed.
    volatile unsigned char sink = p[0];
    (void)sink;
    tprintf("pre: p0=%02x p1=%02x\n", p[0], p[PAGE]);

    valo_checkpoint();

    // Page 0 was clean at the checkpoint and diverged since: restore must
    // re-read it from the file. Page 1 comes from the savestate.
    tprintf("post: p0=%02x p1=%02x\n", p[0], p[PAGE]);
    assert(p[0] == 0x11);
    assert(p[PAGE] == 0x44);

    // Diverge both pages; the next restore re-runs this code rewound.
    memset(p, 0xFF, 2 * PAGE);
    tprintf("diverged\n");

    valo_restore();
}
