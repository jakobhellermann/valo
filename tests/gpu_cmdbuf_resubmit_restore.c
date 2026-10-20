//# link_flags: -lvulkan
//# gpu: yes
//# max_restores: 2
// A command buffer recorded before the checkpoint must stay executable after
// a restore: resubmitting it without re-recording runs the recorded copy.
#include <vulkan/vulkan.h>
#include <assert.h>
#include <stdio.h>
#include <string.h>
#include "testlib.h"

#define CHECK(x) assert((x) == VK_SUCCESS)

static VkDevice device;
static VkQueue queue;
static VkCommandBuffer cmdbuf;
static VkFence fence;

static uint32_t find_mem_type(VkPhysicalDevice pd, uint32_t bits, VkMemoryPropertyFlags props) {
    VkPhysicalDeviceMemoryProperties mp;
    vkGetPhysicalDeviceMemoryProperties(pd, &mp);
    for (uint32_t i = 0; i < mp.memoryTypeCount; i++) {
        if ((bits & (1u << i)) && (mp.memoryTypes[i].propertyFlags & props) == props) return i;
    }
    assert(!"no memory type");
    return 0;
}

static void make_buffer(VkPhysicalDevice pd, VkDeviceSize size, VkMemoryPropertyFlags props,
                        VkBuffer *buf, void **map) {
    VkBufferCreateInfo bci = {
        .sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,
        .size = size,
        .usage = VK_BUFFER_USAGE_TRANSFER_SRC_BIT | VK_BUFFER_USAGE_TRANSFER_DST_BIT,
    };
    CHECK(vkCreateBuffer(device, &bci, NULL, buf));
    VkMemoryRequirements reqs;
    vkGetBufferMemoryRequirements(device, *buf, &reqs);
    VkMemoryAllocateInfo mai = {
        .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
        .allocationSize = reqs.size,
        .memoryTypeIndex = find_mem_type(pd, reqs.memoryTypeBits, props),
    };
    VkDeviceMemory mem;
    CHECK(vkAllocateMemory(device, &mai, NULL, &mem));
    CHECK(vkBindBufferMemory(device, *buf, mem, 0));
    if (map) CHECK(vkMapMemory(device, mem, 0, size, 0, map));
}

static void submit(void) {
    VkSubmitInfo si = {.sType = VK_STRUCTURE_TYPE_SUBMIT_INFO,
                       .commandBufferCount = 1,
                       .pCommandBuffers = &cmdbuf};
    CHECK(vkQueueSubmit(queue, 1, &si, fence));
    CHECK(vkWaitForFences(device, 1, &fence, VK_TRUE, UINT64_MAX));
    CHECK(vkResetFences(device, 1, &fence));
}

int main() {
    VkInstance instance;
    VkApplicationInfo app = {.sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
                             .pApplicationName = "gpu-cmdbuf-resubmit-restore",
                             .apiVersion = VK_API_VERSION_1_2};
    VkInstanceCreateInfo ici = {.sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
                                .pApplicationInfo = &app};
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
    CHECK(vkCreateDevice(pd, &dci, NULL, &device));
    vkGetDeviceQueue(device, 0, 0, &queue);
    VkCommandPoolCreateInfo pci = {.sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO};
    VkCommandPool pool;
    CHECK(vkCreateCommandPool(device, &pci, NULL, &pool));
    VkCommandBufferAllocateInfo cbi = {.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
                                       .commandPool = pool,
                                       .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY,
                                       .commandBufferCount = 1};
    CHECK(vkAllocateCommandBuffers(device, &cbi, &cmdbuf));
    VkFenceCreateInfo fci = {VK_STRUCTURE_TYPE_FENCE_CREATE_INFO};
    CHECK(vkCreateFence(device, &fci, NULL, &fence));

    const VkDeviceSize size = 4096;
    const VkMemoryPropertyFlags host =
        VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT;
    VkBuffer src, dst;
    char *src_map, *dst_map;
    make_buffer(pd, size, host, &src, (void **)&src_map);
    make_buffer(pd, size, host, &dst, (void **)&dst_map);
    strcpy(src_map, "RECORDED");

    VkCommandBufferBeginInfo bi = {.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
    CHECK(vkBeginCommandBuffer(cmdbuf, &bi));
    VkBufferCopy region = {.size = size};
    vkCmdCopyBuffer(cmdbuf, src, dst, 1, &region);
    CHECK(vkEndCommandBuffer(cmdbuf));

    submit();
    tprintf("live: %s\n", dst_map);

    valo_checkpoint();

    memset(dst_map, 0, size);
    submit();
    tprintf("resubmit: %s\n", dst_map);

    valo_restore();
    return 1;
}
