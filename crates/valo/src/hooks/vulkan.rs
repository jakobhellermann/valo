//! Vulkan hooks.

use std::ffi::{CStr, c_char, c_void};
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use crate::determinism::skip_draw;
use crate::{info, override_sym, present};
use vulkan_headers::vulkan::vulkan::*;

const LIB: *const c_char = c"libvulkan.so.1".as_ptr();

static SWAPCHAIN_INVALIDATE_PENDING: AtomicBool = AtomicBool::new(false);

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

/// Replacement for a `dlsym` of `vkGetInstanceProcAddr`. `None` for lookups into
/// other libraries: the loader resolves each layer's own entry point this way, and
/// handing it the hook would make the hook dispatch back into itself.
pub(crate) fn dlsym_instance_proc_addr(resolved: *mut c_void) -> Option<*mut c_void> {
    let loader = crate::hooks::link_function(
        &LOADER_INSTANCE_PROC_ADDR,
        c"vkGetInstanceProcAddr".as_ptr(),
        LIB,
    )?;
    (resolved == loader).then_some(vkGetInstanceProcAddr as *mut c_void)
}

static LOADER_INSTANCE_PROC_ADDR: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

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

override_sym!("system" vkGetPhysicalDeviceSurfacePresentModesKHR(physical_device: VkPhysicalDevice, surface: VkSurfaceKHR, p_count: &mut u32, p_modes: *mut VkPresentModeKHR) -> VkResult, lib: LIB, |real| {
    let res = real(physical_device, surface, p_count, p_modes);
    if res == VK_SUCCESS && !p_modes.is_null() {
        // SAFETY: on success with non-null `pPresentModes`, the query wrote `*p_count` modes there.
        let modes = unsafe { std::slice::from_raw_parts(p_modes, *p_count as usize) };
        present::record_surface_modes(surface, modes);
    }
    res
});

override_sym!("system" vkCreateSwapchainKHR(device: VkDevice, create_info: &VkSwapchainCreateInfoKHR, allocator: *const VkAllocationCallbacks, swapchain: &mut VkSwapchainKHR) -> VkResult, lib: LIB, |real| {
    match present::rewrite_create_info(create_info) {
        Some(rewritten) => real(device, &rewritten, allocator, swapchain),
        None => real(device, create_info, allocator, swapchain),
    }
});

override_sym!("system" vkQueuePresentKHR(queue: VkQueue, present_info: &VkPresentInfoKHR) -> VkResult, lib: LIB, |real| {
    skip_draw::capture_queue(queue);
    let wait_semaphores = match present_info.waitSemaphoreCount {
        0 => &[][..],
        // SAFETY: VkPresentInfoKHR VU: a nonzero `waitSemaphoreCount` requires
        // `pWaitSemaphores` to point to that many handles.
        count => unsafe { std::slice::from_raw_parts(present_info.pWaitSemaphores, count as usize) },
    };
    // SAFETY: `queue` and `wait_semaphores` are valid per the VUs of this present.
    if let Some(result) = unsafe { skip_draw::drain_present(queue, wait_semaphores) } {
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

override_sym!("system" vkGetPhysicalDeviceSurfaceCapabilitiesKHR( physical_device: VkPhysicalDevice, surface: VkSurfaceKHR, p_capabilities: &mut VkSurfaceCapabilitiesKHR ) -> VkResult, lib: LIB, |real| {
    let result = real(physical_device, surface, p_capabilities);
    if result != VK_SUCCESS {
        crate::hooks::xlib::xfixup::log_counters("caps-query fail");
    }
    result
});

override_sym!("system" vkAcquireNextImageKHR( device: VkDevice, swapchain: VkSwapchainKHR, timeout: u64, semaphore: VkSemaphore, fence: VkFence, p_image_index: &mut u32 ) -> VkResult, lib: LIB, |real| {
    // SAFETY: `semaphore` and `fence` are null or valid per the VUs of this acquire.
    if unsafe { skip_draw::fake_acquire(semaphore, fence, p_image_index) } {
        return VK_SUCCESS;
    }
    acquire_next_image(move || real(device, swapchain, timeout, semaphore, fence, p_image_index))
});

override_sym!("system" vkAcquireNextImage2KHR( device: VkDevice, p_acquire_info: &VkAcquireNextImageInfoKHR, p_image_index: &mut u32 ) -> VkResult, lib: LIB, |real| {
    // SAFETY: `semaphore` and `fence` are null or valid per the VUs of this acquire.
    if unsafe { skip_draw::fake_acquire(p_acquire_info.semaphore, p_acquire_info.fence, p_image_index) } {
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
