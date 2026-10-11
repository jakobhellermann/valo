#![allow(clippy::undocumented_unsafe_blocks)]

use std::cell::Cell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicPtr, Ordering};

use libc::c_char;

mod deadline;
mod dlhook;
pub(crate) mod glx;
mod poll;
mod posix;
mod pthread;
mod randomness;
mod sdl;
mod signal;
mod sleep;
mod steam;
mod time;
pub(crate) mod vulkan;
pub(crate) mod xcb;
pub(crate) mod xlib;

thread_local! {
    static IS_NATIVE: Cell<u32> = const { Cell::new(0) };
}

fn is_native() -> bool {
    IS_NATIVE.get() > 0
}
pub(crate) fn run_native<T>(f: impl FnOnce() -> T) -> T {
    IS_NATIVE.with(|cell| cell.update(|count| count + 1));
    let ret = f();
    IS_NATIVE.with(|cell| cell.update(|count| count - 1));
    ret
}

pub(crate) fn link_function(
    orig: &AtomicPtr<c_void>,
    source: *const c_char,
    library: *const c_char,
) -> Option<*mut c_void> {
    let mut real = orig.load(Ordering::SeqCst);

    if !real.is_null() {
        return Some(real);
    }

    // SAFETY: `source` is a nul-terminated C string passed in from the macro.
    let name = unsafe { std::ffi::CStr::from_ptr(source) };
    crate::debug!(
        Hooks,
        "hooking '{}'",
        name.to_str().unwrap_or("invalid utf8")
    );

    real = run_native(|| unsafe { libc::dlsym(libc::RTLD_NEXT, source) });
    if real.is_null() && !library.is_null() {
        let handle =
            run_native(|| unsafe { libc::dlopen(library, libc::RTLD_LAZY | libc::RTLD_NOLOAD) });
        if !handle.is_null() {
            real = run_native(|| unsafe { libc::dlsym(handle, source) });
            unsafe { libc::dlclose(handle) };
        }
    }
    if !real.is_null() {
        orig.store(real, Ordering::SeqCst);
        return Some(real);
    }

    None
}

/// Defines an `extern "C"` function that overrides a symbol via `LD_PRELOAD`.
/// The original is resolved lazily (cached) and bound to the `|real|`
/// identifier for use inside the body.
/// `lib:` optionally searches an already loaded library if `RTLD_NEXT` fails.
///
/// # Example
///
/// ```ignore
/// override_sym!(SDL_RenderPresent(renderer: *const ()) |real| {
///     crate::frame::frame_boundary(move || real(renderer));
/// });
///
/// override_sym!(XOpenDisplay(name: *const c_char) -> *const () |real| {
///     real(name)
/// });
/// ```
///
/// A `variadic` hook additionally binds the caller's variadic arguments; its
/// body runs in an `unsafe fn` and calls `real` with the forwarded values.
/// Unlike other hooks its body also runs outside the target process, as
/// varargs can't be forwarded generically:
///
/// ```ignore
/// override_sym!(variadic open(path: *const c_char, flags: c_int) -> c_int |real, args| {
///     unsafe { real(path, flags, args.next_arg::<c_uint>()) }
/// });
/// ```
#[macro_export]
macro_rules! override_sym {
    ($abi:literal $fn:ident ($($param:ident: $ty:ty),*) $(-> $ret_ty:ty)? |$real:ident| $($block:block)?) => {
        $crate::override_sym!(@impl $abi, $fn ($($param: $ty),*) $(-> $ret_ty)?, std::ptr::null(), |$real| $($block)?);
    };
    ($abi:literal $fn:ident ($($param:ident: $ty:ty),*) $(-> $ret_ty:ty)?, lib: $lib:expr, |$real:ident| $($block:block)?) => {
        $crate::override_sym!(@impl $abi, $fn ($($param: $ty),*) $(-> $ret_ty)?, $lib, |$real| $($block)?);
    };
    ($fn:ident ($($param:ident: $ty:ty),*) $(-> $ret_ty:ty)? |$real:ident| $($block:block)?) => {
        $crate::override_sym!(@impl "C", $fn ($($param: $ty),*) $(-> $ret_ty)?, std::ptr::null(), |$real| $($block)?);
    };
    ($fn:ident ($($param:ident: $ty:ty),*) $(-> $ret_ty:ty)?, lib: $lib:expr, |$real:ident| $($block:block)?) => {
        $crate::override_sym!(@impl "C", $fn ($($param: $ty),*) $(-> $ret_ty)?, $lib, |$real| $($block)?);
    };
    (variadic $fn:ident ($($param:ident: $ty:ty),*) $(-> $ret_ty:ty)? |$real:ident, $va:ident| $block:block) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $fn($($param: $ty,)* mut $va: ...) $(-> $ret_ty)? {
            const NAME: &[u8] = concat!(stringify!($fn), "\0").as_bytes();
            static SYM: std::sync::atomic::AtomicPtr<std::ffi::c_void> =
                std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

            let fn_ptr = $crate::hooks::link_function(&SYM, NAME.as_ptr().cast(), std::ptr::null());
            // SAFETY: the symbol resolved under this name is the variadic libc function declared by this hook.
            let $real = unsafe {
                std::mem::transmute::<*mut _, unsafe extern "C" fn($($ty,)* ...) $(-> $ret_ty)?>(
                    fn_ptr.unwrap()
                )
            };

            $block
        }
    };
    (@impl $abi:literal, $fn:ident ($($param:ident: $ty:ty),*) $(-> $ret_ty:ty)?, $lib:expr, |$real:ident| $($block:block)?) => {
        #[unsafe(no_mangle)]
        pub extern $abi fn $fn($($param: $ty),*) $(-> $ret_ty)? {
            const NAME: &[u8] = concat!(stringify!($fn), "\0").as_bytes();
            static SYM: std::sync::atomic::AtomicPtr<std::ffi::c_void> =
                std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

            let fn_ptr = $crate::hooks::link_function(&SYM, NAME.as_ptr().cast(), $lib);
            let fn_ptr = unsafe {
                std::mem::transmute::<*mut _, extern $abi fn($($ty),*) $(-> $ret_ty)?>(
                    fn_ptr.unwrap()
                )
            };

            let $real = fn_ptr;

            if !$crate::is_target() {
                $real($($param),*)
            } else {
                $($block)?
            }

        }
    };
}

/// Sets errno for a hook failing like its libc counterpart and returns that call's -1.
pub(crate) fn fail(errno: rustix::io::Errno) -> std::ffi::c_int {
    // SAFETY: __errno_location returns the calling thread's errno slot, always valid; rustix has no errno setter.
    unsafe { *libc::__errno_location() = errno.raw_os_error() };
    -1
}
