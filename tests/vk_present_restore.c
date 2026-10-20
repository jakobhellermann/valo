//# link_flags: -lxcb
// A restore rewinds into a frame whose image is already acquired and whose
// present is still pending: a direct valo_checkpoint() between submit and
// present (boundary checkpoints come after the present). The presentation
// engine cannot rewind — the resumed present carries swapchain state the
// discarded timeline may have destroyed server-side — so the resumed
// vkQueuePresentKHR must answer OUT_OF_DATE, like the acquire path's
// one-shot already does.
//
// Timeline: frame 6 checkpoints mid-frame (after acquire+submit, before
// present). Frame 10 destroys and recreates the swapchain, killing the old
// X pixmaps. Frame 12 restores; the resumed frame-6 present must answer
// OUT_OF_DATE; forwarded, it would return SUCCESS while the BadPixmap
// arrives async on the X connection.
//
// Requires a Vulkan driver with X11 (DRI3/Present) support and a running X
// server, like the gpu tests require kumquat.
#include <dlfcn.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

#include <xcb/xcb.h>
#define VK_USE_PLATFORM_XCB_KHR 1
#include <vulkan/vulkan.h>

#include "testlib.h"

#define CHECKPOINT_FRAME 6
#define MARKER_FRAME 8
#define RECREATE_FRAME 10
#define RESTORE_FRAME 12
#define TOTAL_FRAMES 24
#define MARKER_PATH "/tmp/valo-vk-present-restore-marker"

static PFN_vkGetInstanceProcAddr gipa;
static PFN_vkGetDeviceProcAddr gdpa;

static PFN_vkCreateInstance gvk_create_instance;
static PFN_vkEnumeratePhysicalDevices gvk_enumerate_physical_devices;
static PFN_vkCreateDevice gvk_create_device;
static PFN_vkCreateXcbSurfaceKHR gvk_create_xcb_surface;
static PFN_vkGetPhysicalDeviceQueueFamilyProperties gvk_queue_family_props;
static PFN_vkGetPhysicalDeviceSurfaceSupportKHR gvk_surface_support;
static PFN_vkGetPhysicalDeviceSurfaceCapabilitiesKHR gvk_surface_caps;
static PFN_vkGetDeviceQueue gvk_get_device_queue;
static PFN_vkCreateSemaphore gvk_create_semaphore;
static PFN_vkCreateSwapchainKHR gvk_create_swapchain;
static PFN_vkDestroySwapchainKHR gvk_destroy_swapchain;
static PFN_vkAcquireNextImageKHR gvk_acquire;
static PFN_vkQueueSubmit gvk_submit;
static PFN_vkQueuePresentKHR gvk_present;

static VkInstance instance;
static VkPhysicalDevice pd;
static VkDevice dev;
static VkQueue queue;
static VkSurfaceKHR surface;
static VkSwapchainKHR swapchain;

// Per-frame semaphore pair: acquire signals, submit drains it and signals
// the present semaphore. Restores rewind lazily created pairs away, so
// every frame creates its own on first use.
static VkSemaphore acq_sem[TOTAL_FRAMES];
static VkSemaphore pres_sem[TOTAL_FRAMES];

static int die(const char *what, VkResult r) {
    tprintf("FAIL: %s -> %d\n", what, (int)r);
    return 1;
}

static VkResult create_swapchain(void) {
    VkSurfaceCapabilitiesKHR caps;
    VkResult r = gvk_surface_caps(pd, surface, &caps);
    if (r != VK_SUCCESS) return r;
    VkSwapchainCreateInfoKHR ci = {
        .sType = VK_STRUCTURE_TYPE_SWAPCHAIN_CREATE_INFO_KHR,
        .surface = surface,
        .minImageCount = caps.minImageCount,
        .imageFormat = VK_FORMAT_B8G8R8A8_UNORM,
        .imageColorSpace = VK_COLOR_SPACE_SRGB_NONLINEAR_KHR,
        .imageExtent = caps.currentExtent,
        .imageArrayLayers = 1,
        .imageUsage = VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT,
        .imageSharingMode = VK_SHARING_MODE_EXCLUSIVE,
        .preTransform = caps.currentTransform,
        .compositeAlpha = VK_COMPOSITE_ALPHA_OPAQUE_BIT_KHR,
        .presentMode = VK_PRESENT_MODE_FIFO_KHR,
        .clipped = VK_TRUE,
    };
    if (caps.currentExtent.width == 0xFFFFFFFFu) {
        ci.imageExtent.width = 128;
        ci.imageExtent.height = 128;
    }
    return gvk_create_swapchain(dev, &ci, NULL, &swapchain);
}

