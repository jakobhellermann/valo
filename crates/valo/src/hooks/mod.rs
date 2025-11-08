#![allow(clippy::undocumented_unsafe_blocks)]

mod sdl;

#[macro_export]
macro_rules! sym {
    ($cstr:literal) => {{
        #[allow(clippy::undocumented_unsafe_blocks)]
        static SYM: std::sync::LazyLock<$crate::utils::UnsafeSendSync<*mut ()>> =
            std::sync::LazyLock::new(|| {
                let sym = $cstr.as_ptr().cast();
                let res = unsafe { libc::dlsym(libc::RTLD_NEXT, sym) };
                $crate::utils::UnsafeSendSync(res.cast())
            });

        SYM.0
    }};
}
