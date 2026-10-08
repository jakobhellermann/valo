// A memfd descriptor must point at the same memfd after a restore: memfds
// are anonymous and cannot be reopened, so the original kernel object has to
// survive (or the rewound state talks to a different file).
#define _GNU_SOURCE
#include <assert.h>
#include <fcntl.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>
#include "testlib.h"

#define SIZE 4096

int main() {
    int fd = memfd_create("valo-fd-memfd-rewind", 0);
    assert(fd >= 0);
    assert(ftruncate(fd, SIZE) == 0);
    assert(write(fd, "hello", 5) == 5);

    struct stat before;
    assert(fstat(fd, &before) == 0);

    valo_checkpoint();

    // Runs again after the restore.
    struct stat after;
    assert(fstat(fd, &after) == 0);
    assert(after.st_ino == before.st_ino);
    assert(lseek(fd, 0, SEEK_SET) == 0);
    char buf[5];
    assert(read(fd, buf, 5) == 5);
    assert(memcmp(buf, "hello", 5) == 0);
    tprintf("fd: restored\n");

    // Discard timeline: close the memfd and occupy its number.
    assert(close(fd) == 0);
    int replacement = open("/dev/null", O_RDONLY);
    assert(replacement == fd);

    valo_restore();
}
