// TODO(ai-review): generated, not yet audited
//! epaint's side: text layout with its default fonts and tessellation into triangles.

use std::sync::{Arc, Mutex};

use epaint::text::{FontDefinitions, ViewportKey};
use epaint::{
    ClippedPrimitive, ClippedShape, ColorImage, Fonts, Pos2, Rect, Shape, TessellationOptions,
    Tessellator, TextOptions,
};

static PAINTER: Mutex<Option<Painter>> = Mutex::new(None);

struct Painter {
    fonts: Fonts,
    atlas: Atlas,
}

/// The font atlas, which every vertex samples: shapes other than text use its white texel.
#[derive(Clone)]
pub(super) struct Atlas {
    pub image: Arc<ColorImage>,
    /// Changes whenever `image` does.
    pub version: u64,
}

pub(super) struct Paint {
    pub primitives: Vec<ClippedPrimitive>,
    pub atlas: Atlas,
}

/// Tessellates the shapes `build` lays out for a `width` × `height` image.
pub(super) fn paint(
    width: u32,
    height: u32,
    build: impl FnOnce(&mut Fonts) -> Vec<Shape>,
) -> Paint {
    let mut guard = PAINTER.lock().unwrap();
    let painter = guard.get_or_insert_with(Painter::new);
    painter
        .fonts
        .begin_pass(TextOptions::default(), ViewportKey::default());
    let shapes = build(&mut painter.fonts);
    if painter.fonts.font_image_delta().is_some() {
        painter.atlas = Atlas {
            image: Arc::new(painter.fonts.image()),
            version: painter.atlas.version + 1,
        };
    }
    let clip = Rect::from_min_max(Pos2::ZERO, Pos2::new(width as f32, height as f32));
    let mut tessellator = Tessellator::new(
        1.0,
        TessellationOptions::default(),
        painter.fonts.font_image_size(),
        painter.fonts.texture_atlas().prepared_discs(),
    );
    let primitives = tessellator.tessellate_shapes(
        shapes
            .into_iter()
            .map(|shape| ClippedShape::new(clip, shape))
            .collect(),
    );
    Paint {
        primitives,
        atlas: painter.atlas.clone(),
    }
}

impl Painter {
    fn new() -> Self {
        let mut fonts = Fonts::new(TextOptions::default(), FontDefinitions::default());
        fonts.font_image_delta();
        Painter {
            atlas: Atlas {
                image: Arc::new(fonts.image()),
                version: 0,
            },
            fonts,
        }
    }
}
