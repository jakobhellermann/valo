//! Vulkan hooks.

use std::ffi::{CStr, c_char, c_void};
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use ash::vk::Handle;

use crate::determinism::skip_draw;
use crate::{info, overlay, override_sym, present};
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
        b"vkCreateDevice" => vkCreateDevice as *const c_void,
        b"vkDestroyDevice" => vkDestroyDevice as *const c_void,
        b"vkGetDeviceQueue" => vkGetDeviceQueue as *const c_void,
        b"vkGetDeviceQueue2" => vkGetDeviceQueue2 as *const c_void,
        b"vkCreateSwapchainKHR" => vkCreateSwapchainKHR as *const c_void,
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
        b"vkDestroyDevice" => vkDestroyDevice as *const c_void,
        b"vkGetDeviceQueue" => vkGetDeviceQueue as *const c_void,
        b"vkGetDeviceQueue2" => vkGetDeviceQueue2 as *const c_void,
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
/// boundary, partly on driver threads, so present is no boundary here; the
/// restore invalidation on acquire, the kick arming on swapchain destroy and
/// the overlay's bookkeeping and drawing are substituted. `None` for lookups
/// into other libraries (layers), which pass through.
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
        b"vkCreateSwapchainKHR" => driver_create_swapchain as *const c_void,
        b"vkQueuePresentKHR" => driver_queue_present as *const c_void,
        b"vkGetDeviceQueue" => vkGetDeviceQueue as *const c_void,
        b"vkGetDeviceQueue2" => vkGetDeviceQueue2 as *const c_void,
        b"vkDestroyDevice" => vkDestroyDevice as *const c_void,
        _other => result,
    }
}

static LOADER_CREATE_SWAPCHAIN: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static LOADER_QUEUE_PRESENT: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// Swapchain create for the overlay only, without the present-mode rewrite of the game's own.
extern "system" fn driver_create_swapchain(
    device: VkDevice,
    create_info: &VkSwapchainCreateInfoKHR,
    allocator: *const VkAllocationCallbacks,
    swapchain: &mut VkSwapchainKHR,
) -> VkResult {
    let loader = crate::hooks::link_function(
        &LOADER_CREATE_SWAPCHAIN,
        c"vkCreateSwapchainKHR".as_ptr(),
        LIB,
    )
    .expect("driver vkCreateSwapchainKHR called without a loaded libvulkan");
    // SAFETY: `loader` is the loader's exported `vkCreateSwapchainKHR`, which has this signature.
    let real = unsafe {
        std::mem::transmute::<
            *mut c_void,
            extern "system" fn(
                VkDevice,
                &VkSwapchainCreateInfoKHR,
                *const VkAllocationCallbacks,
                &mut VkSwapchainKHR,
            ) -> VkResult,
        >(loader)
    };
    create_swapchain_with_overlay(device, *create_info, allocator, swapchain, |d, i, a, s| {
        real(d, i, a, s)
    })
}

