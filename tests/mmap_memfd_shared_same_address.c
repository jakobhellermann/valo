// TODO(ai-review): generated, not yet audited
//# max_restores: 2
// Like mmap_memfd_shared_realloc, but the rewound timeline maps a *different*
// memfd with the same name at the same address before the later slot is
// restored (a deterministic allocator re-creating its buffer). The restore
// must not take that mapping for the saved one: same address, name and
// permissions, but another inode.
//
// Pass map (rewind targets: slot 0 -> after checkpoint_slot(0), slot 1 ->
// after checkpoint_slot(1)); the pass counter lives in a /tmp file because
// process memory rewinds:
//   pass 0: map memfd A at ADDR, save slot 1, [check], restore 0
//   pass 1: map a new memfd B at ADDR, restore slot 1
//   pass 2: [check against A], exit
#define _GNU_SOURCE
#include <assert.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>
#include "testlib.h"

#define PAGE 4096
#define ADDR ((void *)0x7e5500000000)

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

static unsigned char *map_new_memfd(int *out_fd) {
    int mfd = memfd_create("valo-test-shared", 0);
    assert(mfd >= 0);
    assert(ftruncate(mfd, PAGE) == 0);
    unsigned char *m =
        mmap(ADDR, PAGE, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_FIXED_NOREPLACE, mfd, 0);
    assert(m == ADDR);
    *out_fd = mfd;
    return m;
}

// The written value differs per pass: a value an earlier pass already
// wrote to the memfd would read back even through a detached mapping.
static void check(void) {
    unsigned char value = 0x30 + pass_count();
    p[0] = value;
    unsigned char via_fd = 0;
    assert(pread(fd, &via_fd, 1, 0) == 1);
    tprintf("wrote %02x through mapping, fd reads %02x\n", value, via_fd);
    assert(via_fd == value);
}

int main() {
    snprintf(pass_path, sizeof(pass_path), "/tmp/valo-memfd-same-addr-%d", (int)getpid());
    unlink(pass_path);

    valo_checkpoint_slot(0);
    pass_bump();
    int pass = pass_count() - 1;
    tprintf("pass %d\n", pass);

    if (pass == 0) {
        p = map_new_memfd(&fd);
        p[0] = 0x11;

        valo_checkpoint_slot(1);

        // Runs on pass 0 and, after the cross-slot restore, on pass 2.
        check();
        if (pass_count() == 1) {
            valo_restore_slot(0);
        }
        unlink(pass_path);
        return 0;
    }

    int other_fd;
    unsigned char *other = map_new_memfd(&other_fd);
    other[0] = 0x99;
    valo_restore_slot(1);
    return 1;
}
