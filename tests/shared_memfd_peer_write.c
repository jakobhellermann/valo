// TODO(ai-review): generated, not yet audited
// Pages of a shared memfd written by another process (absent from this
// process's pagemap at checkpoint time) are checkpointed and rewound.
#define _GNU_SOURCE
#include <assert.h>
#include <fcntl.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>
#include "testlib.h"

#define PAGE 4096
#define SIZE (PAGE * 2)

int main() {
    int fd = memfd_create("valo-shm-peer", 0);
    assert(fd >= 0);
    assert(ftruncate(fd, SIZE) == 0);

    unsigned char *p = mmap(NULL, SIZE, PROT_READ | PROT_WRITE,
                            MAP_SHARED, fd, 0);
    assert(p != MAP_FAILED);

    // Only page 0; page 1 must be absent from this process's pagemap at
    // checkpoint time.
    memset(p, 0x42, PAGE);

    pid_t pid = fork();
    if (pid == 0) {
        unsigned char *c = mmap(NULL, SIZE, PROT_READ | PROT_WRITE,
                                MAP_SHARED, fd, 0);
        if (c == MAP_FAILED) _exit(1);
        c[PAGE] = 0x99;
        _exit(0);
    }
    int status;
    assert(waitpid(pid, &status, 0) == pid);
    assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);

    tprintf("pre: %02x\n", p[0]);

    valo_checkpoint();
    tprintf("post: %02x %02x\n", p[0], p[PAGE]);

    memset(p, 0xff, SIZE);
    tprintf("modified: %02x %02x\n", p[0], p[PAGE]);

    valo_restore();
}
