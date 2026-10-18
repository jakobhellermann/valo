//! Debug overlay: short toasts in a click-through child window in the
//! bottom-right corner of the game window, on its own X connection.

use std::sync::atomic::AtomicU32;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use font8x8::{BASIC_FONTS, UnicodeFonts};
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::shape::{self, SK, SO};
use x11rb::protocol::xproto::{
    ChangeWindowAttributesAux, ClipOrdering, ColormapAlloc, ConfigureWindowAux, ConnectionExt,
    CreateGCAux, CreateWindowAux, EventMask, Gcontext, ImageFormat, ImageOrder, StackMode,
    VisualClass, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;

use crate::input::{GameWindow, viewable_game_window};

const DISCOVER_INTERVAL: Duration = Duration::from_millis(200);
const DISCOVER_REPORT_AFTER: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(30);
const TOAST_DURATION: Duration = Duration::from_secs(1);

const GLYPH_SCALE: usize = 2;
const GLYPH_PX: usize = 8 * GLYPH_SCALE;
const PAD: usize = 6;
const WIN_W: u16 = (16 * GLYPH_PX + 2 * PAD) as u16;
const WIN_H: u16 = (GLYPH_PX + 2 * PAD) as u16;
const MARGIN: i32 = 16;

const TEXT: u32 = 0xffff_ffff;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Error,
}

