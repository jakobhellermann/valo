// TODO(ai-review): generated, not yet audited
//# max_restores: 2
// The saved mapping covers two pages of a memfd; the rewound timeline maps
// only the first page of the same memfd at the same address. The restore
// must map the missing second page again, from the second page of the file
// (the saved offset advanced by the part that is already mapped).
//
// Pass map (rewind targets: slot 0 -> after checkpoint_slot(0), slot 1 ->
// after checkpoint_slot(1)); the pass counter lives in a /tmp file because
// process memory rewinds:
//   pass 0: map both pages at ADDR, save slot 1, [check], restore 0
//   pass 1: map only the first page at ADDR, restore slot 1
//   pass 2: [check both pages], exit
#define _GNU_SOURCE
#include <assert.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>
#include "testlib.h"

#define PAGE 4096
#define ADDR ((void *)0x7e5700000000)

static char pass_path[64];
static int fd = -1;
static unsigned char *p;

static int pass_count(void) {
    struct stat st;
    if (stat(pass_path, &st) != 0) return 0;
    return (int)st.st_size;
}

static void pass_bump(void) {
    int pfd = open(pass_path, O_WRONLY | O_APPEND | O_CREAT, 0600);
    assert(pfd >= 0);
    assert(write(pfd, "x", 1) == 1);
    assert(close(pfd) == 0);
}

static unsigned char *map_pages(size_t pages) {
    unsigned char *m = mmap(ADDR, pages * PAGE, PROT_READ | PROT_WRITE,
                            MAP_SHARED | MAP_FIXED_NOREPLACE, fd, 0);
    assert(m == ADDR);
    return m;
}

// Each page gets its own per-pass value; both must reach their own file
// offset.
static void check(void) {
    for (int page = 0; page < 2; page++) {
        unsigned char value = 0x30 + 0x10 * page + pass_count();
        p[page * PAGE] = value;
        unsigned char via_fd = 0;
        assert(pread(fd, &via_fd, 1, page * PAGE) == 1);
        tprintf("page %d: wrote %02x through mapping, fd reads %02x\n", page, value, via_fd);
        assert(via_fd == value);
    }
}

int main() {
    snprintf(pass_path, sizeof(pass_path), "/tmp/valo-memfd-partial-%d", (int)getpid());
    unlink(pass_path);

    // Before the first save: the memfd itself exists in both timelines.
    fd = memfd_create("valo-test-shared", 0);
    assert(fd >= 0);
    assert(ftruncate(fd, 2 * PAGE) == 0);

    valo_checkpoint_slot(0);
    pass_bump();
    int pass = pass_count() - 1;
    tprintf("pass %d\n", pass);

    if (pass == 0) {
        p = map_pages(2);
        p[0] = 0x11;
        p[PAGE] = 0x22;

        valo_checkpoint_slot(1);

        // Runs on pass 0 and, after the cross-slot restore, on pass 2.
        check();
        if (pass_count() == 1) {
            valo_restore_slot(0);
        }
        unlink(pass_path);
        return 0;
    }

    map_pages(1);
    valo_restore_slot(1);
    return 1;
}
