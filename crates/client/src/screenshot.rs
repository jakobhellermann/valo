//! Captures an X window's contents into a PNG.

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt, ImageFormat, ImageOrder, MapState, Window};

/// Writes the window's current contents to `path` as an RGB PNG.
pub fn capture(window: Window, path: &Path) -> Result<()> {
    let (conn, _) = x11rb::connect(None).context("connect to the X server")?;
    let attrs = conn
        .get_window_attributes(window)?
        .reply()
        .with_context(|| format!("window 0x{window:x} attributes"))?;
    ensure!(
        attrs.map_state == MapState::VIEWABLE,
        "window 0x{window:x} is not viewable"
    );
    let geometry = conn.get_geometry(window)?.reply()?;
    let (width, height) = (geometry.width, geometry.height);
    let image = conn
        .get_image(ImageFormat::Z_PIXMAP, window, 0, 0, width, height, !0)?
        .reply()
        .with_context(|| format!("read the contents of window 0x{window:x}"))?;

    let visual = conn
        .setup()
        .roots
        .iter()
        .flat_map(|screen| &screen.allowed_depths)
        .flat_map(|depth| &depth.visuals)
        .find(|visual| visual.visual_id == image.visual)
        .with_context(|| format!("visual 0x{:x} of window 0x{window:x}", image.visual))?;
    let bits_per_pixel = conn
        .setup()
        .pixmap_formats
        .iter()
        .find(|format| format.depth == image.depth)
        .map(|format| format.bits_per_pixel)
        .with_context(|| format!("pixmap format of depth {}", image.depth))?;
    let masks = [visual.red_mask, visual.green_mask, visual.blue_mask];
    if bits_per_pixel != 32
        || conn.setup().image_byte_order != ImageOrder::LSB_FIRST
        || masks.iter().any(|mask| mask.count_ones() != 8)
    {
        bail!(
            "unsupported image layout: {bits_per_pixel} bits per pixel, {:?}, masks {masks:x?}",
            conn.setup().image_byte_order
        );
    }

    let pixels = usize::from(width) * usize::from(height);
    ensure!(
        image.data.len() == pixels * 4,
        "image of {width}x{height} has {} bytes",
        image.data.len()
    );
    let channel = |pixel: u32, mask: u32| ((pixel & mask) >> mask.trailing_zeros()) as u8;
    let mut rgb = Vec::with_capacity(pixels * 3);
    for &chunk in image.data.as_chunks::<4>().0 {
        let pixel = u32::from_le_bytes(chunk);
        rgb.extend(masks.map(|mask| channel(pixel, mask)));
    }

    let file = File::create(path).with_context(|| format!("create {}", path.display()))?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), width.into(), height.into());
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&rgb)?;
    writer.finish()?;
    Ok(())
}
