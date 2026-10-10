//# link_mode: preload
// A reader that keeps one /dev/urandom fd open gets data past the first page.
#include <assert.h>
#include <fcntl.h>
#include <unistd.h>
#include "testlib.h"

int main() {
    int fd = open("/dev/urandom", O_RDONLY);
    assert(fd >= 0);
    unsigned char buf[4096];
    int total = 0;
    for (int i = 0; i < 16; i++) {
        ssize_t n = read(fd, buf, sizeof(buf));
        assert(n == sizeof(buf));
        total += n;
    }
    close(fd);
    tprintf("read %d\n", total);
}
