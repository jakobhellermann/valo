//! Vulkan hooks.
//! `vkQueuePresentKHR` acts as a frame boundary.

// Inspired by https://github.com/clementgallet/libTAS/blob/master/src/library/rendering/vulkanwrappers.cpp, vulkanloader.cpp
// The functions are `override_sym` for convenience, the symbol resolution happens through dlsym, not LD_PRELOAD interposition

use std::ffi::{CStr, c_char, c_int, c_void};

use crate::override_sym;

const LIB: *const c_char = c"libvulkan.so.1".as_ptr();

override_sym!(vkGetInstanceProcAddr(instance: *mut c_void, name: *const c_char) -> *const c_void, lib: LIB, |real| {
    let result = real(instance, name);
    if name.is_null() {
        return result;
    }
    // SAFETY: `name` is a NUL-terminated C string per the Vulkan spec.
    let requested = unsafe { CStr::from_ptr(name) };
    match requested.to_bytes() {
        b"vkGetDeviceProcAddr" => vkGetDeviceProcAddr as *const c_void,
        b"vkQueuePresentKHR" => vkQueuePresentKHR as *const c_void,
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
        _other => result,
    }
});

override_sym!(vkQueuePresentKHR(queue: *mut c_void, present_info: *const c_void) -> c_int, lib: LIB, |real| {
    crate::frame::frame_boundary(move || real(queue, present_info))
});
