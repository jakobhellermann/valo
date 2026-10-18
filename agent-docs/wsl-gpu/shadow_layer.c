// Vulkan layer simulating copy-based coherence for host-visible memory.
//
// SHADOW_MODE=0  passthrough, only frame statistics
// SHADOW_MODE=1  host mappings are memfd shadows tracked with async uffd-wp; dirty pages are copied
//                to the real mapping before every vkQueueSubmit (no GPU->CPU copy-back)
// SHADOW_MODE=2  host mappings are memfd shadows synced only by vkFlush/InvalidateMappedMemoryRanges
// SHADOW_NONCOHERENT=1 hide HOST_COHERENT from the app's memory properties
// SHADOW_CACHED=1  allocate HOST_VISIBLE non-cached requests from a HOST_CACHED type instead
// SHADOW_NOVSYNC=1 force an uncapped present mode
#define _GNU_SOURCE
#include <vulkan/vk_layer.h>
#include <vulkan/vulkan.h>

#include <fcntl.h>
#include <linux/fs.h>
#include <linux/userfaultfd.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

#define PAGE 4096
#define EXPORT __attribute__((visibility("default")))

#define DIE(...)                                   \
    do {                                           \
        fprintf(stderr, "[shadow] FATAL: " __VA_ARGS__); \
        fprintf(stderr, "\n");                     \
        abort();                                   \
    } while (0)

static int g_mode, g_cached, g_novsync, g_noncoherent;
static FILE *g_log;
static pthread_mutex_t g_lock = PTHREAD_MUTEX_INITIALIZER;

static double now(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec + ts.tv_nsec * 1e-9;
}

// ---- dispatch -------------------------------------------------------------------------------

typedef struct {
    void *key;
    VkInstance instance;
    PFN_vkGetInstanceProcAddr gipa;
    PFN_vkDestroyInstance DestroyInstance;
    PFN_vkGetPhysicalDeviceMemoryProperties GetPhysicalDeviceMemoryProperties;
    PFN_vkGetPhysicalDeviceMemoryProperties2 GetPhysicalDeviceMemoryProperties2;
    PFN_vkGetPhysicalDeviceSurfacePresentModesKHR GetPhysicalDeviceSurfacePresentModesKHR;
} Inst;

typedef struct {
    void *key;
    VkDevice device;
    VkPhysicalDevice phys;
    PFN_vkGetDeviceProcAddr gdpa;
    PFN_vkDestroyDevice DestroyDevice;
    PFN_vkAllocateMemory AllocateMemory;
    PFN_vkFreeMemory FreeMemory;
    PFN_vkMapMemory MapMemory;
    PFN_vkUnmapMemory UnmapMemory;
    PFN_vkQueueSubmit QueueSubmit;
    PFN_vkQueuePresentKHR QueuePresentKHR;
    PFN_vkCreateSwapchainKHR CreateSwapchainKHR;
    PFN_vkFlushMappedMemoryRanges FlushMappedMemoryRanges;
    PFN_vkInvalidateMappedMemoryRanges InvalidateMappedMemoryRanges;
    VkPhysicalDeviceMemoryProperties mem_props;
    Inst *inst;
} Dev;

static Inst g_insts[8];
static Dev g_devs[8];

static void *key_of(const void *dispatchable) { return *(void **)dispatchable; }

static Inst *inst_of(const void *h) {
    for (int i = 0; i < 8; i++)
        if (g_insts[i].key == key_of(h)) return &g_insts[i];
    DIE("unknown instance");
}

static Dev *dev_of(const void *h) {
    for (int i = 0; i < 8; i++)
        if (g_devs[i].key == key_of(h)) return &g_devs[i];
    DIE("unknown device");
}

// ---- shadow mappings ----------------------------------------------------------------------

typedef struct {
    VkDeviceMemory mem;
    VkDeviceSize size;
    uint32_t type;
    char *real;    // driver mapping of the whole allocation
    char *shadow;  // memfd mapping handed to the app
    size_t shadow_size;
} Alloc;

#define MAX_ALLOCS 4096
static Alloc g_allocs[MAX_ALLOCS];
static int g_uffd = -1, g_pagemap = -1;
static struct page_region *g_regions;
static size_t g_regions_cap;

