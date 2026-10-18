//# link_mode: preload
// TODO(ai-review): generated, not yet audited
//
// dlsym(RTLD_NEXT) resolves relative to its caller's return address, so the
// interposed dlsym must not show up as the caller. Otherwise a preload after
// libvalo asking for its own symbol's next definition gets itself back (Steam's
// gameoverlayrenderer.so dlopen tail-jumped into itself forever).
#define _GNU_SOURCE
#include <dlfcn.h>
#include <string.h>

#include "testlib.h"

int main() {
    void *next = dlsym(RTLD_NEXT, "dlsym");
    Dl_info info;
    assert(next);
    int found = dladdr(next, &info);
    assert(found);
    const char *base = strrchr(info.dli_fname, '/');
    tprintf("dlsym(RTLD_NEXT, \"dlsym\") from main: %s\n", base ? base + 1 : info.dli_fname);
}
