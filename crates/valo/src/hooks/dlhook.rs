use std::ffi::{CStr, c_char, c_void};
use std::sync::LazyLock;

use crate::trace;

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
            b"SteamAPI_RestartAppIfNecessary" => {
                Some(crate::hooks::steam::SteamAPI_RestartAppIfNecessary as *mut c_void)
            }
            b"SteamAPI_Init" => Some(crate::hooks::steam::SteamAPI_Init as *mut c_void),
            b"SteamAPI_InitSafe" => Some(crate::hooks::steam::SteamAPI_InitSafe as *mut c_void),
            b"SteamAPI_IsSteamRunning" => {
                Some(crate::hooks::steam::SteamAPI_IsSteamRunning as *mut c_void)
            }
            b"SteamAPI_Shutdown" => Some(crate::hooks::steam::SteamAPI_Shutdown as *mut c_void),
            b"XOpenDisplay" => Some(crate::hooks::xlib::xdisplay::XOpenDisplay as *mut c_void),
            b"xcb_connect" => Some(crate::hooks::xcb::xcb_connect as *mut c_void),
            b"xcb_connect_to_fd" => Some(crate::hooks::xcb::xcb_connect_to_fd as *mut c_void),
            b"xcb_disconnect" => Some(crate::hooks::xcb::xcb_disconnect as *mut c_void),
            b"xcb_poll_for_event" => Some(crate::hooks::xcb::xcb_poll_for_event as *mut c_void),
            b"xcb_wait_for_event" => Some(crate::hooks::xcb::xcb_wait_for_event as *mut c_void),
            b"xcb_flush" => Some(crate::hooks::xcb::xcb_flush as *mut c_void),
            b"xcb_poll_for_special_event" => {
                Some(crate::hooks::xcb::xcb_poll_for_special_event as *mut c_void)
            }
            b"xcb_wait_for_special_event" => {
                Some(crate::hooks::xcb::xcb_wait_for_special_event as *mut c_void)
            }
            b"xcb_register_for_special_xge" => {
                Some(crate::hooks::xcb::xcb_register_for_special_xge as *mut c_void)
            }
            b"xcb_unregister_for_special_event" => {
                Some(crate::hooks::xcb::xcb_unregister_for_special_event as *mut c_void)
            }
            b"xcb_present_select_input" => {
                Some(crate::hooks::xcb::xcb_present_select_input as *mut c_void)
            }
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
