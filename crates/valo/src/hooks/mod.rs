#![allow(clippy::undocumented_unsafe_blocks)]

mod dlhook;
mod sdl;
mod xlib;

/// Resolves the next definition of a symbol in the dynamic linker's load order
/// via `dlsym(RTLD_NEXT, ...)`, caching the result.
///
/// # Example
///
/// ```ignore
/// #[unsafe(no_mangle)]
/// pub extern "C" fn SDL_RenderPresent(renderer: *const ()) {
///     let ptr = crate::sym!(c"SDL_RenderPresent");
///     let fn_ptr = unsafe { std::mem::transmute::<*mut (), extern "C" fn(*const ())>(ptr) };
///     fn_ptr(renderer);
/// }
/// ```
///
/// For the common case of a passthrough override, prefer [`override_sym!`](crate::override_sym),
/// which expands to the boilerplate above.
#[macro_export]
macro_rules! sym {
    ($cstr:expr) => {{
        #[allow(clippy::undocumented_unsafe_blocks)]
        static SYM: std::sync::LazyLock<$crate::utils::UnsafeSendSync<*mut ()>> =
            std::sync::LazyLock::new(|| {
                $crate::debug!(
                    Hooks,
                    "hooking '{}'",
                    $cstr.to_str().unwrap_or("invalid utf8")
                );
                let sym: *const i8 = $cstr.as_ptr().cast();
                let res = unsafe { libc::dlsym(libc::RTLD_NEXT, sym) };
                $crate::utils::UnsafeSendSync(res.cast())
            });

        SYM.0
    }};
}

/// Defines an `extern "C"` function that overrides a symbol via `LD_PRELOAD`,
/// runs an optional block, then forwards to the original via [`sym!`](crate::sym).
///
/// # Example
///
/// ```ignore
/// override_sym!(XOpenDisplay(name: *const c_char) {
///     debug!(Hooks, "XOpenDisplay called");
/// });
/// ```
#[macro_export]
macro_rules! override_sym {
    ($fn:ident ($($param:ident: $ty:ty),*) $($block:block)?) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn $fn($($param: $ty),*) -> *const () {
            // SAFETY: static lifetime and nul terminated
            const NAME: &std::ffi::CStr = unsafe {CStr::from_ptr(concat!(stringify!($fn), "\0").as_ptr().cast())};
            let ptr = $crate::sym!(NAME);
            let fn_ptr = unsafe {
                std::mem::transmute::<*mut (), extern "C" fn($($ty),*) -> *const ()>(
                    ptr,
                )
            };

            $($block)?

            fn_ptr($($param),*)
        }
    };
}
