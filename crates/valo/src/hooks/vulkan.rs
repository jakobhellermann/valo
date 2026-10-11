//! Vulkan hooks.

use std::ffi::{CStr, c_char, c_void};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::determinism::skip_draw;
use crate::{info, override_sym};
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
        _other => result,
    }
});

override_sym!("system" vkQueuePresentKHR(queue: VkQueue, present_info: &VkPresentInfoKHR) -> VkResult, lib: LIB, |real| {
    skip_draw::capture_queue(queue);
    if skip_draw::active() {
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
