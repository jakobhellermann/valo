// TODO(ai-review): generated, not yet audited
//! Shapes the game submits for the frame it renders, handed on to the present showing that frame.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use epaint::{Color32, Pos2, Shape, Stroke};
use valo_ipc::config::OverlayConfig;

use crate::warn;

/// More frames queued for a present than any swap chain keeps in flight: presents stopped
/// taking them, so the oldest are dropped.
const MAX_PENDING: usize = 8;

static ENABLED: AtomicBool = AtomicBool::new(false);

static FRAMES: Mutex<Frames> = Mutex::new(Frames {
    submitted: None,
    pending: VecDeque::new(),
});

struct Frames {
    /// The game's latest submission, for the frame it is rendering.
    submitted: Option<Frame>,
    /// Submissions of frames that were handed to the driver for presenting, oldest first: a
    /// driver on Vulkan (zink) presents them later from its own thread.
    pending: VecDeque<Option<Frame>>,
}

struct Frame {
    width: u32,
    height: u32,
    lines: Vec<OverlayLine>,
    circles: Vec<OverlayCircle>,
}

/// A line in pixels from the top-left of the submitter's screen; `argb` is straight alpha.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OverlayLine {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
    pub width: f32,
    pub argb: u32,
}

/// A circle outline in pixels from the top-left of the submitter's screen.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OverlayCircle {
    pub x: f32,
    pub y: f32,
    pub radius: f32,
    pub width: f32,
    pub argb: u32,
}

pub fn set_config(config: &OverlayConfig) {
    ENABLED.store(config.enabled, Ordering::Relaxed);
}

pub fn config() -> OverlayConfig {
    OverlayConfig {
        enabled: ENABLED.load(Ordering::Relaxed),
    }
}

/// Whether submitted shapes get drawn.
#[unsafe(no_mangle)]
pub extern "C" fn valo_overlay_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Replaces the shapes drawn over the frame being rendered, on a `width` × `height` screen.
///
/// # Safety
/// `lines` and `circles` point to `line_count` and `circle_count` elements, or are null with a
/// count of 0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn valo_overlay_shapes(
    width: u32,
    height: u32,
    lines: *const OverlayLine,
    line_count: usize,
    circles: *const OverlayCircle,
    circle_count: usize,
) {
    // SAFETY: the caller's contract.
    let (lines, circles) = unsafe { (slice(lines, line_count), slice(circles, circle_count)) };
    FRAMES.lock().unwrap().submitted = Some(Frame {
        width,
        height,
        lines: lines.to_vec(),
        circles: circles.to_vec(),
    });
}

unsafe fn slice<'a, T>(ptr: *const T, len: usize) -> &'a [T] {
    if len == 0 {
        &[]
    } else {
        // SAFETY: the caller's contract.
        unsafe { std::slice::from_raw_parts(ptr, len) }
    }
}

/// The frame being rendered goes to the driver for presenting: its submission becomes the one the
/// next overlay draw takes.
pub(crate) fn queue_frame() {
    let mut frames = FRAMES.lock().unwrap();
    let frame = frames.submitted.take();
    frames.pending.push_back(frame);
    if frames.pending.len() > MAX_PENDING {
        warn!(
            Overlay,
            "{} frames queued without a present taking them, dropping the oldest",
            frames.pending.len()
        );
        frames.pending.pop_front();
    }
}

/// The shapes of the frame being presented, scaled to the presented image.
pub(super) fn take_shapes(width: u32, height: u32) -> Vec<Shape> {
    let Some(frame) = FRAMES.lock().unwrap().pending.pop_front().flatten() else {
        return Vec::new();
    };
    if !ENABLED.load(Ordering::Relaxed) || frame.width == 0 || frame.height == 0 {
        return Vec::new();
    }
    let sx = width as f32 / frame.width as f32;
    let sy = height as f32 / frame.height as f32;
    let at = |x: f32, y: f32| Pos2::new(x * sx, y * sy);
    let lines = frame.lines.iter().map(|l| {
        Shape::line_segment(
            [at(l.x1, l.y1), at(l.x2, l.y2)],
            Stroke::new(l.width, color(l.argb)),
        )
    });
    let circles = frame.circles.iter().map(|c| {
        Shape::circle_stroke(
            at(c.x, c.y),
            c.radius * sx,
            Stroke::new(c.width, color(c.argb)),
        )
    });
    lines.chain(circles).collect()
}

fn color(argb: u32) -> Color32 {
    let [b, g, r, a] = argb.to_le_bytes();
    Color32::from_rgba_unmultiplied(r, g, b, a)
}
