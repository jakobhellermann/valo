// TODO(ai-review): generated, not yet audited
// Verify pages that are all-zero at checkpoint time are zero again after restore.
#define _GNU_SOURCE
#include <assert.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>
#include "testlib.h"

#define PAGE 4096
#define PAGES 4

// page 0 and 2: data, page 1: written then zeroed, page 3: untouched
static void fill(unsigned char *p) {
    memset(p, 0x42, PAGE);
    memset(p + PAGE, 0x42, PAGE);
    memset(p + PAGE, 0, PAGE);
    memset(p + 2 * PAGE, 0x42, PAGE);
}

// One character per page: '0' if all bytes are zero, 'x' otherwise.
static void describe(const unsigned char *p, char *out) {
    for (int i = 0; i < PAGES; i++) {
        out[i] = '0';
        for (int j = 0; j < PAGE; j++)
            if (p[i * PAGE + j]) {
                out[i] = 'x';
                break;
            }
    }
    out[PAGES] = '\0';
}

static void print_state(const char *label, const unsigned char *anon, const unsigned char *shared) {
    char a[PAGES + 1], s[PAGES + 1];
    describe(anon, a);
    describe(shared, s);
    tprintf("%s: anon %s shared %s\n", label, a, s);
}

int main() {
    unsigned char *anon = mmap(NULL, PAGES * PAGE, PROT_READ | PROT_WRITE,
                               MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    assert(anon != MAP_FAILED);
    int fd = memfd_create("zero-page", 0);
    assert(fd >= 0);
    assert(ftruncate(fd, PAGES * PAGE) == 0);
    unsigned char *shared = mmap(NULL, PAGES * PAGE, PROT_READ | PROT_WRITE,
                                 MAP_SHARED, fd, 0);
    assert(shared != MAP_FAILED);

    fill(anon);
    fill(shared);

    print_state("pre", anon, shared);
    valo_checkpoint();
    print_state("post", anon, shared);

    memset(anon, 0xff, PAGES * PAGE);
    memset(shared, 0xff, PAGES * PAGE);
    print_state("modified", anon, shared);

    valo_restore();
}