impl ToastKind {
    fn background(self) -> u32 {
        match self {
            ToastKind::Info => premultiplied(0xff, 0x20, 0x20, 0x20),
            ToastKind::Error => premultiplied(0xff, 0xb0, 0x18, 0x18),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Toast {
    kind: ToastKind,
    text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OverlayEvent {
    Status(String),
    /// The overlay thread has exited.
    Fatal(String),
}

pub struct Overlay {
    toasts: Sender<Toast>,
}

impl Overlay {
    /// No-op once the overlay thread has exited; that exit was already reported as `Fatal`.
    pub fn toast(&self, kind: ToastKind, text: impl Into<String>) {
        let _ = self.toasts.send(Toast {
            kind,
            text: text.into(),
        });
    }
}

pub fn spawn_thread<F>(game_window: GameWindow, on_event: F) -> Overlay
where
    F: Fn(OverlayEvent) + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("overlay".into())
        .spawn(move || {
            if let Err(e) = run(&game_window, &rx, &on_event) {
                on_event(OverlayEvent::Fatal(format!("{e:#}")));
            }
        })
        .expect("spawn overlay thread");
    Overlay { toasts: tx }
}

struct Ctx<'a> {
    conn: &'a RustConnection,
    overlay: Window,
    gc: Gcontext,
}

fn run(
    game_window: &AtomicU32,
    toasts: &Receiver<Toast>,
    on_event: &impl Fn(OverlayEvent),
) -> Result<()> {
    let (conn, screen) = x11rb::connect(None).context("connect to X")?;
    if conn.setup().image_byte_order != ImageOrder::LSB_FIRST {
        bail!("X server image byte order is not LSB first");
    }
    let screen = &conn.setup().roots[screen];
    let root = screen.root;

    let visual = screen
        .allowed_depths
        .iter()
        .filter(|d| d.depth == 32)
        .flat_map(|d| &d.visuals)
        .find(|v| {
            v.class == VisualClass::TRUE_COLOR
                && (v.red_mask, v.green_mask, v.blue_mask) == (0xff0000, 0xff00, 0xff)
        })
        .context("no 32-bit ARGB TrueColor visual")?
        .visual_id;

    let colormap = conn.generate_id()?;
    conn.create_colormap(ColormapAlloc::NONE, colormap, root, visual)?
        .check()
        .context("create ARGB colormap")?;

    loop {
        let started = Instant::now();
        let mut reported = false;
        let game = loop {
            if let Some(w) = viewable_game_window(&conn, game_window) {
                break w;
            }
            if !reported && started.elapsed() > DISCOVER_REPORT_AFTER {
                on_event(OverlayEvent::Status(
                    "the game reported no viewable X window yet".into(),
                ));
                reported = true;
            }
            thread::sleep(DISCOVER_INTERVAL);
        };
        conn.change_window_attributes(
            game,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::STRUCTURE_NOTIFY),
        )?
        .check()
        .context("select StructureNotify on game window")?;

        // A child window bypasses the window manager's toplevel/popup heuristics.
        let overlay = conn.generate_id()?;
        conn.create_window(
            32,
            overlay,
            game,
            0,
            0,
            WIN_W,
            WIN_H,
            0,
            WindowClass::INPUT_OUTPUT,
            visual,
            &CreateWindowAux::new()
                .background_pixel(0)
                .border_pixel(0)
                .colormap(colormap)
                .event_mask(EventMask::EXPOSURE),
        )?
        .check()
        .context("create overlay window")?;
        shape::rectangles(
            &conn,
            SO::SET,
            SK::INPUT,
            ClipOrdering::UNSORTED,
            overlay,
            0,
            0,
            &[],
        )?
        .check()
        .context("clear overlay input shape")?;
        let gc = conn.generate_id()?;
        conn.create_gc(gc, overlay, &CreateGCAux::new())?
            .check()
            .context("create overlay gc")?;

        let ctx = Ctx {
            conn: &conn,
            overlay,
            gc,
        };
        on_event(OverlayEvent::Status(format!(
            "attached to window {game:#x}"
        )));

        if !follow(&ctx, game, toasts)? {
            return Ok(());
        }
    }
}

/// Returns `true` once the game window is destroyed, `false` once no more toasts can arrive.
fn follow(ctx: &Ctx, game: Window, toasts: &Receiver<Toast>) -> Result<bool> {
    let mut shown: Option<(Toast, Instant)> = None;
    loop {
        while let Some(event) = ctx.conn.poll_for_event().context("X connection")? {
            match event {
                Event::ConfigureNotify(e) if e.window == game => {
                    if let Some((toast, _)) = &shown {
                        show(ctx, game, toast)?;
                    }
                }
                Event::Expose(e) if e.window == ctx.overlay && e.count == 0 => {
                    if let Some((toast, _)) = &shown {
                        show(ctx, game, toast)?;
                    }
                }
                Event::DestroyNotify(e) if e.window == game => return Ok(true),
                Event::Error(e) => bail!("X error: {e:?}"),
                _ => {}
            }
        }

        match toasts.recv_timeout(POLL_INTERVAL) {
            Ok(toast) => {
                show(ctx, game, &toast)?;
                shown = Some((toast, Instant::now() + TOAST_DURATION));
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Ok(false),
        }
        if shown
            .as_ref()
            .is_some_and(|(_, until)| Instant::now() >= *until)
        {
            ctx.conn.unmap_window(ctx.overlay)?;
            shown = None;
        }
        ctx.conn.flush()?;
    }
}

/// Sizes the overlay to the toast, maps it in the game window's bottom-right corner and draws it.
fn show(ctx: &Ctx, game: Window, toast: &Toast) -> Result<()> {
    let size = ctx
        .conn
        .get_geometry(game)?
        .reply()
        .context("query game window geometry")?;
    let (width, pixels) = render(toast, usize::from(WIN_W.min(size.width)));
    let x = (i32::from(size.width) - width as i32 - MARGIN).max(0);
    let y = (i32::from(size.height) - i32::from(WIN_H) - MARGIN).max(0);
    ctx.conn
        .configure_window(
            ctx.overlay,
            &ConfigureWindowAux::new()
                .x(x)
                .y(y)
                .width(width as u32)
                .stack_mode(StackMode::ABOVE),
        )?
        .check()
        .context("place overlay")?;
    ctx.conn
        .map_window(ctx.overlay)?
        .check()
        .context("map overlay")?;
    let bytes: Vec<u8> = pixels.iter().flat_map(|p| p.to_le_bytes()).collect();
    ctx.conn
        .put_image(
            ImageFormat::Z_PIXMAP,
            ctx.overlay,
            ctx.gc,
            width as u16,
            WIN_H,
            0,
            0,
            0,
            32,
            &bytes,
        )?
        .check()
        .context("draw toast")?;
    Ok(())
}

/// Renders `toast` as a box `WIN_H` tall, truncating text that does not fit in `max_width`.
fn render(toast: &Toast, max_width: usize) -> (usize, Vec<u32>) {
    let max_chars = max_width.saturating_sub(2 * PAD) / GLYPH_PX;
    let chars: Vec<char> = toast.text.chars().take(max_chars).collect();
    let width = chars.len() * GLYPH_PX + 2 * PAD;
    let height = usize::from(WIN_H);
    let mut pixels = vec![toast.kind.background(); width * height];
    for (i, c) in chars.iter().enumerate() {
        let glyph = BASIC_FONTS
            .get(*c)
            .or_else(|| BASIC_FONTS.get('?'))
            .expect("'?' is in the basic font");
        for (row, bits) in glyph.iter().enumerate() {
            for col in 0..8 {
                if bits & (1 << col) == 0 {
                    continue;
                }
                for dy in 0..GLYPH_SCALE {
                    let y = PAD + row * GLYPH_SCALE + dy;
                    let x = PAD + i * GLYPH_PX + col * GLYPH_SCALE;
                    pixels[y * width + x..][..GLYPH_SCALE].fill(TEXT);
                }
            }
        }
    }
    (width, pixels)
}

fn premultiplied(a: u32, r: u32, g: u32, b: u32) -> u32 {
    (a << 24) | ((r * a / 255) << 16) | ((g * a / 255) << 8) | (b * a / 255)
}
