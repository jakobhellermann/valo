use std::ffi::{CStr, c_char, c_void};
use std::sync::atomic::{AtomicPtr, Ordering};

use crate::trace;

type DlsymFn = unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void;

/// Null until resolved, then the next `dlsym` definition after libvalo, of any version.
static REAL_DLSYM: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// glibc resolves `RTLD_NEXT` against the return address, so that case reaches the real `dlsym`
/// via `jmp` with the caller's frame untouched.
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void {
    core::arch::naked_asm!(
        "cmp rdi, -1", // RTLD_NEXT
        "jne {hook}",
        "mov rax, qword ptr [rip + {real}]",
        "test rax, rax",
        "jz 2f",
        "jmp rax",
        "2:",
        // entry rsp is 8 mod 16: two pushes plus 8 realign it for the call
        "push rdi",
        "push rsi",
        "sub rsp, 8",
        "call {resolve}",
        "add rsp, 8",
        "pop rsi",
        "pop rdi",
        "jmp rax",
        hook = sym dlsym_hook,
        real = sym REAL_DLSYM,
        resolve = sym resolve_real_dlsym,
    )
}

unsafe extern "C" fn dlsym_hook(handle: *mut c_void, symbol: *const c_char) -> *mut c_void {
    let real = real_dlsym();

    if super::is_native() || !crate::is_target() {
        return unsafe { real(handle, symbol) };
    }

    // SAFETY: forwarding to the real dlsym
    let resolved = unsafe { real(handle, symbol) };
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
            b"vkGetInstanceProcAddr" => crate::hooks::vulkan::dlsym_instance_proc_addr(resolved),
            b"vkGetDeviceProcAddr" => crate::hooks::vulkan::dlsym_device_proc_addr(resolved),
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

fn real_dlsym() -> DlsymFn {
    let mut ptr = REAL_DLSYM.load(Ordering::Relaxed);
    if ptr.is_null() {
        ptr = resolve_real_dlsym();
    }
    // SAFETY: non-null values of REAL_DLSYM come from `resolve_real_dlsym`, which only stores a
    // `dlsym` definition, and those have the `DlsymFn` signature.
    unsafe { std::mem::transmute::<*mut c_void, DlsymFn>(ptr) }
}

extern "C" fn resolve_real_dlsym() -> *mut c_void {
    // dlvsym never matches unversioned definitions (e.g. Steam's overlay), so it only yields
    // libc's dlsym, which then finds the next definition of any version.
    // SAFETY: both strings are nul-terminated literals; dlvsym is not interposed, so this does not
    // re-enter `dlsym`.
    let libc_dlsym =
        unsafe { libc::dlvsym(libc::RTLD_NEXT, c"dlsym".as_ptr(), c"GLIBC_2.2.5".as_ptr()) };
    assert!(!libc_dlsym.is_null(), "no dlsym@GLIBC_2.2.5 after libvalo");
    // SAFETY: a `dlsym@GLIBC_2.2.5` definition has the `DlsymFn` signature.
    let libc_dlsym = unsafe { std::mem::transmute::<*mut c_void, DlsymFn>(libc_dlsym) };
    // SAFETY: libc's dlsym with a nul-terminated literal; called from here, RTLD_NEXT resolves
    // relative to libvalo.
    let ptr = unsafe { libc_dlsym(libc::RTLD_NEXT, c"dlsym".as_ptr()) };
    assert!(!ptr.is_null(), "no dlsym after libvalo");
    REAL_DLSYM.store(ptr, Ordering::Relaxed);
    ptr
}
