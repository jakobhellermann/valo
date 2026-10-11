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
//! 2. inside the proc-addr chain we substitute the frame-boundary hooks
//!    (`vkQueuePresentKHR`, the acquire pair), the swapchain hooks for the
//!    present-mode selection (`vkGetPhysicalDeviceSurfacePresentModesKHR`,
//!    `vkCreateSwapchainKHR`) and forward everything else.
//!
//! `vkAcquireNextImageKHR`/`vkAcquireNextImage2KHR` are hooked the same way,
//! but for a different reason: the first acquire — or, when a restore
//! rewinds into a frame whose present is still pending, the first present —
//! after each restore must return `VK_ERROR_OUT_OF_DATE_KHR`. The
//! presentation engine (X server / Wayland compositor) is external state
//! a restore cannot rewind: present-idle events (X11) and explicit-sync
//! release points (Wayland) were consumed in the discarded timeline, and
//! the swapchain's buffers may have died there too, so a restored
//! swapchain would deadlock in acquire, waiting for events that never
//! re-arrive. OUT_OF_DATE is the spec-blessed way to tell the application
//! "presentation state diverged, recreate the swapchain" — the same path
//! every application takes on window resize, and it leaves acquire
//! semantics intact (no image index written, no semaphore or fence
//! signaled).
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
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use crate::determinism::skip_draw;
use crate::{info, override_sym, present};
use vulkan_headers::vulkan::vulkan::*;

const LIB: *const c_char = c"libvulkan.so.1".as_ptr();

static SWAPCHAIN_INVALIDATE_PENDING: AtomicBool = AtomicBool::new(false);

// skip_draw (libTAS `skipping_draw` parity): while the session config
// has skip_draw set, acquire is answered with an empty-submit signal
// fake and present with an empty-submit drain — the image pipeline is
// skipped, the frame boundary still fires. State, layout tables and the
// submit fakes live in `determinism::skip_draw`.

// Let the next swapchain acquire or present answer OUT_OF_DATE
pub fn queue_outofdate_swapchain() {
    SWAPCHAIN_INVALIDATE_PENDING.store(true, Ordering::SeqCst);
}

/// Consumes the restore's one-shot presentation invalidation.
fn needs_swapchain_invalidation() -> bool {
    SWAPCHAIN_INVALIDATE_PENDING.swap(false, Ordering::SeqCst)
}

fn acquire_next_image(real: impl FnOnce() -> VkResult) -> VkResult {
    if needs_swapchain_invalidation() {
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
        b"vkQueueSubmit" => {
            // Capture the real submit for the skip_draw empty submits.
            skip_draw::capture_submit(result);
            result
        }
        b"vkQueuePresentKHR" => vkQueuePresentKHR as *const c_void,
        b"vkAcquireNextImageKHR" => vkAcquireNextImageKHR as *const c_void,
        b"vkAcquireNextImage2KHR" => vkAcquireNextImage2KHR as *const c_void,
        // TODO: debug for  post-restore swapchain recreate failure.
        b"vkGetPhysicalDeviceSurfaceCapabilitiesKHR" => vkGetPhysicalDeviceSurfaceCapabilitiesKHR as *const c_void,
        b"vkGetPhysicalDeviceSurfacePresentModesKHR" => vkGetPhysicalDeviceSurfacePresentModesKHR as *const c_void,
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
        b"vkQueueSubmit" => {
            // Capture the real submit for the skip_draw empty submits.
            skip_draw::capture_submit(result);
            result
        }
        b"vkQueuePresentKHR" =>  vkQueuePresentKHR as *const c_void,
        b"vkAcquireNextImageKHR" => vkAcquireNextImageKHR as *const c_void,
        b"vkAcquireNextImage2KHR" => vkAcquireNextImage2KHR as *const c_void,
        b"vkCreateSwapchainKHR" => vkCreateSwapchainKHR as *const c_void,
        b"vkGetPhysicalDeviceSurfaceCapabilitiesKHR" => vkGetPhysicalDeviceSurfaceCapabilitiesKHR as *const c_void,
        // The destroy arms the kick budget for the swapchain-destroy joins.
        b"vkDestroySwapchainKHR" => vkDestroySwapchainKHR as *const c_void,
        _other => result,
    }
});

/// Replacement for a direct `dlsym` of the loader's `vkGetDeviceProcAddr`,
/// which drivers layered on Vulkan (zink) use instead of the
/// `vkGetInstanceProcAddr` chain. Their presents run inside the GL frame
/// boundary, partly on driver threads, so present stays unhooked; only the
/// restore invalidation on acquire and the kick arming on swapchain destroy
/// are substituted. `None` for lookups into other libraries (layers), which
/// pass through.
pub(crate) fn dlsym_device_proc_addr(resolved: *mut c_void) -> Option<*mut c_void> {
    let loader = crate::hooks::link_function(
        &LOADER_DEVICE_PROC_ADDR,
        c"vkGetDeviceProcAddr".as_ptr(),
        LIB,
    )?;
    (resolved == loader).then_some(driver_get_device_proc_addr as *mut c_void)
}

static LOADER_DEVICE_PROC_ADDR: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

