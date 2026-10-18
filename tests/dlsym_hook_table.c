//# link_mode: preload
//# preload_after: lib/dlsym_hook_table
//# link_flags: -lGL
// TODO(ai-review): generated, not yet audited
//
// libvalo resolves the real definitions behind its hooks with glibc's dlsym. A later
// preload's dlsym that answers from a hook table hands back libvalo's own hook (its table
// entries bind to libvalo), so the hook would call itself until the stack overflows
// (Hollow Knight under mangohud).
#define _GNU_SOURCE
#include <dlfcn.h>
#include <string.h>

#include <GL/glx.h>

#include "testlib.h"

static const char *owner(void *sym) {
    Dl_info info;
    int found = dladdr(sym, &info);
    assert(found);
    const char *base = strrchr(info.dli_fname, '/');
    return base ? base + 1 : info.dli_fname;
}

int main() {
    int (*forwarded)(void) = dlsym(RTLD_DEFAULT, "table_interposer_forwarded");
    assert(forwarded);
    void *swap = (void *)glXGetProcAddressARB((const GLubyte *)"glXSwapBuffers");
    tprintf("glXSwapBuffers from: %s\n", owner(swap));
    tprintf("interposer forwarded: %d\n", forwarded());
}
