// TODO(ai-review): generated, not yet audited
//# max_restores: 2
// The rewound timeline maps the *same* memfd at the same address, but from
// another file offset, before the later slot is restored. Same address,
// name, permissions and inode: the restore must still notice the offset
// differs and map the saved file range again.
//
// Pass map (rewind targets: slot 0 -> after checkpoint_slot(0), slot 1 ->
// after checkpoint_slot(1)); the pass counter lives in a /tmp file because
// process memory rewinds:
//   pass 0: map the memfd at ADDR from offset 0, save slot 1, [check], restore 0
//   pass 1: map the memfd at ADDR from offset PAGE, restore slot 1
//   pass 2: [check offset 0], exit
#define _GNU_SOURCE
#include <assert.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>
#include "testlib.h"

#define PAGE 4096
#define ADDR ((void *)0x7e5600000000)

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

static unsigned char *map_at(off_t offset) {
    unsigned char *m = mmap(ADDR, PAGE, PROT_READ | PROT_WRITE,
                            MAP_SHARED | MAP_FIXED_NOREPLACE, fd, offset);
    assert(m == ADDR);
    return m;
}

// The written value differs per pass: a value an earlier pass already
// wrote to the memfd would read back even through a misplaced mapping.
static void check(void) {
    unsigned char value = 0x30 + pass_count();
    p[0] = value;
    unsigned char via_fd = 0;
    assert(pread(fd, &via_fd, 1, 0) == 1);
    tprintf("wrote %02x through mapping, fd offset 0 reads %02x\n", value, via_fd);
    assert(via_fd == value);
}

int main() {
    snprintf(pass_path, sizeof(pass_path), "/tmp/valo-memfd-offset-%d", (int)getpid());
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
        p = map_at(0);
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

    map_at(PAGE);
    valo_restore_slot(1);
    return 1;
}
