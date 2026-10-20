//# link_flags: -lvulkan
//# gpu: yes
// Flushed and invalidated ranges of non-coherent mapped memory must round
// trip exactly, including VK_WHOLE_SIZE ranges that start at an offset and
// allocations whose size is not a multiple of the page size (the GPU path
// copies these ranges through the command stream when it cannot share the
// mapping with the host).
#include <vulkan/vulkan.h>
#include <assert.h>
#include <stdint.h>
#include <string.h>
#include "testlib.h"

#define CHECK(x) assert((x) == VK_SUCCESS)

static const VkDeviceSize kSize = 5000;
static const VkDeviceSize kFlushOffset = 1024;

static uint8_t pattern(VkDeviceSize i) { return (uint8_t)(i * 7 + 3); }

int main() {
    VkApplicationInfo app = {.sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
                             .pApplicationName = "gpu-host-visible-flush-invalidate",
                             .apiVersion = VK_API_VERSION_1_1};
    VkInstanceCreateInfo ici = {.sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
                                .pApplicationInfo = &app};
    VkInstance instance;
    CHECK(vkCreateInstance(&ici, NULL, &instance));

    VkPhysicalDevice pd;
    uint32_t count = 1;
    CHECK(vkEnumeratePhysicalDevices(instance, &count, &pd));

    const float prio = 1.0f;
    VkDeviceQueueCreateInfo dqci = {.sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
                                    .queueCount = 1,
                                    .pQueuePriorities = &prio};
    VkDeviceCreateInfo dci = {.sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
                              .queueCreateInfoCount = 1,
                              .pQueueCreateInfos = &dqci};
    VkDevice device;
    CHECK(vkCreateDevice(pd, &dci, NULL, &device));

    VkPhysicalDeviceMemoryProperties mp;
    vkGetPhysicalDeviceMemoryProperties(pd, &mp);
    uint32_t type = UINT32_MAX;
    for (uint32_t i = 0; i < mp.memoryTypeCount; i++) {
        if (mp.memoryTypes[i].propertyFlags & VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT) {
            type = i;
            break;
        }
    }
    assert(type != UINT32_MAX);

    VkMemoryAllocateInfo mai = {.sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
                                .allocationSize = kSize,
                                .memoryTypeIndex = type};
    VkDeviceMemory mem;
    CHECK(vkAllocateMemory(device, &mai, NULL, &mem));
    uint8_t *map;
    CHECK(vkMapMemory(device, mem, 0, VK_WHOLE_SIZE, 0, (void **)&map));

    for (VkDeviceSize i = 0; i < kSize; i++) map[i] = pattern(i);
    VkMappedMemoryRange flushed = {.sType = VK_STRUCTURE_TYPE_MAPPED_MEMORY_RANGE,
                                   .memory = mem,
                                   .offset = kFlushOffset,
                                   .size = VK_WHOLE_SIZE};
    CHECK(vkFlushMappedMemoryRanges(device, 1, &flushed));

    memset(map, 0, kSize);
    VkMappedMemoryRange whole = {.sType = VK_STRUCTURE_TYPE_MAPPED_MEMORY_RANGE,
                                 .memory = mem,
                                 .size = VK_WHOLE_SIZE};
    CHECK(vkInvalidateMappedMemoryRanges(device, 1, &whole));

    VkDeviceSize mismatches = 0;
    for (VkDeviceSize i = kFlushOffset; i < kSize; i++) mismatches += map[i] != pattern(i);
    tprintf("flushed tail mismatches after invalidate: %llu\n", (unsigned long long)mismatches);

    // A stream left out of step by the ranges above corrupts the calls after them.
    CHECK(vkInvalidateMappedMemoryRanges(device, 1, &flushed));
    CHECK(vkDeviceWaitIdle(device));
    tprintf("stream in step\n");

    vkUnmapMemory(device, mem);
    vkFreeMemory(device, mem, NULL);
    vkDestroyDevice(device, NULL);
    vkDestroyInstance(instance, NULL);
    return 0;
}
