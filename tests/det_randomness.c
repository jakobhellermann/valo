// TODO(ai-review): generated, not yet audited
//# link_mode: preload
// Deterministic randomness: getrandom(), getentropy() and /dev/urandom (via
// open and fopen) yield the same bytes in every run, so a game seeding its
// RNG from them replays identically. The snapshot pins the bytes across
// runs; the restore pins that the stream rewinds with the process state.
//
// The second valo_restore() exits via VALO_MAX_RESTORES.
#include <assert.h>
#include <dlfcn.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/random.h>
#include <unistd.h>
#include "testlib.h"

static void print_hex(const char *label, const unsigned char *buf, int len) {
    static const char digits[] = "0123456789abcdef";
    char hex[2 * 16 + 1];
    for (int i = 0; i < len; i++) {
        hex[2 * i] = digits[buf[i] >> 4];
        hex[2 * i + 1] = digits[buf[i] & 0xf];
    }
    hex[2 * len] = 0;
    tprintf("%s: %s\n", label, hex);
}

static void read_entropy(void) {
    unsigned char buf[16];
    assert(getrandom(buf, sizeof(buf), 0) == sizeof(buf));
    print_hex("getrandom", buf, sizeof(buf));

    assert(getentropy(buf, sizeof(buf)) == 0);
    print_hex("getentropy", buf, sizeof(buf));

    int fd = open("/dev/urandom", O_RDONLY);
    assert(fd >= 0);
    assert(read(fd, buf, sizeof(buf)) == sizeof(buf));
    close(fd);
    print_hex("open urandom", buf, sizeof(buf));

    FILE *f = fopen("/dev/urandom", "r");
    assert(f);
    assert(fread(buf, 1, sizeof(buf), f) == sizeof(buf));
    fclose(f);
    print_hex("fopen urandom", buf, sizeof(buf));
}

int main() {
    void (*checkpoint)(void) = dlsym(RTLD_DEFAULT, "valo_checkpoint");
    void (*restore)(void) = dlsym(RTLD_DEFAULT, "valo_restore");
    assert(checkpoint && "valo_checkpoint not found");
    assert(restore && "valo_restore not found");

    read_entropy();
    checkpoint();
    read_entropy();
    restore();
    return 1;
}
