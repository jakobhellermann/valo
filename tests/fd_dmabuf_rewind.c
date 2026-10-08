// A dma-buf descriptor must point at the same dma-buf after a restore.
// dma-bufs have no path and no name: the only handle is the fd, so the
// original kernel object has to survive the restore.
#define _GNU_SOURCE
#include <assert.h>
#include <fcntl.h>
#include <linux/udmabuf.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>
#include "testlib.h"

#define SIZE 4096

int main() {
    int memfd = memfd_create("valo-fd-dmabuf-rewind", MFD_ALLOW_SEALING);
    assert(memfd >= 0);
    assert(ftruncate(memfd, SIZE) == 0);
    assert(fcntl(memfd, F_ADD_SEALS, F_SEAL_SHRINK) == 0);

    int udmabuf = open("/dev/udmabuf", O_RDWR);
    assert(udmabuf >= 0);
    struct udmabuf_create create = {
        .memfd = memfd,
        .flags = UDMABUF_FLAGS_CLOEXEC,
        .offset = 0,
        .size = SIZE,
    };
    int dmabuf = ioctl(udmabuf, UDMABUF_CREATE, &create);
    assert(dmabuf >= 0);

    unsigned char *p = mmap(NULL, SIZE, PROT_READ | PROT_WRITE, MAP_SHARED, dmabuf, 0);
    assert(p != MAP_FAILED);
    memset(p, 0x42, SIZE);
    tprintf("pre: %02x\n", p[0]);

    struct stat before;
    assert(fstat(dmabuf, &before) == 0);

    valo_checkpoint();

    // Runs again after the restore.
    struct stat after;
    assert(fstat(dmabuf, &after) == 0);
    assert(after.st_ino == before.st_ino);
    assert(p[0] == 0x42);
    tprintf("fd: restored\n");

    // Discard timeline: close the dma-buf and occupy its number. The mapping
    // keeps the memory alive on its own.
    assert(close(dmabuf) == 0);
    int replacement = open("/dev/null", O_RDONLY);
    assert(replacement == dmabuf);

    valo_restore();
}
