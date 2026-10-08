//# link_flags: -lvulkan
//# gpu: yes
//# max_restores: 4
// Descriptor set bindings must survive snapshot/restore cycles.
//
// Known-bug regression test (currently RED): kumquat/gfxstream skip
// descriptor sets without host backing at snapshot save; the restore
// replay lazily re-allocates them EMPTY. Reads through the set then
// return zeros (robustness null descriptor) instead of the buffer
// content — the "deterministic blackscreen" class of Hollow Knight
// sessions.
//
// The compute shader copies 1024 uints from an input buffer to an
// output buffer THROUGH the descriptor set; every copy must reproduce
// the original pattern.
//
// Loop shape (the restore rewinds to the slot-0 checkpoint continuation,
// so the i==2 save runs again post-restore — producing the trimmed-log
// save suspected as the trigger):
//   i=1: copy, save slot 0
//   i=2: copy, save slot 1   <- after a restore this is the trimmed save
//   i=3: copy, restore slot 0  -> rewinds to the i==2 continuation
//   ... until VALO_MAX_RESTORES terminates the process (exit 0).
//
// Requires a kumquat build with savestate slots:
//   VALO_KUMQUAT_BIN=<kumquat> VALO_GFXSTREAM_ICD=<guest icd json> \
//       cargo valo test gpu_descriptor_set_restore
#include <vulkan/vulkan.h>
#include <assert.h>
#include <string.h>
#include "testlib.h"

#define CHECK(x) assert((x) == VK_SUCCESS)
#define N 1024

static VkDevice device;
static VkQueue queue;
static VkCommandPool cmdpool;
static VkCommandBuffer cmdbuf;
static VkFence fence;
static VkDescriptorSet desc_set;
static VkPipeline pipeline;
static VkPipelineLayout pipelayout;
static VkBuffer in_buf, out_buf;
static VkDeviceMemory in_mem, out_mem;
static uint32_t *out_mapped;

