//# link_flags: -lvulkan
//# build_lib: lib/fake_vk_layer
// TODO(ai-review): generated, not yet audited
//
// The Vulkan loader looks up each layer's vkGetInstanceProcAddr via dlsym on the
// layer's handle. Only the loader's export may be swapped for libvalo's hook:
// handing it out for a layer makes the hook call the loader, which calls the
// "layer" again (Steam's overlay layer: vkCreateInstance failed, Unity overflowed
// its stack).
#define _GNU_SOURCE
#include <dlfcn.h>
#include <libgen.h>
#include <limits.h>
#include <stdio.h>
#include <string.h>

#include "testlib.h"

static const char *owner(void *sym) {
    Dl_info info;
    int found = dladdr(sym, &info);
    assert(found);
    const char *base = strrchr(info.dli_fname, '/');
    return base ? base + 1 : info.dli_fname;
}

int main() {
    char exe[PATH_MAX];
    ssize_t len = readlink("/proc/self/exe", exe, sizeof(exe) - 1);
    assert(len > 0);
    exe[len] = 0;
    char layer_path[PATH_MAX];
    snprintf(layer_path, sizeof(layer_path), "%s/lib/fake_vk_layer.so", dirname(exe));

    void *layer = dlopen(layer_path, RTLD_NOW | RTLD_LOCAL);
    assert(layer);
    void *(*own)(void) = dlsym(layer, "fake_layer_gipa");
    assert(own);
    void *layer_gipa = dlsym(layer, "vkGetInstanceProcAddr");
    tprintf("layer vkGetInstanceProcAddr is its own: %s\n", layer_gipa == own() ? "yes" : "no");

    void *loader = dlopen("libvulkan.so.1", RTLD_NOW | RTLD_NOLOAD);
    assert(loader);
    tprintf("loader vkGetInstanceProcAddr from: %s\n", owner(dlsym(loader, "vkGetInstanceProcAddr")));
}
