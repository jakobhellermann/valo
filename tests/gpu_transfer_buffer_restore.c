//# link_flags: -lvulkan
//# gpu: yes
//# max_restores: 1
// TODO(ai-review): generated, not yet audited
//
// The content of a transfer buffer must come back at a restore. The host
// uploads it through a staging copy followed by a barrier; the barrier once
// named every access bit, which D3D12 rejects at command list close, so the
// restore aborted with VK_ERROR_OUT_OF_HOST_MEMORY.
#include <vulkan/vulkan.h>
#include <assert.h>
#include <stdint.h>
#include <string.h>
#include "testlib.h"

#define CHECK(x) assert((x) == VK_SUCCESS)

static const VkDeviceSize kSize = 64 * 1024;

static VkPhysicalDevice pd;
static VkDevice device;
static VkQueue queue;
static VkCommandBuffer cmdbuf;
static VkFence fence;
static VkBuffer device_buf, staging_buf;
static VkDeviceMemory staging_mem;
static uint8_t *staging_map;

static uint8_t pattern(VkDeviceSize i) { return (uint8_t)(i * 13 + 5); }

static uint32_t find_mem_type(uint32_t bits, VkMemoryPropertyFlags props) {
    VkPhysicalDeviceMemoryProperties mp;
    vkGetPhysicalDeviceMemoryProperties(pd, &mp);
    for (uint32_t i = 0; i < mp.memoryTypeCount; i++) {
        if ((bits & (1u << i)) && (mp.memoryTypes[i].propertyFlags & props) == props) return i;
    }
    assert(!"no memory type");
    return 0;
}

static VkDeviceMemory make_buffer(VkMemoryPropertyFlags props, VkBuffer *buf) {
    VkBufferCreateInfo bci = {
        .sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,
        .size = kSize,
        .usage = VK_BUFFER_USAGE_TRANSFER_SRC_BIT | VK_BUFFER_USAGE_TRANSFER_DST_BIT,
    };
    CHECK(vkCreateBuffer(device, &bci, NULL, buf));
    VkMemoryRequirements reqs;
    vkGetBufferMemoryRequirements(device, *buf, &reqs);
    VkMemoryAllocateInfo mai = {
        .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
        .allocationSize = reqs.size,
        .memoryTypeIndex = find_mem_type(reqs.memoryTypeBits, props),
    };
    VkDeviceMemory mem;
    CHECK(vkAllocateMemory(device, &mai, NULL, &mem));
    CHECK(vkBindBufferMemory(device, *buf, mem, 0));
    return mem;
}

static void copy(VkBuffer src, VkBuffer dst) {
    VkCommandBufferBeginInfo bi = {
        .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO,
        .flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT,
    };
    CHECK(vkBeginCommandBuffer(cmdbuf, &bi));
    VkBufferCopy region = {.size = kSize};
    vkCmdCopyBuffer(cmdbuf, src, dst, 1, &region);
    CHECK(vkEndCommandBuffer(cmdbuf));
    VkSubmitInfo si = {
        .sType = VK_STRUCTURE_TYPE_SUBMIT_INFO,
        .commandBufferCount = 1,
        .pCommandBuffers = &cmdbuf,
    };
    CHECK(vkQueueSubmit(queue, 1, &si, fence));
    CHECK(vkWaitForFences(device, 1, &fence, VK_TRUE, UINT64_MAX));
    CHECK(vkResetFences(device, 1, &fence));
}

static void upload(void) {
    for (VkDeviceSize i = 0; i < kSize; i++) staging_map[i] = pattern(i);
    VkMappedMemoryRange range = {.sType = VK_STRUCTURE_TYPE_MAPPED_MEMORY_RANGE,
                                 .memory = staging_mem,
                                 .size = VK_WHOLE_SIZE};
    CHECK(vkFlushMappedMemoryRanges(device, 1, &range));
    copy(staging_buf, device_buf);
}

static void check_and_clobber(void) {
    memset(staging_map, 0, kSize);
    VkMappedMemoryRange range = {.sType = VK_STRUCTURE_TYPE_MAPPED_MEMORY_RANGE,
                                 .memory = staging_mem,
                                 .size = VK_WHOLE_SIZE};
    CHECK(vkFlushMappedMemoryRanges(device, 1, &range));
    copy(device_buf, staging_buf);
    CHECK(vkInvalidateMappedMemoryRanges(device, 1, &range));
    VkDeviceSize mismatches = 0;
    for (VkDeviceSize i = 0; i < kSize; i++) mismatches += staging_map[i] != pattern(i);
    tprintf("device buffer mismatches: %llu\n", (unsigned long long)mismatches);

    // Zeroes the device buffer, so only a restore can bring the pattern back.
    memset(staging_map, 0, kSize);
    CHECK(vkFlushMappedMemoryRanges(device, 1, &range));
    copy(staging_buf, device_buf);
}

int main() {
    VkApplicationInfo app = {.sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
                             .pApplicationName = "gpu-transfer-buffer-restore",
                             .apiVersion = VK_API_VERSION_1_1};
    VkInstanceCreateInfo ici = {.sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
                                .pApplicationInfo = &app};
    VkInstance instance;
    CHECK(vkCreateInstance(&ici, NULL, &instance));
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

    VkCommandPoolCreateInfo cpci = {
        .sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,
        .flags = VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT,
    };
    VkCommandPool pool;
    CHECK(vkCreateCommandPool(device, &cpci, NULL, &pool));
    VkCommandBufferAllocateInfo cbai = {
        .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
        .commandPool = pool,
        .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY,
        .commandBufferCount = 1,
    };
    CHECK(vkAllocateCommandBuffers(device, &cbai, &cmdbuf));
    VkFenceCreateInfo fci = {.sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO};
    CHECK(vkCreateFence(device, &fci, NULL, &fence));

    make_buffer(VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT, &device_buf);
    staging_mem = make_buffer(VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT, &staging_buf);
    CHECK(vkMapMemory(device, staging_mem, 0, VK_WHOLE_SIZE, 0, (void **)&staging_map));

    // The counter lives in restored stack memory: a restore resumes after the
    // checkpoint with i == 1, so the check runs once live and once restored.
    for (volatile int i = 0;; i++) {
        if (i == 1) {
            upload();
            valo_checkpoint_slot(0);
        }
        if (i == 2) {
            check_and_clobber();
            valo_restore_slot(0);
        }
    }
}
