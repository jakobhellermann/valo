#![allow(non_camel_case_types)]

mod conn;
mod events;
mod present;

pub use conn::*;
pub use events::*;
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

#[repr(C)]
pub struct xcb_void_cookie_t {
    sequence: u32,
}
