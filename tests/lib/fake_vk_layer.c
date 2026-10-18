// TODO(ai-review): generated, not yet audited
//
// Stand-in for a Vulkan layer library: exports its own vkGetInstanceProcAddr.
static void *gipa(void *instance, const char *name) {
    (void)instance;
    (void)name;
    return 0;
}

void *vkGetInstanceProcAddr(void *instance, const char *name) __attribute__((alias("gipa")));

// The exported name is interposable (libvalo defines it too), the static one is not.
void *fake_layer_gipa(void) {
    return (void *)gipa;
}
