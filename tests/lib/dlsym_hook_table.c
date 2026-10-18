// TODO(ai-review): generated, not yet audited
//
// GL interposer like MangoHud's shim: its unversioned dlsym answers names it hooks with
// entries of a table. Built without -Bsymbolic, those entries bind to the first definition
// in the global scope, which is libvalo's when libvalo is preloaded first.
#define _GNU_SOURCE
#include <dlfcn.h>
#include <string.h>

typedef void *(*DlsymFn)(void *, const char *);
typedef void *(*GpaFn)(const unsigned char *);

static DlsymFn libc_dlsym(void) {
    static DlsymFn real;
    if (!real)
        real = (DlsymFn)dlvsym(RTLD_NEXT, "dlsym", "GLIBC_2.2.5");
    return real;
}

static int forwarded;

int table_interposer_forwarded(void) {
    return forwarded;
}

void *glXGetProcAddressARB(const unsigned char *name) {
    forwarded++;
    GpaFn next = (GpaFn)libc_dlsym()(RTLD_NEXT, "glXGetProcAddressARB");
    return next(name);
}

static const struct {
    const char *name;
    void *fn;
} hooks[] = {
    {"glXGetProcAddressARB", (void *)glXGetProcAddressARB},
};

void *dlsym(void *handle, const char *symbol) {
    void *found = libc_dlsym()(handle, symbol);
    if (!found)
        return found;
    for (size_t i = 0; i < sizeof(hooks) / sizeof(hooks[0]); i++)
        if (strcmp(hooks[i].name, symbol) == 0)
            return hooks[i].fn;
    return found;
}
