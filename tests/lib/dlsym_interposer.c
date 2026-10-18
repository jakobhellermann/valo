// TODO(ai-review): generated, not yet audited
//
// Unversioned dlsym interposer, like Steam's gameoverlayrenderer.so. Forwarding via a
// plain call breaks RTLD_NEXT for its callers, so only use it with other handles.
#define _GNU_SOURCE
#include <dlfcn.h>
#include <string.h>

static char last[64];

const char *interposer_last_dlsym(void) {
    return last;
}

void *dlsym(void *handle, const char *symbol) {
    static void *(*real)(void *, const char *);
    if (!real)
        real = dlvsym(RTLD_NEXT, "dlsym", "GLIBC_2.2.5");
    strncpy(last, symbol, sizeof(last) - 1);
    return real(handle, symbol);
}