extern "system" fn driver_get_device_proc_addr(
    device: VkDevice,
    name: *const c_char,
) -> *const c_void {
    let loader = LOADER_DEVICE_PROC_ADDR.load(Ordering::SeqCst);
    assert!(
        !loader.is_null(),
        "driver vkGetDeviceProcAddr handed out before the loader's was resolved"
    );
    // SAFETY: `loader` is the loader's exported `vkGetDeviceProcAddr`
    // (checked in `dlsym_device_proc_addr` before this function is handed
    // out), which has exactly this signature.
    let real = unsafe {
        std::mem::transmute::<
            *mut c_void,
            extern "system" fn(VkDevice, *const c_char) -> *const c_void,
        >(loader)
    };
    let result = real(device, name);
    if name.is_null() || result.is_null() {
        return result;
    }
    // SAFETY: `name` is a NUL-terminated C string per the Vulkan spec.
    match unsafe { CStr::from_ptr(name) }.to_bytes() {
        b"vkAcquireNextImageKHR" => driver_acquire_next_image as *const c_void,
        b"vkDestroySwapchainKHR" => vkDestroySwapchainKHR as *const c_void,
        _other => result,
    }
}

static LOADER_ACQUIRE_NEXT_IMAGE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// Acquire without the skip_draw fake: the driver's presents are not
/// hooked, so a faked acquire would never see its matching faked present.
extern "system" fn driver_acquire_next_image(
    device: VkDevice,
    swapchain: VkSwapchainKHR,
    timeout: u64,
    semaphore: VkSemaphore,
    fence: VkFence,
    p_image_index: *mut u32,
) -> VkResult {
    let loader = crate::hooks::link_function(
        &LOADER_ACQUIRE_NEXT_IMAGE,
        c"vkAcquireNextImageKHR".as_ptr(),
        LIB,
    )
    .expect("driver vkAcquireNextImageKHR called without a loaded libvulkan");
    // SAFETY: `loader` is the loader's exported `vkAcquireNextImageKHR`,
    // which has exactly this signature.
    let real = unsafe {
        std::mem::transmute::<
            *mut c_void,
            extern "system" fn(
                VkDevice,
                VkSwapchainKHR,
                u64,
                VkSemaphore,
                VkFence,
                *mut u32,
            ) -> VkResult,
        >(loader)
    };
    acquire_next_image(move || real(device, swapchain, timeout, semaphore, fence, p_image_index))
}

override_sym!("system" vkGetPhysicalDeviceSurfacePresentModesKHR(physical_device: VkPhysicalDevice, surface: VkSurfaceKHR, p_count: *mut u32, p_modes: *mut VkPresentModeKHR) -> VkResult, lib: LIB, |real| {
    let res = real(physical_device, surface, p_count, p_modes);
    if res == VK_SUCCESS && !p_count.is_null() && !p_modes.is_null() {
        // SAFETY: on success the query wrote `*p_count` modes to `p_modes`.
        let modes = unsafe { std::slice::from_raw_parts(p_modes, *p_count as usize) };
        present::record_surface_modes(surface, modes);
    }
    res
});

override_sym!("system" vkCreateSwapchainKHR(device: VkDevice, create_info: *const VkSwapchainCreateInfoKHR, allocator: *const VkAllocationCallbacks, swapchain: *mut VkSwapchainKHR) -> VkResult, lib: LIB, |real| {
    // SAFETY: per the vkCreateSwapchainKHR contract, `pCreateInfo` is a
    // required (non-optional) valid pointer to a VkSwapchainCreateInfoKHR.
    let create_info = unsafe { &*create_info };

    match present::rewrite_create_info(create_info) {
        Some(rewritten) => real(device, &rewritten, allocator, swapchain),
        None => real(device, create_info, allocator, swapchain),
    }
});

override_sym!("system" vkQueuePresentKHR(queue: VkQueue, present_info: *const VkPresentInfoKHR) -> VkResult, lib: LIB, |real| {
    skip_draw::capture_queue(queue);
    // SAFETY: per the vkQueuePresentKHR contract, `pPresentInfo` is a required valid pointer to a VkPresentInfoKHR.
    let present_info = unsafe { &*present_info };
    // SAFETY: `queue` is the presenting queue; the handles come from the game's own present info.
    if let Some(result) = unsafe { skip_draw::drain_present(queue, present_info) } {
        return crate::frame::frame_boundary(move || result);
    }

    if needs_swapchain_invalidation() {
        info!(
            Hooks,
            "restore invalidated presentation state: present -> OUT_OF_DATE"
        );
        return crate::frame::frame_boundary(move || VK_ERROR_OUT_OF_DATE_KHR);
    }

    // Present for real so the semaphores resolve and the queue gets captured.
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
    // SAFETY: all handles come from the game's own acquire call; the fake
    // path only signals them via an empty submit and writes the out pointer.
    if unsafe { skip_draw::fake_acquire(semaphore, fence, p_image_index) } {
        return VK_SUCCESS;
    }
    acquire_next_image(move || real(device, swapchain, timeout, semaphore, fence, p_image_index))
});

override_sym!("system" vkAcquireNextImage2KHR( device: VkDevice, p_acquire_info: *const VkAcquireNextImageInfoKHR, p_image_index: *mut u32 ) -> VkResult, lib: LIB, |real| {
    // SAFETY: `p_acquire_info` is the game's valid acquire info; reading
    // its semaphore/fence fields is in-bounds.
    let acquire_info = unsafe { &*p_acquire_info };
    // SAFETY: handles read from the game's own acquire info; the fake path
    // only signals them via an empty submit and writes the out pointer.
    if unsafe { skip_draw::fake_acquire(acquire_info.semaphore, acquire_info.fence, p_image_index) } {
        return VK_SUCCESS;
    }
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
