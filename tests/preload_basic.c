//# link_mode: preload
// Verify checkpoint/restore works via LD_PRELOAD (no direct linking).
#include <assert.h>
#include <dlfcn.h>
#include "testlib.h"

int main() {
    void (*checkpoint)(void) = dlsym(RTLD_DEFAULT, "valo_checkpoint");
    void (*restore)(void) = dlsym(RTLD_DEFAULT, "valo_restore");
    assert(checkpoint && "valo_checkpoint not found — is LD_PRELOAD set?");
    assert(restore && "valo_restore not found — is LD_PRELOAD set?");

    int x = 42;
    tprintf("before: %d\n", x);
    checkpoint();
    tprintf("after: %d\n", x);
    assert(x == 42);
    x = 99;
    tprintf("modified: %d\n", x);
    restore();
}
