//! GLX hooks.
//! `glXSwapBuffers` acts as a frame boundary.

// Inspired by libTAS: <https://github.com/clementgallet/libTAS/blob/master/src/library/rendering/glxwrappers.cpp>

use crate::{debug, override_sym};
use std::ffi::{CStr, c_char, c_int, c_uint};

const LIB: *const c_char = c"libGL.so.1".as_ptr();

type Display = *const ();
type GLXDrawable = libc::c_ulong;
type GLXContext = *const ();
type GLXFBConfig = *const ();
type Bool_ = c_int;
type GLubyte = u8;

override_sym!(glXGetProcAddress(name: *const GLubyte) -> *const (), lib: LIB, |real| {
    let r = real(name);
    intercept_proc_addr(name, r)
});

override_sym!(glXGetProcAddressARB(name: *const GLubyte) -> *const (), lib: LIB, |real| {
    let r = real(name);
    intercept_proc_addr(name, r)
});

override_sym!(glXGetProcAddressEXT(name: *const GLubyte) -> *const (), lib: LIB, |real| {
    let r = real(name);
    intercept_proc_addr(name, r)
});

override_sym!(glXMakeCurrent(dpy: Display, drawable: GLXDrawable, ctx: GLXContext) -> Bool_, lib: LIB, |real| {
    debug!(Hooks, "glXMakeCurrent");
    real(dpy, drawable, ctx)
});

override_sym!(glXMakeContextCurrent(dpy: Display, draw: GLXDrawable, read: GLXDrawable, ctx: GLXContext) -> Bool_, lib: LIB, |real| {
    debug!(Hooks, "glXMakeContextCurrent");
    real(dpy, draw, read, ctx)
});

override_sym!(glXSwapBuffers(dpy: Display, drawable: GLXDrawable), lib: LIB, |real| {
    crate::frame::frame_boundary(|| real(dpy, drawable));
});

override_sym!(glXSwapIntervalEXT(dpy: Display, drawable: GLXDrawable, interval: c_int), lib: LIB, |real| {
    debug!(Hooks, "glXSwapIntervalEXT interval={interval}");
    real(dpy, drawable, interval)
});

override_sym!(glXSwapIntervalSGI(interval: c_int) -> c_int, lib: LIB, |real| {
    debug!(Hooks, "glXSwapIntervalSGI interval={interval}");
    real(interval)
});

override_sym!(glXSwapIntervalMESA(interval: c_uint) -> c_int, lib: LIB, |real| {
    debug!(Hooks, "glXSwapIntervalMESA interval={interval}");
    real(interval)
});

override_sym!(glXGetSwapIntervalMESA() -> c_int, lib: LIB, |real| {
    debug!(Hooks, "glXGetSwapIntervalMESA");
    real()
});

override_sym!(glXQueryExtensionsString(dpy: Display, screen: c_int) -> *const c_char, lib: LIB, |real| {
    debug!(Hooks, "glXQueryExtensionsString");
    real(dpy, screen)
});

override_sym!(glXQueryDrawable(dpy: Display, draw: GLXDrawable, attribute: c_int, value: *mut c_uint), lib: LIB, |real| {
    debug!(Hooks, "glXQueryDrawable attribute={attribute}");
    real(dpy, draw, attribute, value)
});

override_sym!(glXCreateContextAttribsARB( dpy: Display, config: GLXFBConfig, share_context: GLXContext, direct: Bool_, attrib_list: *const c_int) -> GLXContext, lib: LIB, |real| {
    debug!(Hooks, "glXCreateContextAttribsARB");
    real(dpy, config, share_context, direct, attrib_list)
});

override_sym!(glXDestroyContext(dpy: Display, ctx: GLXContext), lib: LIB, |real| {
    debug!(Hooks, "glXDestroyContext");
    real(dpy, ctx)
});

fn intercept_proc_addr(name: *const GLubyte, real: *const ()) -> *const () {
    if name.is_null() {
        return real;
    }
    // SAFETY: callers pass a NUL-terminated C string.
    let s = unsafe { CStr::from_ptr(name as *const c_char) };
    let ours: Option<*const ()> = match s.to_bytes() {
        b"glXMakeCurrent" => Some(glXMakeCurrent as *const ()),
        b"glXMakeContextCurrent" => Some(glXMakeContextCurrent as *const ()),
        b"glXSwapBuffers" => Some(glXSwapBuffers as *const ()),
        b"glXQueryDrawable" => Some(glXQueryDrawable as *const ()),
        b"glXSwapIntervalEXT" => Some(glXSwapIntervalEXT as *const ()),
        b"glXSwapIntervalSGI" => Some(glXSwapIntervalSGI as *const ()),
        b"glXSwapIntervalMESA" => Some(glXSwapIntervalMESA as *const ()),
        b"glXGetSwapIntervalMESA" => Some(glXGetSwapIntervalMESA as *const ()),
        b"glXQueryExtensionsString" => Some(glXQueryExtensionsString as *const ()),
        b"glXCreateContextAttribsARB" => Some(glXCreateContextAttribsARB as *const ()),
        b"glXDestroyContext" => Some(glXDestroyContext as *const ()),
        _ => None,
    };
    let name_str = s.to_str().unwrap_or("<bad utf8>");
    if let Some(ours) = ours {
        debug!(Hooks, "glXGetProcAddress({name_str}) -> ours");
        ours
    } else {
        debug!(Hooks, "glXGetProcAddress({name_str}) -> real");
        real
    }
}
