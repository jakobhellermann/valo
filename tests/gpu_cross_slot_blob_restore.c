//# link_flags: -lvulkan
//# gpu: yes
//# max_restores: 7
// Cross-slot blob restore: a GPU blob created BETWEEN two snapshots must
// come back from the LATER slot's original snapshot after an earlier-slot
// restore dropped it — and survive that cycle more than once.
//
// Host-side sequence this drives in kumquat/gfxstream:
//   snapshot slot 0 (base resources only)
//   create exportable blob (device-local buffer, dma-buf)
//   snapshot slot 1 (base + blob)
//   restore slot 0    <- kumquat drops the blob, archives it for slot 1
//   restore slot 1    <- replays the ORIGINAL slot-1 snapshot, re-inserts
//                        the archived wrapper, replays vkAllocateMemory
//                        against the re-registered dma-buf
//   [check]           <- blob content must round-trip through a GPU copy
//   ... restore 0, restore 1, [check] ... for three cycles total
//
// The second cycle is the known-death point: after the second slot-1
// restore the C++ side's blob memory has been observed empty ("no blob
// memory" -> GPUVM fault -> DEVICE_LOST -> SIGABRT); the first cycle has
// been observed to survive. A green first check followed by a red second
// check confirms that exact signature; any red check is a failure.
//
// Rewind mechanics: a restore resumes at the checkpoint call's
// continuation, so the "create blob + save slot 1" block would re-run
// after every slot-0 restore and supersede slot 1 — destroying the
// cross-slot scenario (the archive would never be used). The pass
// counter below is derived from a file in /tmp (process memory rewinds,
// the file system does not), so the create+save block runs exactly once
// and every later cycle restores the ORIGINAL slot-1 snapshot. This
// mirrors the manual HK repro (save 1, save 2, restore 1, restore 2),
// which triggers restores externally and never re-saves slot 2 in between.
//
// Pass map (rewind targets: slot 0 -> after checkpoint_slot(0), slot 1
// -> after checkpoint_slot(1)):
//   pass 0: create blob, save slot 1, [check], restore slot 0
//   pass 1: (post-restore-0, blob-less) restore slot 1
//   pass 2: [check after 1st cross-slot cycle], restore slot 0
//   pass 3: restore slot 1
//   pass 4: [check after 2nd cycle], restore slot 0
//   pass 5: restore slot 1
//   pass 6: [check after 3rd cycle], restore slot 0 -> exits via
//           VALO_MAX_RESTORES=7
#include <vulkan/vulkan.h>
#include <assert.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>
#include "testlib.h"

#define CHECK(x) assert((x) == VK_SUCCESS)
#define MAGIC "CROSSSLOT"

static VkDevice device;
static VkQueue queue;
static VkCommandPool cmdpool;
static VkCommandBuffer cmdbuf;
static VkFence fence;
static VkBuffer staging;
static VkDeviceMemory staging_mem;

// The blob under test. Statics rewind with the memory image: slot-0
// state leaves them zero (the blob does not exist there), slot-1 state
// carries the original handles — exactly what the checks rely on.
static VkBuffer devbuf;
static VkDeviceMemory devmem;

/// Pass counter across restores: process memory rewinds, so globals and
/// locals are useless for pass discrimination. One byte appended per
/// pass to this file survives every restore (only guest memory is
/// snapshotted); each pass re-stats the file instead of holding an fd
/// across a restore.
///
/// The read must happen AFTER a restore's rewind point — a checkpoint's
/// continuation is the line after its call, so anything read before the
/// checkpoint keeps its restored (stale) value on every later pass.
static char pass_path[64];

static int pass_count(void) {
    struct stat st;
    if (stat(pass_path, &st) != 0)
        return 0;
    return (int)st.st_size;
}

