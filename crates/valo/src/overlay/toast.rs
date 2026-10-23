// TODO(ai-review): generated, not yet audited
//! Short messages in the bottom-right corner of the presented frames, newest at the bottom.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use epaint::{Color32, FontId, Fonts, Pos2, Rect, Shape, Vec2};

const DURATION: Duration = Duration::from_secs(1);
const MAX_SHOWN: usize = 4;
/// Sizes are in pixels of an image this tall and scale with the presented image's height.
const REFERENCE_HEIGHT: f32 = 1080.0;
const FONT_SIZE: f32 = 16.0;
const PAD: f32 = 6.0;
const MARGIN: f32 = 16.0;

/// Oldest first.
static TOASTS: Mutex<VecDeque<Toast>> = Mutex::new(VecDeque::new());

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToastKind {
    Info,
    Error,
}

impl ToastKind {
    fn background(self) -> Color32 {
        match self {
            ToastKind::Info => Color32::from_rgb(0x20, 0x20, 0x20),
            ToastKind::Error => Color32::from_rgb(0xb0, 0x18, 0x18),
        }
    }
}

struct Toast {
    kind: ToastKind,
    text: String,
    /// On the real monotonic clock, counted from the first present that shows it.
    until: Option<Duration>,
}

pub(crate) fn toast(kind: ToastKind, text: impl Into<String>) {
    let mut toasts = TOASTS.lock().unwrap();
    toasts.push_back(Toast {
        kind,
        text: text.into(),
        until: None,
    });
    if toasts.len() > MAX_SHOWN {
        toasts.pop_front();
    }
}

/// The toasts the present being drawn shows, oldest first.
pub(super) fn live() -> Vec<(ToastKind, String)> {
    let now = real_now();
    let mut toasts = TOASTS.lock().unwrap();
    toasts.retain(|t| t.until.is_none_or(|until| until > now));
    toasts
        .iter_mut()
        .map(|t| {
            t.until.get_or_insert(now + DURATION);
            (t.kind, t.text.clone())
        })
        .collect()
}

pub(super) fn layout(
    fonts: &mut Fonts,
    width: u32,
    height: u32,
    toasts: &[(ToastKind, String)],
) -> Vec<Shape> {
    let scale = height as f32 / REFERENCE_HEIGHT;
    let (pad, margin) = (PAD * scale, MARGIN * scale);
    let font = FontId::monospace(FONT_SIZE * scale);
    let wrap_width = (width as f32 - 2.0 * (margin + pad)).max(0.0);
    let mut bottom = height as f32 - margin;
    let mut shapes = Vec::new();
    for (kind, text) in toasts.iter().rev() {
        let galley = fonts.with_pixels_per_point(1.0).layout(
            text.clone(),
            font.clone(),
            Color32::WHITE,
            wrap_width,
        );
        let size = galley.size() + Vec2::splat(2.0 * pad);
        let min = Pos2::new(width as f32 - margin - size.x, bottom - size.y);
        let rect = Rect::from_min_size(min, size);
        shapes.push(Shape::rect_filled(rect, 0.0, kind.background()));
        shapes.push(Shape::galley(
            min + Vec2::splat(pad),
            galley,
            Color32::WHITE,
        ));
        bottom = rect.min.y - pad;
    }
    shapes
}

fn real_now() -> Duration {
    let ts = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
}