/// Present for the overlay only: the GL swap around it is the frame boundary.
extern "system" fn driver_queue_present(
    queue: VkQueue,
    present_info: &VkPresentInfoKHR,
) -> VkResult {
    let loader =
        crate::hooks::link_function(&LOADER_QUEUE_PRESENT, c"vkQueuePresentKHR".as_ptr(), LIB)
            .expect("driver vkQueuePresentKHR called without a loaded libvulkan");
    // SAFETY: `loader` is the loader's exported `vkQueuePresentKHR`, which has this signature.
    let real = unsafe {
        std::mem::transmute::<*mut c_void, extern "system" fn(VkQueue, &VkPresentInfoKHR) -> VkResult>(
            loader,
        )
    };
    present_with_overlay(queue, present_info, |info| real(queue, info))
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

override_sym!("system" vkCreateDevice(physical_device: VkPhysicalDevice, create_info: *const VkDeviceCreateInfo, allocator: *const VkAllocationCallbacks, device: &mut VkDevice) -> VkResult, lib: LIB, |real| {
    let result = real(physical_device, create_info, allocator, device);
    if result == VK_SUCCESS {
        overlay::on_create_device(
            ash::vk::PhysicalDevice::from_raw(physical_device as u64),
            ash::vk::Device::from_raw(*device as u64),
        );
    }
    result
});

override_sym!("system" vkDestroyDevice(device: VkDevice, allocator: *const VkAllocationCallbacks) -> (), lib: LIB, |real| {
    overlay::on_destroy_device(ash::vk::Device::from_raw(device as u64));
    real(device, allocator);
});

override_sym!("system" vkGetDeviceQueue(device: VkDevice, family: u32, index: u32, queue: &mut VkQueue) -> (), lib: LIB, |real| {
    real(device, family, index, queue);
    overlay::on_get_device_queue(
        ash::vk::Device::from_raw(device as u64),
        family,
        ash::vk::Queue::from_raw(*queue as u64),
    );
});

override_sym!("system" vkGetDeviceQueue2(device: VkDevice, info: &VkDeviceQueueInfo2, queue: &mut VkQueue) -> (), lib: LIB, |real| {
    real(device, info, queue);
    overlay::on_get_device_queue(
        ash::vk::Device::from_raw(device as u64),
        info.queueFamilyIndex,
        ash::vk::Queue::from_raw(*queue as u64),
    );
});

override_sym!("system" vkCreateSwapchainKHR(device: VkDevice, create_info: &VkSwapchainCreateInfoKHR, allocator: *const VkAllocationCallbacks, swapchain: &mut VkSwapchainKHR) -> VkResult, lib: LIB, |real| {
    let info = present::rewrite_create_info(create_info).unwrap_or(*create_info);
    create_swapchain_with_overlay(device, info, allocator, swapchain, |d, i, a, s| real(d, i, a, s))
});

/// Creates the swapchain with images the overlay can draw into, and registers it.
fn create_swapchain_with_overlay(
    device: VkDevice,
    mut info: VkSwapchainCreateInfoKHR,
    allocator: *const VkAllocationCallbacks,
    swapchain: &mut VkSwapchainKHR,
    create: impl FnOnce(
        VkDevice,
        &VkSwapchainCreateInfoKHR,
        *const VkAllocationCallbacks,
        &mut VkSwapchainKHR,
    ) -> VkResult,
) -> VkResult {
    let ash_device = ash::vk::Device::from_raw(device as u64);
    info.imageUsage = overlay::swapchain_usage(
        ash_device,
        ash::vk::SurfaceKHR::from_raw(info.surface as u64),
        ash::vk::ImageUsageFlags::from_raw(info.imageUsage),
    )
    .as_raw();
    let result = create(device, &info, allocator, swapchain);
    if result == VK_SUCCESS {
        // SAFETY: VkSwapchainCreateInfoKHR has the same C layout in both bindings.
        let ash_info = unsafe { &*(&raw const info).cast::<ash::vk::SwapchainCreateInfoKHR>() };
        overlay::on_create_swapchain(
            ash_device,
            ash::vk::SwapchainKHR::from_raw(*swapchain as u64),
            ash_info,
        );
    }
    result
}

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
    crate::frame::frame_boundary(move || {
        overlay::queue_frame();
        present_with_overlay(queue, present_info, |info| real(queue, info))
    })
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
    overlay::on_destroy_swapchain(ash::vk::SwapchainKHR::from_raw(swapchain as u64));
    real(device, swapchain, allocator)
});

/// Presents through `present` after drawing the overlay into the image, waiting on the overlay's
/// draw instead of the game's semaphores, which the draw waits on in turn.
fn present_with_overlay(
    queue: VkQueue,
    info: &VkPresentInfoKHR,
    present: impl FnOnce(&VkPresentInfoKHR) -> VkResult,
) -> VkResult {
    if info.swapchainCount != 1 {
        return present(info);
    }
    let wait: Vec<ash::vk::Semaphore> = match info.waitSemaphoreCount {
        0 => Vec::new(),
        // SAFETY: VkPresentInfoKHR VU: a nonzero `waitSemaphoreCount` requires
        // `pWaitSemaphores` to point to that many handles.
        count => unsafe { std::slice::from_raw_parts(info.pWaitSemaphores, count as usize) }
            .iter()
            .map(|&s| ash::vk::Semaphore::from_raw(s as u64))
            .collect(),
    };
    // SAFETY: one swapchain, so `pSwapchains` and `pImageIndices` hold one entry each.
    let (swapchain, image_index) = unsafe { (*info.pSwapchains, *info.pImageIndices) };
    let Some(drawn) = overlay::before_present(
        ash::vk::Queue::from_raw(queue as u64),
        ash::vk::SwapchainKHR::from_raw(swapchain as u64),
        image_index,
        &wait,
    ) else {
        return present(info);
    };
    let drawn = drawn.as_raw() as VkSemaphore;
    let mut with_overlay = *info;
    with_overlay.waitSemaphoreCount = 1;
    with_overlay.pWaitSemaphores = &drawn;
    present(&with_overlay)
}
