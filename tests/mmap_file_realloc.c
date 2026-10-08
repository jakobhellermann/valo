//# max_restores: 1
// A file mapping that the game unmapped after the checkpoint must be
// re-established from the file on restore: pages the snapshot flagged as
// file-backed (clean) are not stored — their content has to come from the
// mapped file again, so an anonymous remap would read them as zeros.
#include <assert.h>
#include <fcntl.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>
#include "testlib.h"

#define PAGE 4096

static const char *file = "/tmp/valo-test-realloc-file.bin";

int main() {
    int fd = open(file, O_RDWR | O_CREAT | O_TRUNC, 0600);
    assert(fd >= 0);
    unsigned char first[PAGE], second[PAGE];
    memset(first, 0x11, PAGE);
    memset(second, 0x33, PAGE);
    assert(write(fd, first, PAGE) == PAGE);
    assert(write(fd, second, PAGE) == PAGE);

    // Two pages past the start: exercises the file offset in the remap.
    unsigned char *p = mmap(NULL, PAGE, PROT_READ | PROT_WRITE, MAP_PRIVATE, fd, PAGE);
    assert(p != MAP_FAILED);
    close(fd);

    // Fault the page in so the checkpoint sees it present and file-backed
    // (clean pages are assumed equal to the file and not saved).
    volatile unsigned char sink = p[0];
    (void)sink;

    tprintf("pre: p0=%02x\n", p[0]);
    valo_checkpoint();

    // Unmap AFTER the checkpoint: at restore the area is missing from the
    // current maps and must be re-created from the file.
    assert(munmap(p, PAGE) == 0);
    tprintf("unmapped\n");

    valo_restore();

    // ---- rewound execution resumes here after the restore ----
    // The mapping must exist again, at the same address, with the
    // file-backed page reading the file content (0x33 — the second page,
    // via the saved offset). An anonymous remap would read as zeros.
    tprintf("post: p0=%02x\n", p[0]);
    assert(p[0] == 0x33);
    return 0;
}