static void pass_bump(void) {
    int fd = open(pass_path, O_WRONLY | O_APPEND | O_CREAT, 0600);
    assert(fd >= 0);
    assert(write(fd, "x", 1) == 1);
    close(fd);
}

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
                        VkBufferUsageFlags usage,
                        VkMemoryPropertyFlags props,
                        VkExportMemoryAllocateInfo *export_info, VkBuffer *buf,
                        VkDeviceMemory *mem, void **map) {
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
static void sync_staging(int to_device) {
    VkMappedMemoryRange range = {.sType = VK_STRUCTURE_TYPE_MAPPED_MEMORY_RANGE,
                                 .memory = staging_mem,
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

/// Read the blob back through the GPU and verify the pattern. Runs live
/// on pass 0 and after every cross-slot restore slot 1 — the handles come
/// from the slot-1 image, the gfxstream resource from the replay, the
/// memory from the re-attached dma-buf. Any of the three being stale or
/// empty makes this fail (wrong content) or crash (DEVICE_LOST on
/// submit).
static void check_blob(void *staging_map) {
    // Fresh read: the label must reflect the actual number of completed
    // cross-slot cycles (restore0+restore1 pairs), not a rewound stack
    // value (slot-1 resumes land after the checkpoint_slot(1) call,
    // skipping the counter lines in main). One bump per pass0/slot-0
    // resume == one per started cycle.
    int cycle = pass_count() - 1;
    memset(staging_map, 0, 4096);
    sync_staging(1);
    gpu_copy(devbuf, staging, 4096);
    sync_staging(0);
    if (strcmp((char *)staging_map, MAGIC) != 0) {
        tprintf("cycle %d: FAIL: blob readback \"%s\", expected \"%s\"\n",
                cycle, (char *)staging_map, MAGIC);
        assert(!"blob content lost across cross-slot restore");
    }
    tprintf("cycle %d: blob readback: %s\n", cycle, (char *)staging_map);
}

int main() {
    snprintf(pass_path, sizeof(pass_path),
             "/tmp/valo-gpu-cross-slot-%d", (int)getpid());
    unlink(pass_path);

    VkInstance instance;
    VkApplicationInfo app = {.sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
                             .pApplicationName = "gpu-cross-slot",
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
    VkDeviceCreateInfo dci = {
        .sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
        .queueCreateInfoCount = 1,
        .pQueueCreateInfos = &dqci,
    };
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

    // Staging: host-visible, mappable, NOT exportable. Created before the
    // first snapshot, so both slots' images know it.
    void *staging_map;
    make_buffer(device, pd, size,
                VK_BUFFER_USAGE_TRANSFER_SRC_BIT |
                    VK_BUFFER_USAGE_TRANSFER_DST_BIT,
                VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT, NULL, &staging,
                &staging_mem, &staging_map);

    // Slot 0: base resources only. Every "restore slot 0" rewinds to the
    // line below — a state without the blob. The pass counter is read
    // HERE (after the rewind point), not earlier: the fresh read is what
    // distinguishes the first pass from every post-restore-0 pass.
    valo_checkpoint_slot(0);

    int pass = pass_count();
    pass_bump();
    tprintf("pass %d: begin\n", pass);

    if (pass == 0) {
        // Runs exactly once: create the blob BETWEEN the snapshots and
        // save the ORIGINAL slot 1. Later passes skip this (pass > 0), so
        // slot 1 is never superseded and every cross-slot restore replays
        // the original snapshot.

        // Device-local, exportable (dma-buf) -> kumquat blob resource,
        // the class that dies with "no blob memory" in the second
        // cross-slot cycle.
        static VkExportMemoryAllocateInfo emai = {
            .sType = VK_STRUCTURE_TYPE_EXPORT_MEMORY_ALLOCATE_INFO,
            .handleTypes = VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT,
        };
        make_buffer(device, pd, size,
                    VK_BUFFER_USAGE_TRANSFER_SRC_BIT |
                        VK_BUFFER_USAGE_TRANSFER_DST_BIT,
                    VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT, &emai, &devbuf,
                    &devmem, NULL);

        strcpy(staging_map, MAGIC);
        sync_staging(1);
        gpu_copy(staging, devbuf, size);

        valo_checkpoint_slot(1);
    } else {
        // Post-restore-0 timeline: the blob does not exist in guest
        // memory (its handles are slot-0 zeros). The only legal move is
        // straight back to slot 1, which replays the original snapshot
        // and re-inserts the blob the slot-0 restore dropped+archived.
        valo_restore_slot(1);
    }

    // Reached live on pass 0 and after every cross-slot restore slot 1
    // (which rewinds to the checkpoint_slot(1) continuation above).
    check_blob(staging_map);

    // Start the next cycle. Every pass reaching this line is even, so no
    // branch needed. The last iteration exits via VALO_MAX_RESTORES.
    valo_restore_slot(0);

    return 1;
}
