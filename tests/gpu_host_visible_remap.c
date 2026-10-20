//# link_flags: -lvulkan
//# gpu: yes
//# max_restores: 2
// Host-visible memory the GPU host allocates must come back across a restore
// as one object: the CPU mapping shows the checkpoint content, the GPU reads
// the same content, and a CPU write after the restore reaches the GPU.
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

static void gpu_copy(VkBuffer src, VkBuffer dst, VkDeviceSize size) {
    CHECK(vkResetCommandBuffer(cmdbuf, 0));
    VkCommandBufferBeginInfo bi = {.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
    CHECK(vkBeginCommandBuffer(cmdbuf, &bi));
    VkBufferCopy region = {.size = size};
    vkCmdCopyBuffer(cmdbuf, src, dst, 1, &region);
    CHECK(vkEndCommandBuffer(cmdbuf));
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
                             .pApplicationName = "gpu-host-visible-remap",
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
    VkCommandPoolCreateInfo pci = {.sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,
                                   .flags = VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT};
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
    VkBuffer upload, device_local, readback;
    char *upload_map, *readback_map;
    make_buffer(pd, size, host, &upload, (void **)&upload_map);
    make_buffer(pd, size, VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT, &device_local, NULL);
    make_buffer(pd, size, host, &readback, (void **)&readback_map);

    strcpy(upload_map, "PRESERVED");
    gpu_copy(upload, device_local, size);

    valo_checkpoint();

    tprintf("cpu: %s\n", upload_map);
    memset(readback_map, 0, size);
    gpu_copy(device_local, readback, size);
    tprintf("gpu: %s\n", readback_map);

    strcpy(upload_map, "IDENTITY");
    memset(readback_map, 0, size);
    gpu_copy(upload, readback, size);
    tprintf("identity: %s\n", readback_map);

    strcpy(upload_map, "CORRUPTED");
    gpu_copy(upload, device_local, size);

    valo_restore();
    return 1;
}
