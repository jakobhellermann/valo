// TODO(ai-review): generated, not yet audited
//! The game window's pixels at a checkpoint, stored in the slot directory.
//! The X server is not rewound, so a restore would otherwise keep showing
//! the discarded timeline's frame until the game presents again.

use std::ffi::c_int;
use std::fs::File;
use std::io::{ErrorKind, Write};
use std::path::Path;
use std::sync::Mutex;

use valo_ipc::config::RestoreRedraw;

use crate::global_state::GlobalState;
use crate::hooks::xcb::{GAME_XCB_CONNS, xcb_connection_t};
use crate::{debug, warn};

const FILE: &str = "window";

/// (window, pixmap) of each window's last Present: while presents flip onto
/// the window, the window's own contents are stale.
static PRESENTED: Mutex<Vec<(u32, u32)>> = Mutex::new(Vec::new());

std::thread_local! {
    static OWN_PRESENT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
const HEADER_LEN: usize = 9;

pub(crate) fn capture(slot_dir: &Path) {
    let path = slot_dir.join(FILE);
    let reply = match read_current() {
        Ok(Some(reply)) => reply,
        Ok(None) => return remove_stale(&path),
        Err(e) => {
            warn!(Checkpoint, "window image capture failed: {e}");
            return remove_stale(&path);
        }
    };
    let image = reply.image();
    if let Err(e) = image.write_to(&path) {
        warn!(Checkpoint, "window image write failed: {e}");
        return remove_stale(&path);
    }
    debug!(
        Checkpoint,
        "captured window {:#x} {}x{} depth {}",
        image.window,
        image.width,
        image.height,
        image.depth
    );
}

/// Paints the slot's image when `at` is the configured redraw point.
pub(crate) fn redraw(slot_dir: &Path, at: RestoreRedraw) {
    if GlobalState::get().restore_redraw() != at {
        return;
    }
    let bytes = match std::fs::read(slot_dir.join(FILE)) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == ErrorKind::NotFound => return,
        Err(e) => {
            warn!(Restore, "window image read failed: {e}");
            return;
        }
    };
    let Some(image) = WindowImage::decode(&bytes) else {
        warn!(Restore, "window image file is truncated");
        return;
    };
    let Some(conn) = game_conn() else {
        warn!(Restore, "window redraw: no xcb connection");
        return;
    };
    let Ok(len) = u32::try_from(image.data.len()) else {
        warn!(Restore, "window redraw: image too large");
        return;
    };
    let present = presented_pixmap(image.window).is_some();
    let write = match present {
        true => valo_xcb_present_window_image,
        false => valo_xcb_put_window_image,
    };
    OWN_PRESENT.set(true);
    // SAFETY: `conn` is a tracked, live game connection, and `data` holds
    // `len` bytes.
    let code = unsafe {
        write(
            conn,
            image.window,
            image.width,
            image.height,
            image.depth,
            image.data.as_ptr(),
            len,
        )
    };
    OWN_PRESENT.set(false);
    if code != 0 {
        warn!(
            Restore,
            "window redraw of {:#x} failed: X error {code}", image.window
        );
    }
}

/// Remembers `pixmap` as the contents of `window`, unless valo presents it itself.
pub(crate) fn record_present(window: u32, pixmap: u32) {
    if OWN_PRESENT.get() {
        return;
    }
    let mut presented = PRESENTED.lock().unwrap();
    match presented.iter_mut().find(|(w, _)| *w == window) {
        Some(entry) => entry.1 = pixmap,
        None => presented.push((window, pixmap)),
    }
}

fn presented_pixmap(window: u32) -> Option<u32> {
    let presented = PRESENTED.lock().unwrap();
    presented
        .iter()
        .find(|(w, _)| *w == window)
        .map(|(_, p)| *p)
}

/// A failed capture must not leave an older checkpoint's image in the slot.
fn remove_stale(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => warn!(Checkpoint, "removing stale window image failed: {e}"),
    }
}

struct WindowImage<'a> {
    window: u32,
    width: u16,
    height: u16,
    depth: u8,
    data: &'a [u8],
}

