// TODO(ai-review): generated, not yet audited
//# max_restores: 2
// A shared memfd mapping created between two snapshots must come back as a
// mapping of the same memfd when the later slot is restored after an
// earlier-slot restore dropped it, not as an anonymous copy: another
// process (e.g. the GPU host importing the memfd as a dma-buf) keeps
// reading the memfd's pages, so writes through the restored mapping have
// to land there.
//
// Pass map (rewind targets: slot 0 -> after checkpoint_slot(0), slot 1 ->
// after checkpoint_slot(1)); the pass counter lives in a /tmp file because
// process memory rewinds:
//   pass 0: create memfd + shared mapping, save slot 1, [check], restore 0
//   pass 1: (memfd unmapped and closed by the rewind) restore slot 1
//   pass 2: [check], exit
#define _GNU_SOURCE
#include <assert.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>
#include "testlib.h"

#define PAGE 4096

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
    snprintf(pass_path, sizeof(pass_path), "/tmp/valo-memfd-realloc-%d", (int)getpid());
    unlink(pass_path);

    valo_checkpoint_slot(0);
    pass_bump();
    int pass = pass_count() - 1;
    tprintf("pass %d\n", pass);

    if (pass == 0) {
        fd = memfd_create("valo-test-shared", 0);
        assert(fd >= 0);
        assert(ftruncate(fd, PAGE) == 0);
        p = mmap(NULL, PAGE, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
        assert(p != MAP_FAILED);
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

    valo_restore_slot(1);
    return 1;
}
