//! Save and restore X11/XCB connection state across checkpoint/restore.
//!
//! After restoring process memory, the X11 Display and XCB connection structs
//! contain stale sequence numbers from checkpoint time. The X server has moved
//! on, so we need to preserve the current (pre-restore) values and write them
//! back after restore.

use super::MAX_DISPLAYS;
use super::xdisplay::GAME_DISPLAYS;
use crate::Result;

include!("generated.rs");

unsafe extern "C" {
    /// # Safety
    /// `display` must be a live Display with no concurrent access.
    /// The output pointers must be writable and `dst` must be writable for
    /// `dst_len` bytes.
    /// The loaded libxcb must match the vendored layout.
    fn valo_x11_save(
        display: *mut (),
        last_request_read: *mut u64,
        request: *mut u64,
        dst: *mut u8,
        dst_len: usize,
    ) -> usize;
    /// # Safety
    /// `display` must be a live Display with no concurrent access.
    /// `src` must be readable for `src_len` bytes.
    /// The loaded libxcb must match the vendored layout.
    fn valo_x11_restore(
        display: *mut (),
        last_request_read: u64,
        request: u64,
        src: *const u8,
        src_len: usize,
    ) -> std::ffi::c_int;
}

struct DisplayState {
    display: *mut (),
    last_request_read: u64,
    request: u64,
    xcb_conn_buf: [u8; XCB_CONN_BUF_SIZE],
}

pub struct X11SavedState {
    displays: [DisplayState; MAX_DISPLAYS],
    count: usize,
}

impl X11SavedState {
    /// Save current X11 connection state. Call BEFORE restoring memory.
    pub fn save() -> Result<Self> {
        #[allow(clippy::large_const_arrays)]
        const EMPTY: DisplayState = DisplayState {
            display: std::ptr::null_mut(),
            last_request_read: 0,
            request: 0,
            xcb_conn_buf: [0; XCB_CONN_BUF_SIZE],
        };
        let mut state = X11SavedState {
            displays: [EMPTY; MAX_DISPLAYS],
            count: 0,
        };

        for (i, display) in GAME_DISPLAYS.iter().enumerate() {
            let ds = &mut state.displays[i];
            ds.display = display;
            // SAFETY: the output pointers and buffer are valid for the supplied
            // sizes. We assume tracked displays remain live and unused by other
            // threads; this still needs to be enforced by thread suspension.
            let copied = unsafe {
                valo_x11_save(
                    display,
                    &mut ds.last_request_read,
                    &mut ds.request,
                    ds.xcb_conn_buf.as_mut_ptr(),
                    ds.xcb_conn_buf.len(),
                )
            };
            if copied != XCB_CONN_BUF_SIZE {
                return Err(std::io::ErrorKind::InvalidData.into());
            }

            state.count += 1;
        }

        Ok(state)
    }

    /// Restore X11 connection state. Call AFTER restoring memory.
    pub fn restore(&self) -> Result<()> {
        // TODO: Handle displays opened or closed since the checkpoint.
        let mut count = 0;
        for (i, display) in GAME_DISPLAYS.iter().enumerate() {
            if i >= self.count || display != self.displays[i].display {
                return Err(std::io::ErrorKind::InvalidData.into());
            }
            count += 1;
        }
        if count != self.count {
            return Err(std::io::ErrorKind::InvalidData.into());
        }

        for ds in self.displays.iter().take(self.count) {
            // SAFETY: the buffer is readable for the generated length and display
            // identity was checked above. We still assume no concurrent X11 use.
            let status = unsafe {
                valo_x11_restore(
                    ds.display,
                    ds.last_request_read,
                    ds.request,
                    ds.xcb_conn_buf.as_ptr(),
                    XCB_CONN_BUF_SIZE,
                )
            };
            if status != 0 {
                return Err(std::io::ErrorKind::InvalidData.into());
            }
        }
        Ok(())
    }
}
