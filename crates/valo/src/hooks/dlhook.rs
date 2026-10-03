use std::ffi::{CStr, c_char, c_void};
use std::os::raw::c_int;
use std::sync::LazyLock;

use crate::{override_sym, trace};

override_sym!(dlopen(file: *const c_char, mode: c_int) -> *mut c_void |real| {
    let _name = match file.is_null() {
        true => <&CStr>::default(),
        false => unsafe { CStr::from_ptr(file) }
    };
    real(file, mode)
});

fn find_real_dlsym() -> unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void {
    // SAFETY: dlvsym won't recurse into our hook
    unsafe {
        let ptr = libc::dlvsym(libc::RTLD_NEXT, c"dlsym".as_ptr(), c"GLIBC_2.2.5".as_ptr());
        std::mem::transmute(ptr)
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void {
    static REAL_DLSYM: LazyLock<unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void> =
        LazyLock::new(find_real_dlsym);

    if super::is_native() {
        return unsafe { REAL_DLSYM(handle, symbol) };
    }

    // SAFETY: forwarding to the real dlsym
    let resolved = unsafe { REAL_DLSYM(handle, symbol) };
    if resolved.is_null() {
        return resolved;
    }

    if !symbol.is_null() {
        // SAFETY: symbol is a valid C string from the caller
        let name = unsafe { CStr::from_ptr(symbol) };
        trace!(Hooks, "dlsym({})", name.to_str().unwrap_or("<bad utf8>"));

        if let Some(p) = match name.to_bytes() {
            b"XOpenDisplay" => Some(crate::hooks::xlib::xdisplay::XOpenDisplay as *mut c_void),
            b"vkGetInstanceProcAddr" => {
                Some(crate::hooks::vulkan::vkGetInstanceProcAddr as *mut c_void)
            }
            b"glXGetProcAddress" => Some(crate::hooks::glx::glXGetProcAddress as *mut c_void),
            b"glXGetProcAddressARB" => Some(crate::hooks::glx::glXGetProcAddressARB as *mut c_void),
            b"glXGetProcAddressEXT" => Some(crate::hooks::glx::glXGetProcAddressEXT as *mut c_void),
            b"XCloseDisplay" => Some(crate::hooks::xlib::xdisplay::XCloseDisplay as *mut c_void),
            _ => None,
        } {
            return p;
        }
    }

    resolved
}