static Alloc *alloc_find(VkDeviceMemory mem) {
    for (int i = 0; i < MAX_ALLOCS; i++)
        if (g_allocs[i].mem == mem) return &g_allocs[i];
    return NULL;
}

static struct {
    double t0, last_frame, frame_sum, frame_max;
    unsigned long frames, submits, dirty_pages, copied_bytes;
    double sync_s, scan_s;
    unsigned long mapped_bytes, maps;
    unsigned long flushes, flushed_bytes, invalidates, invalidated_bytes;
} S;

static void uffd_init(void) {
    g_uffd = syscall(SYS_userfaultfd, O_CLOEXEC | UFFD_USER_MODE_ONLY);
    if (g_uffd < 0) DIE("userfaultfd: %m");
    struct uffdio_api api = {.api = UFFD_API,
                             .features = UFFD_FEATURE_WP_ASYNC | UFFD_FEATURE_WP_HUGETLBFS_SHMEM};
    if (ioctl(g_uffd, UFFDIO_API, &api) != 0) DIE("UFFDIO_API: %m");
    g_pagemap = open("/proc/self/pagemap", O_RDONLY | O_CLOEXEC);
    if (g_pagemap < 0) DIE("pagemap: %m");
}

static void shadow_create(Alloc *a) {
    a->shadow_size = (a->size + PAGE - 1) & ~(size_t)(PAGE - 1);
    int fd = memfd_create("shadow", MFD_CLOEXEC);
    if (fd < 0 || ftruncate(fd, a->shadow_size) != 0) DIE("memfd: %m");
    a->shadow = mmap(NULL, a->shadow_size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    close(fd);
    if (a->shadow == MAP_FAILED) DIE("mmap shadow: %m");
    memcpy(a->shadow, a->real, a->size);
    S.mapped_bytes += a->shadow_size;
    S.maps++;
    if (g_mode != 1) return;
    struct uffdio_register reg = {.range = {(unsigned long)a->shadow, a->shadow_size},
                                  .mode = UFFDIO_REGISTER_MODE_WP};
    if (ioctl(g_uffd, UFFDIO_REGISTER, &reg) != 0) DIE("UFFDIO_REGISTER: %m");
    struct uffdio_writeprotect wp = {.range = {(unsigned long)a->shadow, a->shadow_size},
                                     .mode = UFFDIO_WRITEPROTECT_MODE_WP};
    if (ioctl(g_uffd, UFFDIO_WRITEPROTECT, &wp) != 0) DIE("UFFDIO_WRITEPROTECT: %m");
}

// Copies pages written since the last sync from the shadow to the real mapping and re-protects them.
static void shadow_sync(Alloc *a) {
    size_t npages = a->shadow_size / PAGE;
    if (g_regions_cap < npages) {
        g_regions = realloc(g_regions, npages * sizeof(*g_regions));
        g_regions_cap = npages;
    }
    double t = now();
    struct pm_scan_arg arg = {
        .size = sizeof(arg),
        .flags = PM_SCAN_WP_MATCHING | PM_SCAN_CHECK_WPASYNC,
        .start = (unsigned long)a->shadow,
        .end = (unsigned long)(a->shadow + a->shadow_size),
        .vec = (unsigned long)g_regions,
        .vec_len = npages,
        .category_mask = PAGE_IS_WRITTEN,
        .return_mask = PAGE_IS_WRITTEN,
    };
    long n = ioctl(g_pagemap, PAGEMAP_SCAN, &arg);
    if (n < 0) DIE("PAGEMAP_SCAN: %m");
    S.scan_s += now() - t;
    for (long i = 0; i < n; i++) {
        size_t off = g_regions[i].start - (unsigned long)a->shadow;
        size_t len = g_regions[i].end - g_regions[i].start;
        if (off + len > a->size) len = a->size - off;
        memcpy(a->real + off, a->shadow + off, len);
        S.dirty_pages += (g_regions[i].end - g_regions[i].start) / PAGE;
        S.copied_bytes += len;
    }
}

static void shadow_sync_all(void) {
    double t = now();
    for (int i = 0; i < MAX_ALLOCS; i++)
        if (g_allocs[i].shadow) shadow_sync(&g_allocs[i]);
    S.sync_s += now() - t;
}

static void shadow_destroy(Alloc *a) {
    if (g_mode == 1) {
        shadow_sync(a);
        struct uffdio_range r = {(unsigned long)a->shadow, a->shadow_size};
        if (ioctl(g_uffd, UFFDIO_UNREGISTER, &r) != 0) DIE("UFFDIO_UNREGISTER: %m");
    }
    munmap(a->shadow, a->shadow_size);
    S.mapped_bytes -= a->shadow_size;
    a->shadow = NULL;
}

// ---- intercepted device functions ----------------------------------------------------------

static VKAPI_ATTR VkResult VKAPI_CALL l_AllocateMemory(VkDevice device, const VkMemoryAllocateInfo *info,
                                                      const VkAllocationCallbacks *cb, VkDeviceMemory *out) {
    Dev *d = dev_of(device);
    VkMemoryAllocateInfo mod = *info;
    VkMemoryPropertyFlags f = d->mem_props.memoryTypes[info->memoryTypeIndex].propertyFlags;
    if (g_cached && (f & VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT) && !(f & VK_MEMORY_PROPERTY_HOST_CACHED_BIT)) {
        for (uint32_t t = 0; t < d->mem_props.memoryTypeCount; t++) {
            VkMemoryPropertyFlags g = d->mem_props.memoryTypes[t].propertyFlags;
            if ((g & f) == f && (g & VK_MEMORY_PROPERTY_HOST_CACHED_BIT)) {
                mod.memoryTypeIndex = t;
                break;
            }
        }
    }
    VkResult r = d->AllocateMemory(device, &mod, cb, out);
    if (r != VK_SUCCESS) return r;
    pthread_mutex_lock(&g_lock);
    Alloc *a = alloc_find(VK_NULL_HANDLE);
    if (!a) DIE("too many allocations");
    *a = (Alloc){.mem = *out, .size = info->allocationSize, .type = mod.memoryTypeIndex};
    pthread_mutex_unlock(&g_lock);
    return r;
}

static VKAPI_ATTR void VKAPI_CALL l_FreeMemory(VkDevice device, VkDeviceMemory mem, const VkAllocationCallbacks *cb) {
    Dev *d = dev_of(device);
    pthread_mutex_lock(&g_lock);
    Alloc *a = mem ? alloc_find(mem) : NULL;
    if (a) {
        if (a->shadow) shadow_destroy(a);
        if (a->real) d->UnmapMemory(device, mem);
        memset(a, 0, sizeof(*a));
    }
    pthread_mutex_unlock(&g_lock);
    d->FreeMemory(device, mem, cb);
}

static VKAPI_ATTR VkResult VKAPI_CALL l_MapMemory(VkDevice device, VkDeviceMemory mem, VkDeviceSize offset,
                                                 VkDeviceSize size, VkMemoryMapFlags flags, void **pp) {
    Dev *d = dev_of(device);
    if (g_mode == 0) return d->MapMemory(device, mem, offset, size, flags, pp);
    pthread_mutex_lock(&g_lock);
    Alloc *a = alloc_find(mem);
    if (!a) DIE("map of unknown memory");
    if (a->real) DIE("double map");
    VkResult r = d->MapMemory(device, mem, 0, VK_WHOLE_SIZE, flags, (void **)&a->real);
    if (r == VK_SUCCESS) {
        shadow_create(a);
        *pp = a->shadow + offset;
    }
    pthread_mutex_unlock(&g_lock);
    return r;
}

static VKAPI_ATTR void VKAPI_CALL l_UnmapMemory(VkDevice device, VkDeviceMemory mem) {
    Dev *d = dev_of(device);
    if (g_mode == 0) {
        d->UnmapMemory(device, mem);
        return;
    }
    pthread_mutex_lock(&g_lock);
    Alloc *a = alloc_find(mem);
    if (!a || !a->real) DIE("unmap of unmapped memory");
    shadow_destroy(a);
    a->real = NULL;
    pthread_mutex_unlock(&g_lock);
    d->UnmapMemory(device, mem);
}

// Copies the given ranges between shadow and real mapping; to_real selects the direction.
static void ranges_copy(uint32_t n, const VkMappedMemoryRange *ranges, int to_real) {
    for (uint32_t i = 0; i < n; i++) {
        Alloc *a = alloc_find(ranges[i].memory);
        if (!a || !a->shadow) DIE("flush/invalidate of unmapped memory");
        VkDeviceSize off = ranges[i].offset;
        VkDeviceSize len = ranges[i].size == VK_WHOLE_SIZE ? a->size - off : ranges[i].size;
        if (off + len > a->size) DIE("range out of bounds");
        if (to_real)
            memcpy(a->real + off, a->shadow + off, len);
        else
            memcpy(a->shadow + off, a->real + off, len);
        if (to_real)
            S.flushed_bytes += len;
        else
            S.invalidated_bytes += len;
    }
}

static VKAPI_ATTR VkResult VKAPI_CALL l_FlushMappedMemoryRanges(VkDevice device, uint32_t n,
                                                               const VkMappedMemoryRange *ranges) {
    Dev *d = dev_of(device);
    pthread_mutex_lock(&g_lock);
    S.flushes++;
    if (g_mode == 2) {
        double t = now();
        ranges_copy(n, ranges, 1);
        S.sync_s += now() - t;
    }
    pthread_mutex_unlock(&g_lock);
    return d->FlushMappedMemoryRanges(device, n, ranges);
}

static VKAPI_ATTR VkResult VKAPI_CALL l_InvalidateMappedMemoryRanges(VkDevice device, uint32_t n,
                                                                    const VkMappedMemoryRange *ranges) {
    Dev *d = dev_of(device);
    VkResult r = d->InvalidateMappedMemoryRanges(device, n, ranges);
    pthread_mutex_lock(&g_lock);
    S.invalidates++;
    if (g_mode == 2) {
        double t = now();
        ranges_copy(n, ranges, 0);
        S.sync_s += now() - t;
    }
    pthread_mutex_unlock(&g_lock);
    return r;
}

static void strip_coherent(VkPhysicalDeviceMemoryProperties *p) {
    if (!g_noncoherent) return;
    for (uint32_t t = 0; t < p->memoryTypeCount; t++)
        p->memoryTypes[t].propertyFlags &= ~VK_MEMORY_PROPERTY_HOST_COHERENT_BIT;
}

static VKAPI_ATTR void VKAPI_CALL l_GetPhysicalDeviceMemoryProperties(VkPhysicalDevice phys,
                                                                     VkPhysicalDeviceMemoryProperties *p);
static VKAPI_ATTR void VKAPI_CALL l_GetPhysicalDeviceMemoryProperties2(VkPhysicalDevice phys,
                                                                      VkPhysicalDeviceMemoryProperties2 *p);

static VKAPI_ATTR VkResult VKAPI_CALL l_QueueSubmit(VkQueue queue, uint32_t n, const VkSubmitInfo *s, VkFence fence) {
    Dev *d = dev_of(queue);
    pthread_mutex_lock(&g_lock);
    S.submits++;
    if (g_mode == 1) shadow_sync_all();
    pthread_mutex_unlock(&g_lock);
    return d->QueueSubmit(queue, n, s, fence);
}

static void report(double t) {
    double span = t - S.t0;
    unsigned long f = S.frames ? S.frames : 1;
    fprintf(g_log,
            "[shadow] %5.1fs: %4lu frames, avg %6.2f ms (max %6.1f) | %5.1f submits/f | dirty %7.1f pages/f "
            "(%6.2f MiB/f) | sync %5.2f ms/f (scan %4.2f) | flush %5.1f/f %6.3f MiB/f | inval %4.1f/f %6.3f MiB/f "
            "| shadowed %4lu MiB in %lu maps\n",
            span, S.frames, S.frame_sum / f * 1e3, S.frame_max * 1e3, (double)S.submits / f,
            (double)S.dirty_pages / f, S.copied_bytes / (double)f / (1 << 20), S.sync_s / f * 1e3,
            S.scan_s / f * 1e3, (double)S.flushes / f, S.flushed_bytes / (double)f / (1 << 20),
            (double)S.invalidates / f, S.invalidated_bytes / (double)f / (1 << 20), S.mapped_bytes >> 20, S.maps);
    fflush(g_log);
    S.t0 = t;
    S.frames = S.submits = S.dirty_pages = S.copied_bytes = 0;
    S.flushes = S.flushed_bytes = S.invalidates = S.invalidated_bytes = 0;
    S.frame_sum = S.frame_max = S.sync_s = S.scan_s = 0;
}

static VKAPI_ATTR VkResult VKAPI_CALL l_QueuePresentKHR(VkQueue queue, const VkPresentInfoKHR *info) {
    Dev *d = dev_of(queue);
    VkResult r = d->QueuePresentKHR(queue, info);
    pthread_mutex_lock(&g_lock);
    double t = now();
    if (S.last_frame > 0) {
        double dt = t - S.last_frame;
        S.frame_sum += dt;
        if (dt > S.frame_max) S.frame_max = dt;
        S.frames++;
    }
    S.last_frame = t;
    if (S.t0 == 0) S.t0 = t;
    if (t - S.t0 >= 5.0) report(t);
    pthread_mutex_unlock(&g_lock);
    return r;
}

static VKAPI_ATTR VkResult VKAPI_CALL l_CreateSwapchainKHR(VkDevice device, const VkSwapchainCreateInfoKHR *info,
                                                          const VkAllocationCallbacks *cb, VkSwapchainKHR *out) {
    Dev *d = dev_of(device);
    VkSwapchainCreateInfoKHR mod = *info;
    if (g_novsync) {
        uint32_t n = 0;
        VkPresentModeKHR modes[16];
        d->inst->GetPhysicalDeviceSurfacePresentModesKHR(d->phys, info->surface, &n, NULL);
        if (n > 16) n = 16;
        d->inst->GetPhysicalDeviceSurfacePresentModesKHR(d->phys, info->surface, &n, modes);
        for (uint32_t i = 0; i < n; i++)
            if (modes[i] == VK_PRESENT_MODE_IMMEDIATE_KHR || modes[i] == VK_PRESENT_MODE_MAILBOX_KHR)
                mod.presentMode = modes[i];
        fprintf(g_log, "[shadow] present mode %d -> %d\n", info->presentMode, mod.presentMode);
    }
    return d->CreateSwapchainKHR(device, &mod, cb, out);
}

static VKAPI_ATTR void VKAPI_CALL l_DestroyDevice(VkDevice device, const VkAllocationCallbacks *cb) {
    Dev *d = dev_of(device);
    PFN_vkDestroyDevice fn = d->DestroyDevice;
    memset(d, 0, sizeof(*d));
    fn(device, cb);
}

// ---- layer plumbing -------------------------------------------------------------------------

static void init_once(void) {
    static int done;
    if (done) return;
    done = 1;
    const char *m = getenv("SHADOW_MODE");
    g_mode = m ? atoi(m) : 0;
    g_cached = getenv("SHADOW_CACHED") && atoi(getenv("SHADOW_CACHED"));
    g_novsync = getenv("SHADOW_NOVSYNC") && atoi(getenv("SHADOW_NOVSYNC"));
    g_noncoherent = getenv("SHADOW_NONCOHERENT") && atoi(getenv("SHADOW_NONCOHERENT"));
    const char *path = getenv("SHADOW_LOG");
    g_log = path ? fopen(path, "w") : stderr;
    if (!g_log) DIE("open log");
    fprintf(g_log, "[shadow] mode=%d cached=%d novsync=%d noncoherent=%d\n", g_mode, g_cached, g_novsync,
            g_noncoherent);
    if (g_mode == 1) uffd_init();
}

static VKAPI_ATTR VkResult VKAPI_CALL l_CreateInstance(const VkInstanceCreateInfo *ci, const VkAllocationCallbacks *cb,
                                                      VkInstance *out) {
    init_once();
    VkLayerInstanceCreateInfo *li = (VkLayerInstanceCreateInfo *)ci->pNext;
    while (li && !(li->sType == VK_STRUCTURE_TYPE_LOADER_INSTANCE_CREATE_INFO && li->function == VK_LAYER_LINK_INFO))
        li = (VkLayerInstanceCreateInfo *)li->pNext;
    if (!li) DIE("no instance link info");
    PFN_vkGetInstanceProcAddr gipa = li->u.pLayerInfo->pfnNextGetInstanceProcAddr;
    li->u.pLayerInfo = li->u.pLayerInfo->pNext;
    PFN_vkCreateInstance create = (PFN_vkCreateInstance)gipa(VK_NULL_HANDLE, "vkCreateInstance");
    VkResult r = create(ci, cb, out);
    if (r != VK_SUCCESS) return r;
    pthread_mutex_lock(&g_lock);
    Inst *in = NULL;
    for (int i = 0; i < 8 && !in; i++)
        if (!g_insts[i].key) in = &g_insts[i];
    if (!in) DIE("too many instances");
    in->key = key_of(*out);
    in->instance = *out;
    in->gipa = gipa;
    in->DestroyInstance = (PFN_vkDestroyInstance)gipa(*out, "vkDestroyInstance");
    in->GetPhysicalDeviceMemoryProperties =
        (PFN_vkGetPhysicalDeviceMemoryProperties)gipa(*out, "vkGetPhysicalDeviceMemoryProperties");
    in->GetPhysicalDeviceMemoryProperties2 =
        (PFN_vkGetPhysicalDeviceMemoryProperties2)gipa(*out, "vkGetPhysicalDeviceMemoryProperties2");
    if (!in->GetPhysicalDeviceMemoryProperties2)
        in->GetPhysicalDeviceMemoryProperties2 =
            (PFN_vkGetPhysicalDeviceMemoryProperties2)gipa(*out, "vkGetPhysicalDeviceMemoryProperties2KHR");
    in->GetPhysicalDeviceSurfacePresentModesKHR =
        (PFN_vkGetPhysicalDeviceSurfacePresentModesKHR)gipa(*out, "vkGetPhysicalDeviceSurfacePresentModesKHR");
    pthread_mutex_unlock(&g_lock);
    return r;
}

static VKAPI_ATTR void VKAPI_CALL l_DestroyInstance(VkInstance instance, const VkAllocationCallbacks *cb) {
    Inst *in = inst_of(instance);
    PFN_vkDestroyInstance fn = in->DestroyInstance;
    memset(in, 0, sizeof(*in));
    fn(instance, cb);
}

static VKAPI_ATTR VkResult VKAPI_CALL l_CreateDevice(VkPhysicalDevice phys, const VkDeviceCreateInfo *ci,
                                                    const VkAllocationCallbacks *cb, VkDevice *out) {
    Inst *in = inst_of(phys);
    VkLayerDeviceCreateInfo *li = (VkLayerDeviceCreateInfo *)ci->pNext;
    while (li && !(li->sType == VK_STRUCTURE_TYPE_LOADER_DEVICE_CREATE_INFO && li->function == VK_LAYER_LINK_INFO))
        li = (VkLayerDeviceCreateInfo *)li->pNext;
    if (!li) DIE("no device link info");
    PFN_vkGetInstanceProcAddr gipa = li->u.pLayerInfo->pfnNextGetInstanceProcAddr;
    PFN_vkGetDeviceProcAddr gdpa = li->u.pLayerInfo->pfnNextGetDeviceProcAddr;
    li->u.pLayerInfo = li->u.pLayerInfo->pNext;
    PFN_vkCreateDevice create = (PFN_vkCreateDevice)gipa(in->instance, "vkCreateDevice");
    VkResult r = create(phys, ci, cb, out);
    if (r != VK_SUCCESS) return r;
    for (uint32_t i = 0; i < ci->enabledExtensionCount; i++)
        fprintf(g_log, "[shadow] device extension %s\n", ci->ppEnabledExtensionNames[i]);
    pthread_mutex_lock(&g_lock);
    Dev *d = NULL;
    for (int i = 0; i < 8 && !d; i++)
        if (!g_devs[i].key) d = &g_devs[i];
    if (!d) DIE("too many devices");
    d->key = key_of(*out);
    d->device = *out;
    d->phys = phys;
    d->inst = in;
    d->gdpa = gdpa;
#define LOAD(name) d->name = (PFN_vk##name)gdpa(*out, "vk" #name)
    LOAD(DestroyDevice);
    LOAD(AllocateMemory);
    LOAD(FreeMemory);
    LOAD(MapMemory);
    LOAD(UnmapMemory);
    LOAD(QueueSubmit);
    LOAD(QueuePresentKHR);
    LOAD(CreateSwapchainKHR);
    LOAD(FlushMappedMemoryRanges);
    LOAD(InvalidateMappedMemoryRanges);
#undef LOAD
    in->GetPhysicalDeviceMemoryProperties(phys, &d->mem_props);
    for (uint32_t t = 0; t < d->mem_props.memoryTypeCount; t++)
        fprintf(g_log, "[shadow] memory type %u flags 0x%x\n", t, d->mem_props.memoryTypes[t].propertyFlags);
    pthread_mutex_unlock(&g_lock);
    return r;
}

static PFN_vkVoidFunction device_hook(const char *name) {
#define HOOK(fn) \
    if (!strcmp(name, "vk" #fn)) return (PFN_vkVoidFunction)l_##fn;
    HOOK(DestroyDevice);
    HOOK(AllocateMemory);
    HOOK(FreeMemory);
    HOOK(MapMemory);
    HOOK(UnmapMemory);
    HOOK(QueueSubmit);
    HOOK(QueuePresentKHR);
    HOOK(CreateSwapchainKHR);
    HOOK(FlushMappedMemoryRanges);
    HOOK(InvalidateMappedMemoryRanges);
#undef HOOK
    if (g_mode == 1 && (!strcmp(name, "vkMapMemory2KHR") || !strcmp(name, "vkMapMemory2") ||
                        !strcmp(name, "vkQueueSubmit2") || !strcmp(name, "vkQueueSubmit2KHR")))
        fprintf(g_log, "[shadow] WARNING: app queried unhooked %s\n", name);
    return NULL;
}

EXPORT VKAPI_ATTR PFN_vkVoidFunction VKAPI_CALL shadow_GetDeviceProcAddr(VkDevice device, const char *name);

EXPORT VKAPI_ATTR PFN_vkVoidFunction VKAPI_CALL shadow_GetInstanceProcAddr(VkInstance instance, const char *name) {
    if (!strcmp(name, "vkGetInstanceProcAddr")) return (PFN_vkVoidFunction)shadow_GetInstanceProcAddr;
    if (!strcmp(name, "vkGetDeviceProcAddr")) return (PFN_vkVoidFunction)shadow_GetDeviceProcAddr;
    if (!strcmp(name, "vkCreateInstance")) return (PFN_vkVoidFunction)l_CreateInstance;
    if (!strcmp(name, "vkDestroyInstance")) return (PFN_vkVoidFunction)l_DestroyInstance;
    if (!strcmp(name, "vkCreateDevice")) return (PFN_vkVoidFunction)l_CreateDevice;
    if (!strcmp(name, "vkGetPhysicalDeviceMemoryProperties"))
        return (PFN_vkVoidFunction)l_GetPhysicalDeviceMemoryProperties;
    if (!strcmp(name, "vkGetPhysicalDeviceMemoryProperties2") ||
        !strcmp(name, "vkGetPhysicalDeviceMemoryProperties2KHR"))
        return (PFN_vkVoidFunction)l_GetPhysicalDeviceMemoryProperties2;
    PFN_vkVoidFunction hook = device_hook(name);
    if (hook) return hook;
    if (!instance) return NULL;
    return inst_of(instance)->gipa(instance, name);
}

EXPORT VKAPI_ATTR PFN_vkVoidFunction VKAPI_CALL shadow_GetDeviceProcAddr(VkDevice device, const char *name) {
    if (!strcmp(name, "vkGetDeviceProcAddr")) return (PFN_vkVoidFunction)shadow_GetDeviceProcAddr;
    PFN_vkVoidFunction hook = device_hook(name);
    if (hook) return hook;
    return dev_of(device)->gdpa(device, name);
}

static VKAPI_ATTR void VKAPI_CALL l_GetPhysicalDeviceMemoryProperties(VkPhysicalDevice phys,
                                                                     VkPhysicalDeviceMemoryProperties *p) {
    inst_of(phys)->GetPhysicalDeviceMemoryProperties(phys, p);
    strip_coherent(p);
}

static VKAPI_ATTR void VKAPI_CALL l_GetPhysicalDeviceMemoryProperties2(VkPhysicalDevice phys,
                                                                      VkPhysicalDeviceMemoryProperties2 *p) {
    inst_of(phys)->GetPhysicalDeviceMemoryProperties2(phys, p);
    strip_coherent(&p->memoryProperties);
}
