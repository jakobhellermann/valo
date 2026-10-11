//# max_restores: 2
// TODO(ai-review): generated, not yet audited
//
// A library dlopen'd between two checkpoints must come back relocated when
// its slot is restored from a timeline in which it was never loaded.
//
// The dynamic loader writes relocated addresses into the RELRO segment
// (.got, and .got.plt for BIND_NOW libraries) and only then makes it
// read-only. Such a page differs from the file, so it has to be saved
// even though the mapping is not writable; restoring it from the file
// leaves raw link-time offsets in the GOT and the first call through it
// jumps to an unmapped address (Hollow Knight: Steam plugins loaded after
// slot 1, slot 3 saved later, restore 1 then 3, SteamAPI_RunCallbacks
// -> SIGSEGV at the unrelocated PLT target).
//
// libz is full RELRO + BIND_NOW on the target distros and not linked by
// the test or libvalo; compress() reaches malloc through its GOT.
//
// Pass map (file in /tmp counts passes, process memory rewinds):
//   pass 0: dlopen libz, use it, save slot 1, restore slot 0
//   pass 1: slot-0 timeline (libz not mapped), restore slot 1
//   pass 2: use libz from the restored slot-1 state
#include <dlfcn.h>
#include <fcntl.h>
#include <stdlib.h>
#include <sys/stat.h>

#include "testlib.h"

typedef int (*compress_fn)(unsigned char *, unsigned long *, const unsigned char *,
                           unsigned long);
typedef int (*uncompress_fn)(unsigned char *, unsigned long *, const unsigned char *,
                             unsigned long);

static compress_fn z_compress;
static uncompress_fn z_uncompress;

static char pass_path[64];

static int pass_count(void);
static void pass_bump(void);
static int libz_mapped(void);
static void roundtrip(const char *when);

int main(void) {
    snprintf(pass_path, sizeof(pass_path), "/tmp/valo-test-dlopen-relro-%d", getpid());
    unlink(pass_path);

    valo_checkpoint_slot(0);
    if (pass_count() == 0) {
        pass_bump();
        void *libz = dlopen("libz.so.1", RTLD_NOW | RTLD_LOCAL);
        assert(libz != NULL);
        z_compress = (compress_fn)dlsym(libz, "compress");
        z_uncompress = (uncompress_fn)dlsym(libz, "uncompress");
        assert(z_compress != NULL && z_uncompress != NULL);
        roundtrip("before");

        valo_checkpoint_slot(1);
        if (pass_count() == 1) {
            pass_bump();
            tprintf("restore slot 0\n");
            valo_restore_slot(0);
        }
        roundtrip("after");
        return 0;
    }

    tprintf("slot 0 timeline: libz mapped=%d\n", libz_mapped());
    valo_restore_slot(1);
    return 1;
}

static void roundtrip(const char *when) {
    static const unsigned char input[] = "valo relro restore valo relro restore";
    unsigned char packed[128];
    unsigned char unpacked[128];
    unsigned long packed_len = sizeof(packed);
    unsigned long unpacked_len = sizeof(unpacked);

    assert(z_compress(packed, &packed_len, input, sizeof(input)) == 0);
    assert(z_uncompress(unpacked, &unpacked_len, packed, packed_len) == 0);
    assert(unpacked_len == sizeof(input));
    assert(memcmp(unpacked, input, sizeof(input)) == 0);
    tprintf("%s: zlib roundtrip ok\n", when);
}

// Counts passes across restores: the file survives, process memory rewinds.
static int pass_count(void) {
    struct stat st;
    if (stat(pass_path, &st) != 0)
        return 0;
    return (int)st.st_size;
}

static void pass_bump(void) {
    int fd = open(pass_path, O_WRONLY | O_APPEND | O_CREAT, 0600);
    assert(fd >= 0);
    assert(write(fd, "x", 1) == 1);
    close(fd);
}

static int libz_mapped(void) {
    FILE *maps = fopen("/proc/self/maps", "r");
    assert(maps != NULL);
    char line[512];
    int mapped = 0;
    while (fgets(line, sizeof(line), maps)) {
        if (strstr(line, "/libz.so"))
            mapped = 1;
    }
    fclose(maps);
    return mapped;
}
