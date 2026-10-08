// A regular file's descriptor survives a restore: same fd number, same
// file identity, rewound seek offset, and descriptors opened only in the
// discarded timeline are closed.
#define _GNU_SOURCE
#include <assert.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>
#include "testlib.h"

static int count_fds(void) {
    DIR *dir = opendir("/proc/self/fd");
    assert(dir != NULL);
    int dir_fd = dirfd(dir);
    int count = 0;
    struct dirent *entry;
    while ((entry = readdir(dir)) != NULL) {
        int fd = atoi(entry->d_name);
        if (fd != dir_fd)
            count++;
    }
    closedir(dir);
    return count;
}

int main() {
    int fd = open("/tmp/valo-fd-rewind", O_RDWR | O_CREAT | O_CLOEXEC, 0644);
    assert(fd >= 0);
    assert(write(fd, "0123456789", 10) == 10);
    assert(lseek(fd, 3, SEEK_SET) == 3);

    struct stat before;
    assert(fstat(fd, &before) == 0);
    int fds_before = count_fds();

    valo_checkpoint();

    // Runs again after the restore.
    struct stat after;
    assert(fstat(fd, &after) == 0);
    assert(after.st_ino == before.st_ino);
    assert(lseek(fd, 0, SEEK_CUR) == 3);
    assert(count_fds() == fds_before);
    tprintf("fd: restored\n");

    // Discard timeline: replace the fd and open an extra one.
    assert(close(fd) == 0);
    int replacement = open("/dev/null", O_RDONLY);
    assert(replacement == fd);
    assert(open("/dev/null", O_RDONLY | O_CLOEXEC) > fd);

    valo_restore();
}
