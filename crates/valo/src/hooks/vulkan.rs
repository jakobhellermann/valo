// TODO(ai-review): generated, not yet audited
//! Minimal Vulkan frame-boundary hooks.
//!
//! Vulkan entry points never resolve through the PLT or the global symbol
//! scope: the game fetches `vkGetInstanceProcAddr` once (Hollow Knight's
//! UnityPlayer `dlopen`s `libvulkan.so.1` and `dlsym`s the entry point
//! itself) and then walks the proc-addr chain. So interposition happens in
//! two places:
//!
//! 1. the `dlsym` hook (`dlhook.rs`) hands out our `vkGetInstanceProcAddr`
//!    for handle-scoped lookups into the loader, unconditionally by name
//!    (libTAS parity: the loader also dlsym()s this name from layer/ICD
//!    libs, whose instance chains get silently flattened — no recursion,
//!    layer gipas are only consulted once during chain building and our
//!    answers are chain-top pointers), and
//! 2. inside the proc-addr chain we substitute our `vkQueuePresentKHR` (the
//!    only presentation-driven frame boundary) and forward everything else.
//!
//! `vkAcquireNextImageKHR`/`vkAcquireNextImage2KHR` are hooked the same way,
//! but for a different reason: the first acquire after each restore must
//! return `VK_ERROR_OUT_OF_DATE_KHR`. The presentation engine (X server /
//! Wayland compositor) is external state a restore cannot rewind:
//! present-idle events (X11) and explicit-sync release points (Wayland)
//! were consumed in the discarded timeline, so a restored swapchain would
//! deadlock in acquire, waiting for events that never re-arrive. OUT_OF_DATE
//! is the spec-blessed way to tell the application "presentation state
//! diverged, recreate the swapchain" — the same path every application
//! takes on window resize, and it leaves acquire semantics intact (no image
//! index written, no semaphore or fence signaled).
//!
//! The one-shot trigger compares the *not-snapshotted* restore counter
//! (`ReservedMemory::restore_count`) against a module static that *is*
//! snapshot/rewound: after a restore, the static holds the pre-restore value
//! while the counter advanced, so exactly one acquire per restore
//! invalidates. The rewind of the static is what makes it self-arming again
//! for multi-cycle restores — no per-swapchain bookkeeping needed.
//!
//! The real functions are resolved lazily by `override_sym!`: `RTLD_NEXT`
//! misses the `RTLD_LOCAL`-loaded loader, so the `lib:` fallback looks the
//! symbol up in the game's already loaded libvulkan. The dlsym'd present
//! trampoline dispatches through the passed object's dispatch table, so
//! device chains built by layers (e.g. MangoHud's overlay) stay in the call
//! path.
//!
//! Dispatchable handles (`VkInstance`, `VkDevice`, `VkQueue`) are ABI-stable
//! opaque pointers; struct layouts and result codes come from the
//! `vulkan-headers` crate — a direct `vulkan.h` translation, no loader, no
//! prototypes.
//!
//! Translated from libTAS
//! (`src/library/rendering/vulkanwrappers.cpp`, `vulkanloader.cpp`).

use std::ffi::{CStr, c_char, c_void};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::{info, override_sym};
use vulkan_headers::vulkan::vulkan::*;

const LIB: *const c_char = c"libvulkan.so.1".as_ptr();

static SWAPCHAIN_INVALIDATE_PENDING: AtomicBool = AtomicBool::new(false);

// Let the next swapchain acquire answer OUT_OF_DATE
pub fn queue_outofdate_swapchain() {
    SWAPCHAIN_INVALIDATE_PENDING.store(true, Ordering::SeqCst);
}

fn acquire_next_image(real: impl FnOnce() -> VkResult) -> VkResult {
    if SWAPCHAIN_INVALIDATE_PENDING.swap(false, Ordering::SeqCst) {
        info!(
            Hooks,
            "restore invalidated presentation state: acquire -> OUT_OF_DATE"
        );
        return VK_ERROR_OUT_OF_DATE_KHR;
    }
    real()
}

