// TODO(ai-review): generated, not yet audited
//# link_flags: -lvulkan
//# gpu: yes
//# max_restores: 3
// GPU blob resources (exportable device-local memory) must survive
// checkpoint/restore with their content intact — verified through GPU copy
// operations, NOT through host mapping of the exportable memory (that path
// hits the kumquat CREATE_GUEST_HANDLE bug, which games may or may not hit).
//
// Flow:
//   1. create staging buffer (host-visible, mappable, NOT exportable)
//   2. create device-local buffer (exportable -> blob via kumquat)
//   3. fill staging with "PRESERVED", GPU-copy staging -> device buffer
//   4. valo_checkpoint()
//   5. overwrite staging with "CORRUPTED", GPU-copy staging -> device buffer
//   6. valo_restore()
//   7. GPU-copy device buffer -> staging, read staging:
//      must show "PRESERVED" again (the post-checkpoint "CORRUPTED"
//      copy was undone by the restore).
#include <vulkan/vulkan.h>
#include <assert.h>
#include <string.h>
#include <stdlib.h>
#include "testlib.h"

#define CHECK(x) assert((x) == VK_SUCCESS)

static VkDevice device;
static VkQueue queue;
static VkCommandPool cmdpool;
static VkCommandBuffer cmdbuf;
static VkFence fence;

static uint32_t find_mem_type(VkPhysicalDevice pd, uint32_t bits,
                              VkMemoryPropertyFlags props) {
    VkPhysicalDeviceMemoryProperties mp;
    vkGetPhysicalDeviceMemoryProperties(pd, &mp);
    for (uint32_t i = 0; i < mp.memoryTypeCount; i++) {
        if ((bits & (1u << i)) &&
            (mp.memoryTypes[i].propertyFlags & props) == props)
            return i;
    }
    return 0xffffffff;
}

static void make_buffer(VkDevice dev, VkPhysicalDevice pd, VkDeviceSize size,
                        VkBufferUsageFlags usage, VkMemoryPropertyFlags props,
                        VkExportMemoryAllocateInfo *export_info,
                        VkBuffer *buf, VkDeviceMemory *mem, void **map) {
    VkBufferCreateInfo bci = {
        .sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,
        .size = size,
        .usage = usage,
        .sharingMode = VK_SHARING_MODE_EXCLUSIVE,
    };
    CHECK(vkCreateBuffer(dev, &bci, NULL, buf));

    VkMemoryRequirements reqs;
    vkGetBufferMemoryRequirements(dev, *buf, &reqs);

    VkMemoryAllocateInfo mai = {
        .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
        .allocationSize = reqs.size,
        .memoryTypeIndex = find_mem_type(pd, reqs.memoryTypeBits, props),
    };
    if (export_info)
        mai.pNext = export_info;

    CHECK(vkAllocateMemory(dev, &mai, NULL, mem));
    CHECK(vkBindBufferMemory(dev, *buf, *mem, 0));

    if (map)
        CHECK(vkMapMemory(dev, *mem, 0, size, 0, map));
}

// The staging memory need not be coherent: host writes are flushed and
// device writes invalidated explicitly.
static void sync_staging(VkDeviceMemory mem, int to_device) {
    VkMappedMemoryRange range = {.sType = VK_STRUCTURE_TYPE_MAPPED_MEMORY_RANGE,
                                 .memory = mem,
                                 .size = VK_WHOLE_SIZE};
    if (to_device)
        CHECK(vkFlushMappedMemoryRanges(device, 1, &range));
    else
        CHECK(vkInvalidateMappedMemoryRanges(device, 1, &range));
}