static const uint32_t shader_spv[] = {
    0x07230203, 0x00010000, 0x000d000b, 0x00000029, 0x00000000, 0x00020011,
    0x00000001, 0x0006000b, 0x00000001, 0x4c534c47, 0x6474732e, 0x3035342e,
    0x00000000, 0x0003000e, 0x00000000, 0x00000001, 0x0006000f, 0x00000005,
    0x00000004, 0x6e69616d, 0x00000000, 0x0000000b, 0x00060010, 0x00000004,
    0x00000011, 0x00000040, 0x00000001, 0x00000001, 0x00030003, 0x00000002,
    0x000001c2, 0x000a0004, 0x475f4c47, 0x4c474f4f, 0x70635f45, 0x74735f70,
    0x5f656c79, 0x656e696c, 0x7269645f, 0x69746365, 0x00006576, 0x00080004,
    0x475f4c47, 0x4c474f4f, 0x6e695f45, 0x64756c63, 0x69645f65, 0x74636572,
    0x00657669, 0x00040005, 0x00000004, 0x6e69616d, 0x00000000, 0x00030005,
    0x00000008, 0x00000069, 0x00080005, 0x0000000b, 0x475f6c67, 0x61626f6c,
    0x766e496c, 0x7461636f, 0x496e6f69, 0x00000044, 0x00040005, 0x00000017,
    0x4274754f, 0x00006675, 0x00050006, 0x00000017, 0x00000000, 0x7674756f,
    0x00000000, 0x00030005, 0x00000019, 0x00000000, 0x00040005, 0x0000001e,
    0x75426e49, 0x00000066, 0x00040006, 0x0000001e, 0x00000000, 0x00766e69,
    0x00030005, 0x00000020, 0x00000000, 0x00040047, 0x0000000b, 0x0000000b,
    0x0000001c, 0x00040047, 0x00000016, 0x00000006, 0x00000004, 0x00030047,
    0x00000017, 0x00000003, 0x00040048, 0x00000017, 0x00000000, 0x00000019,
    0x00050048, 0x00000017, 0x00000000, 0x00000023, 0x00000000, 0x00030047,
    0x00000019, 0x00000019, 0x00040047, 0x00000019, 0x00000021, 0x00000001,
    0x00040047, 0x00000019, 0x00000022, 0x00000000, 0x00040047, 0x0000001d,
    0x00000006, 0x00000004, 0x00030047, 0x0000001e, 0x00000003, 0x00040048,
    0x0000001e, 0x00000000, 0x00000018, 0x00050048, 0x0000001e, 0x00000000,
    0x00000023, 0x00000000, 0x00030047, 0x00000020, 0x00000018, 0x00040047,
    0x00000020, 0x00000021, 0x00000000, 0x00040047, 0x00000020, 0x00000022,
    0x00000000, 0x00040047, 0x00000028, 0x0000000b, 0x00000019, 0x00020013,
    0x00000002, 0x00030021, 0x00000003, 0x00000002, 0x00040015, 0x00000006,
    0x00000020, 0x00000000, 0x00040020, 0x00000007, 0x00000007, 0x00000006,
    0x00040017, 0x00000009, 0x00000006, 0x00000003, 0x00040020, 0x0000000a,
    0x00000001, 0x00000009, 0x0004003b, 0x0000000a, 0x0000000b, 0x00000001,
    0x0004002b, 0x00000006, 0x0000000c, 0x00000000, 0x00040020, 0x0000000d,
    0x00000001, 0x00000006, 0x0004002b, 0x00000006, 0x00000011, 0x00000400,
    0x00020014, 0x00000012, 0x0004001c, 0x00000016, 0x00000006, 0x00000011,
    0x0003001e, 0x00000017, 0x00000016, 0x00040020, 0x00000018, 0x00000002,
    0x00000017, 0x0004003b, 0x00000018, 0x00000019, 0x00000002, 0x00040015,
    0x0000001a, 0x00000020, 0x00000001, 0x0004002b, 0x0000001a, 0x0000001b,
    0x00000000, 0x0004001c, 0x0000001d, 0x00000006, 0x00000011, 0x0003001e,
    0x0000001e, 0x0000001d, 0x00040020, 0x0000001f, 0x00000002, 0x0000001e,
    0x0004003b, 0x0000001f, 0x00000020, 0x00000002, 0x00040020, 0x00000022,
    0x00000002, 0x00000006, 0x0004002b, 0x00000006, 0x00000026, 0x00000040,
    0x0004002b, 0x00000006, 0x00000027, 0x00000001, 0x0006002c, 0x00000009,
    0x00000028, 0x00000026, 0x00000027, 0x00000027, 0x00050036, 0x00000002,
    0x00000004, 0x00000000, 0x00000003, 0x000200f8, 0x00000005, 0x0004003b,
    0x00000007, 0x00000008, 0x00000007, 0x00050041, 0x0000000d, 0x0000000e,
    0x0000000b, 0x0000000c, 0x0004003d, 0x00000006, 0x0000000f, 0x0000000e,
    0x0003003e, 0x00000008, 0x0000000f, 0x0004003d, 0x00000006, 0x00000010,
    0x00000008, 0x000500b0, 0x00000012, 0x00000013, 0x00000010, 0x00000011,
    0x000300f7, 0x00000015, 0x00000000, 0x000400fa, 0x00000013, 0x00000014,
    0x00000015, 0x000200f8, 0x00000014, 0x0004003d, 0x00000006, 0x0000001c,
    0x00000008, 0x0004003d, 0x00000006, 0x00000021, 0x00000008, 0x00060041,
    0x00000022, 0x00000023, 0x00000020, 0x0000001b, 0x00000021, 0x0004003d,
    0x00000006, 0x00000024, 0x00000023, 0x00060041, 0x00000022, 0x00000025,
    0x00000019, 0x0000001b, 0x0000001c, 0x0003003e, 0x00000025, 0x00000024,
    0x000200f9, 0x00000015, 0x000200f8, 0x00000015, 0x000100fd, 0x00010038,
};

static uint32_t find_mem_type(VkPhysicalDevice pd, uint32_t bits,
                              VkMemoryPropertyFlags props) {
    VkPhysicalDeviceMemoryProperties mp;
    vkGetPhysicalDeviceMemoryProperties(pd, &mp);
    for (uint32_t i = 0; i < mp.memoryTypeCount; i++)
        if ((bits & (1u << i)) &&
            (mp.memoryTypes[i].propertyFlags & props) == props)
            return i;
    return 0xffffffff;
}

