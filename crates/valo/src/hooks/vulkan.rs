//! Vulkan hooks.
//! `vkQueuePresentKHR` acts as a frame boundary.

// Inspired by https://github.com/clementgallet/libTAS/blob/master/src/library/rendering/vulkanwrappers.cpp, vulkanloader.cpp
// The functions are `override_sym` for convenience, the symbol resolution happens through dlsym, not LD_PRELOAD interposition

use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::{info, override_sym};

const LIB: *const c_char = c"libvulkan.so.1".as_ptr();

const VK_ERROR_OUT_OF_DATE_KHR: c_int = -1000001004;

static SWAPCHAIN_INVALIDATE_PENDING: AtomicBool = AtomicBool::new(false);

// Let the next swapchain acquire answer OUT_OF_DATE
pub fn queue_outofdate_swapchain() {
    SWAPCHAIN_INVALIDATE_PENDING.store(true, Ordering::SeqCst);
}

fn acquire_next_image(real: impl FnOnce() -> c_int) -> c_int {
    if SWAPCHAIN_INVALIDATE_PENDING.swap(false, Ordering::SeqCst) {
        info!(
            Hooks,
            "restore invalidated presentation state: acquire -> OUT_OF_DATE"
        );
        return VK_ERROR_OUT_OF_DATE_KHR;
    }
    real()
}

override_sym!(vkGetInstanceProcAddr(instance: *mut c_void, name: *const c_char) -> *const c_void,
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
        _other => result,
    }
});

override_sym!(vkGetDeviceProcAddr(device: *mut c_void, name: *const c_char) -> *const c_void,
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
        _other => result,
    }
});

override_sym!(vkQueuePresentKHR(queue: *mut c_void, present_info: *const c_void) -> c_int, lib: LIB, |real| {
    crate::frame::frame_boundary(move || real(queue, present_info))
});

override_sym!(vkAcquireNextImageKHR( device: *mut c_void, swapchain: u64, timeout: u64, semaphore: u64, fence: u64, p_image_index: *mut u32 ) -> c_int, lib: LIB, |real| {
    acquire_next_image(move || real(device, swapchain, timeout, semaphore, fence, p_image_index))
});

override_sym!(vkAcquireNextImage2KHR( device: *mut c_void, p_acquire_info: *const c_void, p_image_index: *mut u32 ) -> c_int, lib: LIB, |real| {
    acquire_next_image(move || real(device, p_acquire_info, p_image_index))
});
