//# link_flags: -lvulkan
//# gpu: yes
//# max_restores: 6
// Host-visible GPU host memory across two slots: a buffer created between the
// slot 0 and slot 1 snapshots must come back with every slot 1 restore, and
// each restore leaves CPU mapping and GPU memory as one object.
#include <vulkan/vulkan.h>
#include <assert.h>
#include <stdio.h>
#include <string.h>
#include <fcntl.h>
#include <sys/stat.h>
#include <unistd.h>
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

static char pass_path[64];

static int pass_count(void) {
    struct stat st;
    return stat(pass_path, &st) == 0 ? (int)st.st_size : 0;
}

static void pass_bump(void) {
    int fd = open(pass_path, O_WRONLY | O_APPEND | O_CREAT, 0600);
    assert(fd >= 0);
    assert(write(fd, "x", 1) == 1);
    close(fd);
}

static void check(const char *label, VkBuffer buf, char *map, VkBuffer readback,
                  char *readback_map, VkDeviceSize size) {
    tprintf("%s cpu: %s\n", label, map);
    memset(readback_map, 0, size);
    gpu_copy(buf, readback, size);
    tprintf("%s gpu: %s\n", label, readback_map);
    strcpy(map, "IDENTITY");
    memset(readback_map, 0, size);
    gpu_copy(buf, readback, size);
    tprintf("%s identity: %s\n", label, readback_map);
}

int main() {
    snprintf(pass_path, sizeof(pass_path), "/tmp/valo-gpu-hv-multislot-%d", (int)getpid());
    unlink(pass_path);

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
    VkBuffer a, b, readback;
    char *a_map, *b_map, *readback_map;
    make_buffer(pd, size, host, &a, (void **)&a_map);
    make_buffer(pd, size, host, &readback, (void **)&readback_map);
    strcpy(a_map, "SLOT0");

    valo_checkpoint_slot(0);
    int pass = pass_count();
    pass_bump();
    tprintf("pass %d\n", pass);
    check("slot0 a", a, a_map, readback, readback_map, size);

    if (pass == 0) {
        make_buffer(pd, size, host, &b, (void **)&b_map);
        strcpy(a_map, "SLOT1A");
        strcpy(b_map, "SLOT1B");
        valo_checkpoint_slot(1);
    } else {
        valo_restore_slot(1);
    }
    check("slot1 a", a, a_map, readback, readback_map, size);
    check("slot1 b", b, b_map, readback, readback_map, size);
    valo_restore_slot(0);
    return 1;
}