override_sym!("system" vkGetInstanceProcAddr(instance: VkInstance, name: *const c_char) -> *const c_void,
    lib: LIB, |real| {
    let result = real(instance, name);
    if name.is_null() {
        return result;
    }
    // SAFETY: `name` is a NUL-terminated C string per the Vulkan spec.
    let requested = unsafe { CStr::from_ptr(name) };
    match requested.to_bytes() {
        b"vkGetDeviceProcAddr" => vkGetDeviceProcAddr as *const c_void,
        b"vkQueuePresentKHR" => vkQueuePresentKHR as *const c_void,
        b"vkAcquireNextImageKHR" => vkAcquireNextImageKHR as *const c_void,
        b"vkAcquireNextImage2KHR" => vkAcquireNextImage2KHR as *const c_void,
        // TODO: debug for  post-restore swapchain recreate failure.
        b"vkGetPhysicalDeviceSurfaceCapabilitiesKHR" => vkGetPhysicalDeviceSurfaceCapabilitiesKHR as *const c_void,
        b"vkDestroySwapchainKHR" => vkDestroySwapchainKHR as *const c_void,
        _other => result,
    }
});

override_sym!("system" vkGetDeviceProcAddr(device: VkDevice, name: *const c_char) -> *const c_void,
    lib: LIB, |real| {
    let result = real(device, name);
    if name.is_null() {
        return result;
    }
    // SAFETY: `name` is a NUL-terminated C string per the Vulkan spec.
    let requested = unsafe { CStr::from_ptr(name) };
    match requested.to_bytes() {
        b"vkQueuePresentKHR" =>  vkQueuePresentKHR as *const c_void,
        b"vkAcquireNextImageKHR" => vkAcquireNextImageKHR as *const c_void,
        b"vkAcquireNextImage2KHR" => vkAcquireNextImage2KHR as *const c_void,
        b"vkGetPhysicalDeviceSurfaceCapabilitiesKHR" => vkGetPhysicalDeviceSurfaceCapabilitiesKHR as *const c_void,
        // The destroy arms the kick budget for the swapchain-destroy joins.
        b"vkDestroySwapchainKHR" => vkDestroySwapchainKHR as *const c_void,
        _other => result,
    }
});

override_sym!("system" vkQueuePresentKHR(queue: VkQueue, present_info: *const VkPresentInfoKHR) -> VkResult, lib: LIB, |real| {
    crate::frame::frame_boundary(move || real(queue, present_info))
});

override_sym!("system" vkGetPhysicalDeviceSurfaceCapabilitiesKHR( physical_device: VkPhysicalDevice, surface: VkSurfaceKHR, p_capabilities: *mut VkSurfaceCapabilitiesKHR ) -> VkResult, lib: LIB, |real| {
    let result = real(physical_device, surface, p_capabilities);
    if result != 0 {
        crate::hooks::xlib::xfixup::log_counters("caps-query fail");
    }
    result
});

override_sym!("system" vkAcquireNextImageKHR( device: VkDevice, swapchain: VkSwapchainKHR, timeout: u64, semaphore: VkSemaphore, fence: VkFence, p_image_index: *mut u32 ) -> VkResult, lib: LIB, |real| {
    acquire_next_image(move || real(device, swapchain, timeout, semaphore, fence, p_image_index))
});

override_sym!("system" vkAcquireNextImage2KHR( device: VkDevice, p_acquire_info: *const VkAcquireNextImageInfoKHR, p_image_index: *mut u32 ) -> VkResult, lib: LIB, |real| {
    acquire_next_image(move || real(device, p_acquire_info, p_image_index))
});

override_sym!("system" vkDestroySwapchainKHR(device: VkDevice, swapchain: VkSwapchainKHR, allocator: *const VkAllocationCallbacks) -> (), lib: LIB, |real| {
    // Every destroy path (resize-driven recreate, OUT_OF_DATE recreate,
    // exit) joins the swapchain's event manager, which may be parked waiting
    // for Present events whose originals the discarded timeline consumed.
    // Arm the kicks: the join hook injects once the destroy's invalidated
    // status is in place.
    crate::hooks::xcb::schedule_present_kicks(3);
    real(device, swapchain, allocator)
});
