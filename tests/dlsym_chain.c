//# link_mode: preload
//# preload_after: lib/dlsym_interposer
// TODO(ai-review): generated, not yet audited
//
// libvalo's dlsym must forward to the next dlsym definition, not straight to libc's: a
// later preload's dlsym is otherwise never called, and Steam's overlay leaves its own
// hooks uninitialized (its close() became a no-op, so bash hung reading `$(...)`).
#include <dlfcn.h>

#include "testlib.h"

int main() {
    const char *(*last)(void) = dlsym(RTLD_DEFAULT, "interposer_last_dlsym");
    assert(last);
    void *sym = dlsym(RTLD_DEFAULT, "puts");
    assert(sym);
    tprintf("interposer saw: %s\n", last());
}