static void make_buffer(VkDevice dev, VkPhysicalDevice pd, VkDeviceSize size,
                        VkBuffer *buf, VkDeviceMemory *mem, void **map) {
    VkBufferCreateInfo bci = {
        .sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,
        .size = size,
        .usage = VK_BUFFER_USAGE_STORAGE_BUFFER_BIT,
        .sharingMode = VK_SHARING_MODE_EXCLUSIVE,
    };
    CHECK(vkCreateBuffer(dev, &bci, NULL, buf));

    VkMemoryRequirements reqs;
    vkGetBufferMemoryRequirements(dev, *buf, &reqs);

    VkMemoryAllocateInfo mai = {
        .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
        .allocationSize = reqs.size,
        .memoryTypeIndex = find_mem_type(pd, reqs.memoryTypeBits,
                                         VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT |
                                             VK_MEMORY_PROPERTY_HOST_COHERENT_BIT),
    };
    CHECK(vkAllocateMemory(dev, &mai, NULL, mem));
    CHECK(vkBindBufferMemory(dev, *buf, *mem, 0));
    if (map)
        CHECK(vkMapMemory(dev, *mem, 0, VK_WHOLE_SIZE, 0, map));
}

/// Run the descriptor-copy and verify the output equals the pattern.
static void run_copy(const char *phase) {
    // Re-record: the bind produces a descriptor-set commit with
    // pendingAlloc==false — the exact production path that hits the
    // lazily re-allocated (possibly empty) host set after a restore.
    CHECK(vkResetFences(device, 1, &fence));
    CHECK(vkResetCommandBuffer(cmdbuf, 0));
    VkCommandBufferBeginInfo bi = {
        .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO,
    };
    CHECK(vkBeginCommandBuffer(cmdbuf, &bi));
    vkCmdBindPipeline(cmdbuf, VK_PIPELINE_BIND_POINT_COMPUTE, pipeline);
    vkCmdBindDescriptorSets(cmdbuf, VK_PIPELINE_BIND_POINT_COMPUTE, pipelayout,
                            0, 1, &desc_set, 0, NULL);
    vkCmdDispatch(cmdbuf, N / 64, 1, 1);
    CHECK(vkEndCommandBuffer(cmdbuf));

    VkSubmitInfo si = {
        .sType = VK_STRUCTURE_TYPE_SUBMIT_INFO,
        .commandBufferCount = 1,
        .pCommandBuffers = &cmdbuf,
    };
    CHECK(vkQueueSubmit(queue, 1, &si, fence));
    CHECK(vkWaitForFences(device, 1, &fence, VK_TRUE, (uint64_t)-1));

    for (uint32_t i = 0; i < N; i++) {
        uint32_t expect = 0xC0FFEE00u + i;
        if (out_mapped[i] != expect) {
            tprintf("%s: FAIL: out[%u]=0x%x expected 0x%x\n", phase, i,
                    out_mapped[i], expect);
            assert(!"descriptor bindings lost across restore");
        }
    }
    tprintf("%s: PASS\n", phase);
}