impl<'a> WindowImage<'a> {
    fn write_to(&self, path: &Path) -> Result<(), std::io::Error> {
        let mut header = [0; HEADER_LEN];
        header[0..4].copy_from_slice(&self.window.to_le_bytes());
        header[4..6].copy_from_slice(&self.width.to_le_bytes());
        header[6..8].copy_from_slice(&self.height.to_le_bytes());
        header[8] = self.depth;
        let mut file = File::create(path)?;
        file.write_all(&header)?;
        file.write_all(self.data)
    }

    fn decode(bytes: &'a [u8]) -> Option<WindowImage<'a>> {
        let (header, data) = bytes.split_first_chunk::<HEADER_LEN>()?;
        Some(WindowImage {
            window: u32::from_le_bytes(header[0..4].try_into().unwrap()),
            width: u16::from_le_bytes(header[4..6].try_into().unwrap()),
            height: u16::from_le_bytes(header[6..8].try_into().unwrap()),
            depth: header[8],
            data,
        })
    }
}

fn read_current() -> Result<Option<ImageReply>, String> {
    let Some(window) = crate::input_feed::current_window() else {
        return Ok(None);
    };
    let conn = game_conn().ok_or("no xcb connection")?;
    let mut raw = RawWindowImage {
        reply: std::ptr::null_mut(),
        data: std::ptr::null(),
        len: 0,
        width: 0,
        height: 0,
        depth: 0,
    };
    let drawable = presented_pixmap(window).unwrap_or(window);
    // SAFETY: `conn` is a tracked, live game connection.
    let code = unsafe { valo_xcb_get_window_image(conn, drawable, &mut raw) };
    if code != 0 {
        return Err(format!(
            "X error {code} on drawable {drawable:#x} of window {window:#x}"
        ));
    }
    Ok(Some(ImageReply { window, raw }))
}

/// A successful `valo_xcb_get_window_image` result; owns the xcb reply.
struct ImageReply {
    window: u32,
    raw: RawWindowImage,
}

impl ImageReply {
    fn image(&self) -> WindowImage<'_> {
        WindowImage {
            window: self.window,
            width: self.raw.width,
            height: self.raw.height,
            depth: self.raw.depth,
            // SAFETY: on success `data` points to `len` bytes inside `reply`,
            // which lives until this value drops.
            data: unsafe { std::slice::from_raw_parts(self.raw.data, self.raw.len as usize) },
        }
    }
}

impl Drop for ImageReply {
    fn drop(&mut self) {
        // SAFETY: on success `reply` is a malloc'd xcb reply this value owns.
        unsafe { libc::free(self.raw.reply) };
    }
}

fn game_conn() -> Option<*mut xcb_connection_t> {
    GAME_XCB_CONNS
        .iter()
        .next()
        .map(|conn| conn as *mut xcb_connection_t)
}

#[repr(C)]
struct RawWindowImage {
    reply: *mut libc::c_void,
    data: *const u8,
    len: u32,
    width: u16,
    height: u16,
    depth: u8,
}

unsafe extern "C" {
    /// # Safety
    /// `conn` must be a live xcb connection.
    fn valo_xcb_get_window_image(
        conn: *mut xcb_connection_t,
        drawable: u32,
        out: *mut RawWindowImage,
    ) -> c_int;

    /// # Safety
    /// `conn` must be a live xcb connection and `data` point to `len`
    /// readable bytes.
    fn valo_xcb_put_window_image(
        conn: *mut xcb_connection_t,
        window: u32,
        width: u16,
        height: u16,
        depth: u8,
        data: *const u8,
        len: u32,
    ) -> c_int;

    /// # Safety
    /// `conn` must be a live xcb connection and `data` point to `len`
    /// readable bytes.
    fn valo_xcb_present_window_image(
        conn: *mut xcb_connection_t,
        window: u32,
        width: u16,
        height: u16,
        depth: u8,
        data: *const u8,
        len: u32,
    ) -> c_int;
}