static void gpu_copy(VkBuffer src, VkBuffer dst, VkDeviceSize size) {
    CHECK(vkResetCommandBuffer(cmdbuf, 0));
    VkCommandBufferBeginInfo bi = {
        .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO,
        .flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT,
    };
    CHECK(vkBeginCommandBuffer(cmdbuf, &bi));
    VkBufferCopy copy = {.size = size};
    vkCmdCopyBuffer(cmdbuf, src, dst, 1, &copy);
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
                             .pApplicationName = "gpu-reattach-test",
                             .apiVersion = VK_API_VERSION_1_2};
    VkInstanceCreateInfo ici = {.sType =
                                    VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
                                .pApplicationInfo = &app};
    CHECK(vkCreateInstance(&ici, NULL, &instance));

    VkPhysicalDevice pd;
    uint32_t count = 1;
    CHECK(vkEnumeratePhysicalDevices(instance, &count, &pd));

    const float prio = 1.0f;
    VkDeviceQueueCreateInfo dqci = {
        .sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
        .queueFamilyIndex = 0,
        .queueCount = 1,
        .pQueuePriorities = &prio,
    };
    VkDeviceCreateInfo dci = {.sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
                              .queueCreateInfoCount = 1,
                              .pQueueCreateInfos = &dqci};
    CHECK(vkCreateDevice(pd, &dci, NULL, &device));
    vkGetDeviceQueue(device, 0, 0, &queue);

    VkCommandPoolCreateInfo pci = {
        .sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,
        .flags = VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT,
    };
    CHECK(vkCreateCommandPool(device, &pci, NULL, &cmdpool));
    VkCommandBufferAllocateInfo cbi = {
        .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
        .commandPool = cmdpool,
        .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY,
        .commandBufferCount = 1,
    };
    CHECK(vkAllocateCommandBuffers(device, &cbi, &cmdbuf));

    VkFenceCreateInfo fci = {VK_STRUCTURE_TYPE_FENCE_CREATE_INFO};
    CHECK(vkCreateFence(device, &fci, NULL, &fence));

    const VkDeviceSize size = 4096;

    // Staging: host-visible, mappable, NOT exportable.
    VkBuffer staging;
    VkDeviceMemory staging_mem;
    void *staging_map;
    make_buffer(device, pd, size,
                VK_BUFFER_USAGE_TRANSFER_SRC_BIT |
                    VK_BUFFER_USAGE_TRANSFER_DST_BIT,
                VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT, NULL, &staging,
                &staging_mem, &staging_map);

    // Device-local: exportable (dmabuf) -> gfxstream blob resource via kumquat.
    VkBuffer devbuf;
    VkDeviceMemory devmem;
    VkExportMemoryAllocateInfo emai = {
        .sType = VK_STRUCTURE_TYPE_EXPORT_MEMORY_ALLOCATE_INFO,
        .handleTypes = VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT,
    };
    // NOEXPORT=1 skips the exportable allocation (bug isolation).
    VkExportMemoryAllocateInfo *export_ptr =
        getenv("NOEXPORT") ? NULL : &emai;
    make_buffer(device, pd, size, VK_BUFFER_USAGE_TRANSFER_SRC_BIT |
                                       VK_BUFFER_USAGE_TRANSFER_DST_BIT,
                VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT, export_ptr, &devbuf,
                &devmem, NULL);

    // 3. fill staging, GPU-copy into the device-local blob.
    strcpy(staging_map, "PRESERVED");
    sync_staging(staging_mem, 1);
    gpu_copy(staging, devbuf, size);

    // 4. checkpoint: GPU blob resources + backing exist now.
    valo_checkpoint();

    // Runs in both timelines (the restore jumps back here):
    //
    //   fresh:    blob = "PRESERVED" (copied above)
    //   restored: blob must be "PRESERVED" again — the code below corrupted
    //             it in the fresh timeline and the restore must have rewound
    //             the blob content via the GPU snapshot.
    //
    // The readback goes through the GPU (blob -> staging), exercising the
    // restored command pool/queue/host context.
    memset(staging_map, 0, size);
    sync_staging(staging_mem, 1);
    gpu_copy(devbuf, staging, size);
    sync_staging(staging_mem, 0);
    tprintf("blob readback: %s\n", (char *)staging_map);

    // 5. corrupt staging + blob (in the restored timeline this sets up the
    //    next cycle; the harness caps restores).
    strcpy(staging_map, "CORRUPTED");
    sync_staging(staging_mem, 1);
    gpu_copy(staging, devbuf, size);

    // 6. restore: undoes the CPU memory AND (via the GPU snapshot trigger)
    //    the blob resources. Jumps back above; the second pass exits via
    //    VALO_MAX_RESTORES, including repeated pipe acknowledgments.
    valo_restore();

    return 1;
}