int main(void) {
    VkApplicationInfo app = {
        .sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
        .apiVersion = VK_API_VERSION_1_0,
    };
    VkInstanceCreateInfo ici = {
        .sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
        .pApplicationInfo = &app,
    };
    VkInstance instance;
    CHECK(vkCreateInstance(&ici, NULL, &instance));

    VkPhysicalDevice pd;
    uint32_t count = 1;
    CHECK(vkEnumeratePhysicalDevices(instance, &count, &pd));

    // queue family with compute
    uint32_t qf_count = 0;
    vkGetPhysicalDeviceQueueFamilyProperties(pd, &qf_count, NULL);
    VkQueueFamilyProperties qf[16];
    if (qf_count > 16) qf_count = 16;
    vkGetPhysicalDeviceQueueFamilyProperties(pd, &qf_count, qf);
    uint32_t qf_idx = 0xffffffff;
    for (uint32_t i = 0; i < qf_count; i++)
        if (qf[i].queueFlags & VK_QUEUE_COMPUTE_BIT) { qf_idx = i; break; }
    assert(qf_idx != 0xffffffff);

    float prio = 1.0f;
    VkDeviceQueueCreateInfo qci = {
        .sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
        .queueFamilyIndex = qf_idx,
        .queueCount = 1,
        .pQueuePriorities = &prio,
    };
    VkDeviceCreateInfo dci = {
        .sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
        .queueCreateInfoCount = 1,
        .pQueueCreateInfos = &qci,
    };
    CHECK(vkCreateDevice(pd, &dci, NULL, &device));
    vkGetDeviceQueue(device, qf_idx, 0, &queue);

    VkCommandPoolCreateInfo pci = {
        .sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,
        .queueFamilyIndex = qf_idx,
    };
    CHECK(vkCreateCommandPool(device, &pci, NULL, &cmdpool));
    VkCommandBufferAllocateInfo cbi = {
        .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
        .commandPool = cmdpool,
        .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY,
        .commandBufferCount = 1,
    };
    CHECK(vkAllocateCommandBuffers(device, &cbi, &cmdbuf));
    VkFenceCreateInfo fci = { .sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO };
    CHECK(vkCreateFence(device, &fci, NULL, &fence));

    make_buffer(device, pd, N * 4, &in_buf, &in_mem, NULL);
    make_buffer(device, pd, N * 4, &out_buf, &out_mem, (void **)&out_mapped);

    // fill the input buffer through a one-off map
    void *in_map;
    CHECK(vkMapMemory(device, in_mem, 0, VK_WHOLE_SIZE, 0, &in_map));
    for (uint32_t i = 0; i < N; i++) ((uint32_t *)in_map)[i] = 0xC0FFEE00u + i;
    vkUnmapMemory(device, in_mem);

    VkDescriptorPoolSize pool_sizes = {
        .type = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
        .descriptorCount = 2,
    };
    VkDescriptorPoolCreateInfo dpci = {
        .sType = VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO,
        .maxSets = 4,
        .poolSizeCount = 1,
        .pPoolSizes = &pool_sizes,
    };
    VkDescriptorPool pool;
    CHECK(vkCreateDescriptorPool(device, &dpci, NULL, &pool));

    VkDescriptorSetLayoutBinding bindings[2] = {
        { .binding = 0,
          .descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
          .descriptorCount = 1,
          .stageFlags = VK_SHADER_STAGE_COMPUTE_BIT },
        { .binding = 1,
          .descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
          .descriptorCount = 1,
          .stageFlags = VK_SHADER_STAGE_COMPUTE_BIT },
    };
    VkDescriptorSetLayoutCreateInfo dlci = {
        .sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO,
        .bindingCount = 2,
        .pBindings = bindings,
    };
    VkDescriptorSetLayout layout;
    CHECK(vkCreateDescriptorSetLayout(device, &dlci, NULL, &layout));

    VkDescriptorSetAllocateInfo dsai = {
        .sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO,
        .descriptorPool = pool,
        .descriptorSetCount = 1,
        .pSetLayouts = &layout,
    };
    CHECK(vkAllocateDescriptorSets(device, &dsai, &desc_set));

    VkDescriptorBufferInfo bin = { .buffer = in_buf, .range = VK_WHOLE_SIZE };
    VkDescriptorBufferInfo bout = { .buffer = out_buf, .range = VK_WHOLE_SIZE };
    VkWriteDescriptorSet writes[2] = {
        { .sType = VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET,
          .dstSet = desc_set,
          .dstBinding = 0,
          .descriptorCount = 1,
          .descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
          .pBufferInfo = &bin },
        { .sType = VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET,
          .dstSet = desc_set,
          .dstBinding = 1,
          .descriptorCount = 1,
          .descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
          .pBufferInfo = &bout },
    };
    vkUpdateDescriptorSets(device, 2, writes, 0, NULL);

    VkPipelineLayoutCreateInfo plci = {
        .sType = VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO,
        .setLayoutCount = 1,
        .pSetLayouts = &layout,
    };
    CHECK(vkCreatePipelineLayout(device, &plci, NULL, &pipelayout));

    VkShaderModuleCreateInfo smci = {
        .sType = VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO,
        .codeSize = sizeof(shader_spv),
        .pCode = shader_spv,
    };
    VkShaderModule module;
    CHECK(vkCreateShaderModule(device, &smci, NULL, &module));

    VkComputePipelineCreateInfo cpi = {
        .sType = VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO,
        .stage = { .sType = VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO,
                   .stage = VK_SHADER_STAGE_COMPUTE_BIT,
                   .module = module,
                   .pName = "main" },
        .layout = pipelayout,
    };
    CHECK(vkCreateComputePipelines(device, VK_NULL_HANDLE, 1, &cpi, NULL,
                                   &pipeline));

    // Loop with the counter in (restored) stack memory: a restore rewinds
    // execution to the slot-0 checkpoint continuation, so the loop body
    // re-runs from there with the checkpoint-time counter.
    for (volatile int i = 0;; i++) {
        if (i == 1) {
            run_copy("i1: baseline (live)");
            valo_checkpoint_slot(0);
        }
        if (i == 2) {
            // Runs live after the first save AND again after every
            // restore: the post-restore save slot1 is the trimmed-log one.
            run_copy("i2: live after save slot0 / save slot1");
            valo_checkpoint_slot(1);
        }
        if (i == 3) {
            run_copy("i3: after restore slot0 (the red check)");
            valo_restore_slot(0);
        }
    }
}
