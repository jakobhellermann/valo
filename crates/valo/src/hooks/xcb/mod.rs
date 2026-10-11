#![allow(non_camel_case_types)]

mod conn;
pub mod events;
mod kicks;
pub mod present;
pub mod quiesce;
mod windows;

pub use conn::*;
pub use events::*;
pub use kicks::*;
pub use present::*;

use std::ffi::c_char;

use x11rb_protocol::errors::ParseError;
use x11rb_protocol::protocol::Event;
use x11rb_protocol::x11_utils::{ExtInfoProvider, ExtensionInformation};

pub const LIB_XCB: *const c_char = c"libxcb.so.1".as_ptr();

unsafe extern "C" {
    pub type xcb_connection_t;
    pub type xcb_generic_event_t;
    pub type xcb_generic_error_t;
    pub type xcb_special_event_t;
    pub type xcb_auth_info_t;
    pub type xcb_extension_t;
}
unsafe impl Send for xcb_connection_t {}

#[repr(C)]
pub struct xcb_void_cookie_t {
    sequence: u32,
}

/// The wire bytes of an event libxcb returned.
#[derive(Clone, Copy)]
pub struct XcbEvent<'a>(&'a [u8; 32]);

impl<'a> XcbEvent<'a> {
    /// # Safety
    /// `event` is null or an event returned by libxcb that outlives `'a`.
    pub unsafe fn from_ptr(event: *mut xcb_generic_event_t) -> Option<Self> {
        // SAFETY: libxcb events start with their 32 wire bytes.
        (!event.is_null()).then(|| Self(unsafe { &*event.cast::<[u8; 32]>() }))
    }

    /// The event code without the send-event bit.
    pub fn code(self) -> u8 {
        self.0[0] & 0x7f
    }

    pub fn parse(self) -> Result<Event, ParseError> {
        Event::parse(self.0, &CoreOnly)
    }
}

/// Decodes core events only; extension events come out as `Event::Unknown`.
struct CoreOnly;

impl ExtInfoProvider for CoreOnly {
    fn get_from_major_opcode(&self, _: u8) -> Option<(&str, ExtensionInformation)> {
        None
    }
    fn get_from_event_code(&self, _: u8) -> Option<(&str, ExtensionInformation)> {
        None
    }
    fn get_from_error_code(&self, _: u8) -> Option<(&str, ExtensionInformation)> {
        None
    }
}