int main(void) {
    unlink(MARKER_PATH);

    // Route every call through the interposed proc-addr chain, like a game
    // that dlopens libvulkan — a linked vkQueuePresentKHR would bypass the
    // hooks entirely.
    void *vulkan = dlopen("libvulkan.so.1", RTLD_NOW);
    if (!vulkan) {
        tprintf("FAIL: dlopen libvulkan: %s\n", dlerror());
        return 1;
    }
    gipa = (PFN_vkGetInstanceProcAddr)dlsym(vulkan, "vkGetInstanceProcAddr");
    if (!gipa) {
        tprintf("FAIL: dlsym vkGetInstanceProcAddr\n");
        return 1;
    }

    xcb_connection_t *conn = xcb_connect(NULL, NULL);
    if (xcb_connection_has_error(conn)) {
        tprintf("FAIL: xcb_connect (is there an X server?)\n");
        return 1;
    }
    xcb_screen_iterator_t screens = xcb_setup_roots_iterator(xcb_get_setup(conn));
    if (screens.rem < 1) {
        tprintf("FAIL: no X screen\n");
        return 1;
    }
    xcb_screen_t *screen = screens.data;
    xcb_window_t window = xcb_generate_id(conn);
    // Unmanaged: a window manager reconfiguring the fresh window would make
    // presents return OUT_OF_DATE at frames that depend on the desktop.
    const uint32_t override_redirect = 1;
    xcb_create_window(conn, screen->root_depth, window, screen->root, 0, 0, 128, 128, 0,
                      XCB_WINDOW_CLASS_INPUT_OUTPUT, screen->root_visual,
                      XCB_CW_OVERRIDE_REDIRECT, &override_redirect);
    xcb_map_window(conn, window);
    xcb_flush(conn);

    const char *instance_extensions[] = {
        VK_KHR_SURFACE_EXTENSION_NAME,
        VK_KHR_XCB_SURFACE_EXTENSION_NAME,
    };
    VkApplicationInfo app = {
        .sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
        .pApplicationName = "valo-vk-present-restore",
        .apiVersion = VK_API_VERSION_1_0,
    };
    VkInstanceCreateInfo ici = {
        .sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
        .pApplicationInfo = &app,
        .enabledExtensionCount = 2,
        .ppEnabledExtensionNames = instance_extensions,
    };
    gvk_create_instance = (PFN_vkCreateInstance)gipa(VK_NULL_HANDLE, "vkCreateInstance");
    if (!gvk_create_instance) return die("vkCreateInstance", VK_ERROR_INITIALIZATION_FAILED);
    VkResult r = gvk_create_instance(&ici, NULL, &instance);
    if (r != VK_SUCCESS) return die("vkCreateInstance", r);

    gvk_enumerate_physical_devices = (PFN_vkEnumeratePhysicalDevices)gipa(instance, "vkEnumeratePhysicalDevices");
    gvk_create_xcb_surface = (PFN_vkCreateXcbSurfaceKHR)gipa(instance, "vkCreateXcbSurfaceKHR");
    gvk_queue_family_props = (PFN_vkGetPhysicalDeviceQueueFamilyProperties)gipa(instance, "vkGetPhysicalDeviceQueueFamilyProperties");
    gvk_surface_support = (PFN_vkGetPhysicalDeviceSurfaceSupportKHR)gipa(instance, "vkGetPhysicalDeviceSurfaceSupportKHR");
    gvk_surface_caps = (PFN_vkGetPhysicalDeviceSurfaceCapabilitiesKHR)gipa(instance, "vkGetPhysicalDeviceSurfaceCapabilitiesKHR");
    gvk_create_device = (PFN_vkCreateDevice)gipa(instance, "vkCreateDevice");
    gvk_get_device_queue = (PFN_vkGetDeviceQueue)gipa(instance, "vkGetDeviceQueue");
    gvk_create_semaphore = (PFN_vkCreateSemaphore)gipa(instance, "vkCreateSemaphore");
    gvk_destroy_swapchain = (PFN_vkDestroySwapchainKHR)gipa(instance, "vkDestroySwapchainKHR");
    if (!gvk_enumerate_physical_devices || !gvk_create_xcb_surface || !gvk_queue_family_props ||
        !gvk_surface_support || !gvk_surface_caps || !gvk_create_device ||
        !gvk_get_device_queue || !gvk_create_semaphore || !gvk_destroy_swapchain) {
        tprintf("FAIL: instance proc-addr missing\n");
        return 1;
    }

    uint32_t pd_count = 1;
    r = gvk_enumerate_physical_devices(instance, &pd_count, &pd);
    if (r != VK_SUCCESS || pd_count < 1) return die("vkEnumeratePhysicalDevices", r);

    VkXcbSurfaceCreateInfoKHR sci = {
        .sType = VK_STRUCTURE_TYPE_XCB_SURFACE_CREATE_INFO_KHR,
        .connection = conn,
        .window = window,
    };
    r = gvk_create_xcb_surface(instance, &sci, NULL, &surface);
    if (r != VK_SUCCESS) return die("vkCreateXcbSurfaceKHR", r);

    uint32_t family_count = 0;
    gvk_queue_family_props(pd, &family_count, NULL);
    if (family_count == 0) return die("no queue families", VK_ERROR_INITIALIZATION_FAILED);
    uint32_t queue_family = 0xFFFFFFFFu;
    for (uint32_t i = 0; i < family_count; i++) {
        VkBool32 supported = VK_FALSE;
        if (gvk_surface_support(pd, i, surface, &supported) == VK_SUCCESS && supported) {
            queue_family = i;
            break;
        }
    }
    if (queue_family == 0xFFFFFFFFu) {
        tprintf("FAIL: no present-capable queue family\n");
        return 1;
    }

    float prio = 1.0f;
    VkDeviceQueueCreateInfo qci = {
        .sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
        .queueFamilyIndex = queue_family,
        .queueCount = 1,
        .pQueuePriorities = &prio,
    };
    const char *device_extensions[] = { VK_KHR_SWAPCHAIN_EXTENSION_NAME };
    VkDeviceCreateInfo dci = {
        .sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
        .queueCreateInfoCount = 1,
        .pQueueCreateInfos = &qci,
        .enabledExtensionCount = 1,
        .ppEnabledExtensionNames = device_extensions,
    };
    r = gvk_create_device(pd, &dci, NULL, &dev);
    if (r != VK_SUCCESS) return die("vkCreateDevice", r);

    // The device-level functions come through the interposed device chain
    // — this is where the present hook lives.
    gdpa = (PFN_vkGetDeviceProcAddr)gipa(instance, "vkGetDeviceProcAddr");
    gvk_create_swapchain = (PFN_vkCreateSwapchainKHR)gdpa(dev, "vkCreateSwapchainKHR");
    gvk_acquire = (PFN_vkAcquireNextImageKHR)gdpa(dev, "vkAcquireNextImageKHR");
    gvk_submit = (PFN_vkQueueSubmit)gdpa(dev, "vkQueueSubmit");
    gvk_present = (PFN_vkQueuePresentKHR)gdpa(dev, "vkQueuePresentKHR");
    if (!gdpa || !gvk_create_swapchain || !gvk_acquire || !gvk_submit || !gvk_present) {
        tprintf("FAIL: device proc-addr missing\n");
        return 1;
    }
    gvk_get_device_queue(dev, queue_family, 0, &queue);

    r = create_swapchain();
    if (r != VK_SUCCESS) return die("vkCreateSwapchainKHR", r);

    for (int frame = 0; frame < TOTAL_FRAMES; frame++) {
        if (acq_sem[frame] == VK_NULL_HANDLE) {
            VkSemaphoreCreateInfo semci = { .sType = VK_STRUCTURE_TYPE_SEMAPHORE_CREATE_INFO };
            if (gvk_create_semaphore(dev, &semci, NULL, &acq_sem[frame]) != VK_SUCCESS ||
                gvk_create_semaphore(dev, &semci, NULL, &pres_sem[frame]) != VK_SUCCESS) {
                return die("vkCreateSemaphore", VK_ERROR_INITIALIZATION_FAILED);
            }
        }

        uint32_t idx = 0;
        r = gvk_acquire(dev, swapchain, UINT64_MAX, acq_sem[frame], VK_NULL_HANDLE, &idx);
        if (r == VK_ERROR_OUT_OF_DATE_KHR) {
            // the desktop reconfigured the window: retry this frame on a fresh swapchain
            gvk_destroy_swapchain(dev, swapchain, NULL);
            r = create_swapchain();
            if (r != VK_SUCCESS) return die("recreate swapchain", r);
            frame--;
            continue;
        }
        if (r != VK_SUCCESS && r != VK_SUBOPTIMAL_KHR) return die("vkAcquireNextImageKHR", r);

        // Drain the acquire semaphore without rendering; the present
        // semaphore carries the wait the real present would consume.
        VkPipelineStageFlags stage = VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT;
        VkSubmitInfo si = {
            .sType = VK_STRUCTURE_TYPE_SUBMIT_INFO,
            .waitSemaphoreCount = 1,
            .pWaitSemaphores = &acq_sem[frame],
            .pWaitDstStageMask = &stage,
            .signalSemaphoreCount = 1,
            .pSignalSemaphores = &pres_sem[frame],
        };
        r = gvk_submit(queue, 1, &si, VK_NULL_HANDLE);
        if (r != VK_SUCCESS) return die("vkQueueSubmit", r);

        if (frame == CHECKPOINT_FRAME) {
            // Mid-frame checkpoint: the image is acquired, the present is
            // pending.
            tprintf("frame=%d checkpoint (present pending)\n", frame);
            valo_checkpoint();
        }

        VkPresentInfoKHR pi = {
            .sType = VK_STRUCTURE_TYPE_PRESENT_INFO_KHR,
            .waitSemaphoreCount = 1,
            .pWaitSemaphores = &pres_sem[frame],
            .swapchainCount = 1,
            .pSwapchains = &swapchain,
            .pImageIndices = &idx,
        };
        VkResult present = gvk_present(queue, &pi);
        // Sync the server after each present: its replies and events then
        // land before the next frame's reads, so lvp observes them at a
        // fixed frame instead of at the server's reply latency.
        free(xcb_get_input_focus_reply(conn, xcb_get_input_focus(conn), NULL));

        int resumed = access(MARKER_PATH, F_OK) == 0;
        if (frame == CHECKPOINT_FRAME && resumed) {
            // The restore resumed into this pending present: it must not
            // be forwarded to the presentation engine.
            if (present != VK_ERROR_OUT_OF_DATE_KHR) {
                tprintf("FAIL: present after restore returned %d, want OUT_OF_DATE\n",
                        (int)present);
                return 1;
            }
            tprintf("frame=%d present out-of-date, recreated\n", frame);
            gvk_destroy_swapchain(dev, swapchain, NULL);
            r = create_swapchain();
            if (r != VK_SUCCESS) return die("recreate swapchain", r);
            continue;
        }
        if (present == VK_ERROR_OUT_OF_DATE_KHR || present == VK_SUBOPTIMAL_KHR) {
            // the desktop reconfigured the window; the frame's milestones still run
            gvk_destroy_swapchain(dev, swapchain, NULL);
            r = create_swapchain();
            if (r != VK_SUCCESS) return die("recreate swapchain", r);
        } else if (present != VK_SUCCESS) {
            return die("vkQueuePresentKHR", present);
        }

        if (frame == MARKER_FRAME) {
            // Survives the rewind in the filesystem: the resumed pass
            // detects that it is a restore re-run.
            int fd = open(MARKER_PATH, O_CREAT | O_WRONLY, 0644);
            if (fd >= 0) close(fd);
        }
        if (frame == RECREATE_FRAME) {
            // Kill the old swapchain's X pixmaps server-side, like the
            // discarded timeline of the real bug.
            gvk_destroy_swapchain(dev, swapchain, NULL);
            r = create_swapchain();
            if (r != VK_SUCCESS) return die("recreate swapchain", r);
            tprintf("frame=%d swapchain destroyed and recreated\n", frame);
        }
        if (frame == RESTORE_FRAME) {
            tprintf("frame=%d restore\n", frame);
            valo_restore();
        }
    }

    tprintf("done\n");
    return 0;
}
