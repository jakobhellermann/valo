#![allow(non_camel_case_types)]

mod conn;
pub(crate) mod events;
mod kicks;
pub(crate) mod present;
pub(crate) mod queue;
mod windows;

pub use conn::*;
pub use events::*;
pub use kicks::*;
pub use present::*;

use std::ffi::c_char;

const LIB_XCB: *const c_char = c"libxcb.so.1".as_ptr();

unsafe extern "C" {
    pub type xcb_connection_t;
    pub type xcb_generic_event_t;
    pub type xcb_special_event_t;
    pub type xcb_auth_info_t;
    pub type xcb_extension_t;
}
unsafe impl Send for xcb_connection_t {}
// SAFETY: the event queue hands raw event pointers between threads; they
// point at libc heap allocations, not thread-local state.
unsafe impl Send for xcb_generic_event_t {}

#[repr(C)]
pub struct xcb_void_cookie_t {
    sequence: u32,
}
