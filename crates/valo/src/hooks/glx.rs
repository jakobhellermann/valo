//! GLX hooks.
//! `glXSwapBuffers` acts as a frame boundary.

// Inspired by libTAS: <https://github.com/clementgallet/libTAS/blob/master/src/library/rendering/glxwrappers.cpp>

use crate::determinism::skip_draw;
use crate::{debug, override_sym};
use std::ffi::{CStr, c_char, c_int, c_uint};
use std::os::raw::c_void;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use crate::hooks::{link_function, run_native};
use crate::token::Exclusive;

const LIB: *const c_char = c"libGL.so.1".as_ptr();

type Display = *const ();
type GLXDrawable = libc::c_ulong;
type GLXContext = *const ();
type GLXFBConfig = *const ();
type Bool_ = c_int;
type GLubyte = u8;

/// Whether a GLX context was ever made current: only then can GL work
/// exist that a checkpoint must drain.
static GL_CONTEXT_EVER_CURRENT: AtomicBool = AtomicBool::new(false);
static GL_FINISH: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static GL_FLUSH: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

type FinishFn = unsafe extern "C" fn();

/// Complete all GL work of the calling thread's context before a
/// checkpoint or restore freezes the process. GL drivers submit
/// asynchronously from their own worker threads; the freeze may only
/// park those workers idle, never mid-transfer, or the freeze captures
/// shared submission state that no side will ever finish: a checkpoint
/// would snapshot it half-consumed, a restore would leave transport
/// traffic of the discarded timeline behind. `glFinish` returns once
/// the graphics stack processed every command of the current context,
/// which proves the driver's submission queues are drained and the
/// host consumed everything. A wedged graphics stack blocks here: the
/// honest outcome, since a snapshot taken around in-flight submissions
/// would restore into a desynced driver. The proof parameter ties the
/// call to the exclusive checkpoint/restore session.
pub fn quiesce(_proof: &mut Exclusive) {
    if !GL_CONTEXT_EVER_CURRENT.load(Ordering::Acquire) {
        return;
    }
    let Some(finish) = link_function(&GL_FINISH, c"glFinish".as_ptr(), LIB) else {
        return;
    };
    // SAFETY: the linked symbol is the real glFinish, and a context was
    // made current at least once; without a current context on this
    // thread libGL's dispatch is a no-op. Native mode keeps the
    // driver's internal waits out of the hooked sleep and time paths.
    let finish: FinishFn = unsafe { std::mem::transmute(finish) };
    run_native(|| unsafe { finish() });
}

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
    let made = real(dpy, drawable, ctx);
    if made != 0 {
        GL_CONTEXT_EVER_CURRENT.store(true, Ordering::Release);
    }
    made
});

override_sym!(glXMakeContextCurrent(dpy: Display, draw: GLXDrawable, read: GLXDrawable, ctx: GLXContext) -> Bool_, lib: LIB, |real| {
    debug!(Hooks, "glXMakeContextCurrent");
    let made = real(dpy, draw, read, ctx);
    if made != 0 {
        GL_CONTEXT_EVER_CURRENT.store(true, Ordering::Release);
    }
    made
});

// A skipped frame does not swap at all: GL drivers on Vulkan (zink) acquire the next swapchain
// image inside the swap, and the presentation engine hands images back at display rate.
override_sym!(glXSwapBuffers(dpy: Display, drawable: GLXDrawable), lib: LIB, |real| {
    crate::frame::frame_boundary(|| {
        if skip_draw::skips_this_frame() {
            flush();
        } else {
            real(dpy, drawable);
        }
    });
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

/// Submits the frame's GL work without presenting it.
fn flush() {
    let flush = link_function(&GL_FLUSH, c"glFlush".as_ptr(), LIB)
        .expect("glXSwapBuffers called without a loaded libGL");
    // SAFETY: the linked symbol is the real glFlush, which takes no arguments and returns nothing.
    let flush: FinishFn = unsafe { std::mem::transmute(flush) };
    // SAFETY: glFlush has no preconditions; without a current context libGL's dispatch is a no-op.
    unsafe { flush() };
}
