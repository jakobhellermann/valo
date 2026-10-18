// Copy bandwidth between a memfd shadow and each host-visible memory type of every Vulkan device.
#define _GNU_SOURCE
#include <vulkan/vulkan.h>

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

#define SIZE ((size_t)64 << 20)
#define PAGE 4096
#define RUNS 5

#define VK(expr)                                                              \
    do {                                                                      \
        VkResult r_ = (expr);                                                 \
        if (r_ != VK_SUCCESS) {                                               \
            fprintf(stderr, "FAILED %s: %d (line %d)\n", #expr, r_, __LINE__); \
            exit(1);                                                          \
        }                                                                     \
    } while (0)

static double now(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec + ts.tv_nsec * 1e-9;
}

// Copies every `stride`-th page (stride 1 = everything, as one memcpy).
static void copy_pages(char *dst, const char *src, size_t stride) {
    if (stride == 1) {
        memcpy(dst, src, SIZE);
        return;
    }
    for (size_t off = 0; off < SIZE; off += stride * PAGE) memcpy(dst + off, src + off, PAGE);
}

// Returns the best GB/s of the payload actually copied.
static double bench(char *dst, const char *src, size_t stride) {
    double best = 1e30;
    for (int i = 0; i < RUNS; i++) {
        double t = now();
        copy_pages(dst, src, stride);
        t = now() - t;
        if (t < best) best = t;
    }
    return (double)(SIZE / stride) / best / 1e9;
}

static void flags_str(VkMemoryPropertyFlags f, char *out) {
    out[0] = 0;
    if (f & VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT) strcat(out, "DEV ");
    if (f & VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT) strcat(out, "VIS ");
    if (f & VK_MEMORY_PROPERTY_HOST_COHERENT_BIT) strcat(out, "COH ");
    if (f & VK_MEMORY_PROPERTY_HOST_CACHED_BIT) strcat(out, "CACHED ");
}

static void report(const char *what, char *dst, const char *src) {
    printf("    %-34s all %6.2f GB/s | 1/4 pages %6.2f GB/s | 1/16 pages %6.2f GB/s\n", what,
           bench(dst, src, 1), bench(dst, src, 4), bench(dst, src, 16));
}

int main(void) {
    int memfd = memfd_create("shadow", MFD_CLOEXEC);
    if (memfd < 0 || ftruncate(memfd, SIZE) != 0) { perror("memfd"); return 1; }
    char *shadow = mmap(NULL, SIZE, PROT_READ | PROT_WRITE, MAP_SHARED, memfd, 0);
    char *anon = mmap(NULL, SIZE, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (shadow == MAP_FAILED || anon == MAP_FAILED) { perror("mmap"); return 1; }
    for (size_t i = 0; i < SIZE; i++) shadow[i] = (char)i;
    memset(anon, 1, SIZE);

    printf("baseline (memfd <-> anonymous memory)\n");
    report("memfd -> anon", anon, shadow);
    report("anon -> memfd", shadow, anon);

    VkApplicationInfo app = {.sType = VK_STRUCTURE_TYPE_APPLICATION_INFO, .apiVersion = VK_API_VERSION_1_2};
    VkInstanceCreateInfo ici = {.sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO, .pApplicationInfo = &app};
    VkInstance inst;
    VK(vkCreateInstance(&ici, NULL, &inst));
    uint32_t ndev = 0;
    VK(vkEnumeratePhysicalDevices(inst, &ndev, NULL));
    VkPhysicalDevice devs[8];
    VK(vkEnumeratePhysicalDevices(inst, &ndev, devs));

    for (uint32_t d = 0; d < ndev; d++) {
        VkPhysicalDeviceProperties props;
        vkGetPhysicalDeviceProperties(devs[d], &props);
        printf("\n%s\n", props.deviceName);
        float prio = 1.0f;
        VkDeviceQueueCreateInfo qci = {.sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
                                       .queueCount = 1, .pQueuePriorities = &prio};
        VkDeviceCreateInfo dci = {.sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
                                  .queueCreateInfoCount = 1, .pQueueCreateInfos = &qci};
        VkDevice dev;
        VK(vkCreateDevice(devs[d], &dci, NULL, &dev));
        VkPhysicalDeviceMemoryProperties mp;
        vkGetPhysicalDeviceMemoryProperties(devs[d], &mp);
        for (uint32_t t = 0; t < mp.memoryTypeCount; t++) {
            VkMemoryPropertyFlags f = mp.memoryTypes[t].propertyFlags;
            if (!(f & VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT)) continue;
            char fs[64];
            flags_str(f, fs);
            printf("  type %u [%s]\n", t, fs);
            VkMemoryAllocateInfo mai = {.sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
                                        .allocationSize = SIZE, .memoryTypeIndex = t};
            VkDeviceMemory mem;
            VkResult r = vkAllocateMemory(dev, &mai, NULL, &mem);
            if (r != VK_SUCCESS) { printf("    vkAllocateMemory failed: %d\n", r); continue; }
            char *p;
            VK(vkMapMemory(dev, mem, 0, VK_WHOLE_SIZE, 0, (void **)&p));
            report("shadow -> device (submit)", p, shadow);
            report("device -> shadow (fence)", shadow, p);
            vkUnmapMemory(dev, mem);
            vkFreeMemory(dev, mem, NULL);
        }
        vkDestroyDevice(dev, NULL);
    }
    vkDestroyInstance(inst, NULL);
    return 0;
}
