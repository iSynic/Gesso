//! The display list: what to draw, independent of the backend. Built once per fragment and
//! zoom band, replayed by every renderer (GPU, CPU, and later SVG/PDF export).
//!
//! Coordinates: every command carries `xf`, the transform from the shape's local units to the
//! fragment's space. The frame supplies the fragment-to-device transform.

use std::sync::Arc;

use kurbo::{Affine, BezPath, Rect};
use peniko::{BlendMode, Color, Extend, Fill, Gradient, ImageQuality};
use text::GlyphRun;

/// A decoded image (straight-alpha RGBA8), shared between lists and frames.
#[derive(Debug)]
pub struct Image {
    /// Stable key: asset hash + mip level, so backends can cache uploads.
    pub key: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: peniko::Blob<u8>,
}

#[derive(Debug, Clone)]
pub struct ImagePaint {
    pub image: Arc<Image>,
    pub extend: Extend,
    pub quality: ImageQuality,
    pub alpha: f32,
}

/// Paint of a fill, stroke or glyph run. Gradients and images are positioned by the command's
/// `brush_xf` (brush space → local space).
#[derive(Debug, Clone)]
pub enum Brush {
    Solid(Color),
    Gradient(Gradient),
    Image(ImagePaint),
}

/// Effects that need content rendered offscreen, then processed.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Blur the region's content. `sigma` in local units.
    LayerBlur { sigma: f64 },
    /// Draw only a shadow of the region content's alpha: dilated by `spread`, blurred, offset,
    /// tinted. Offsets and lengths in local units.
    DropShadow { color: Color, offset: (f64, f64), sigma: f64, spread: f64 },
    /// Shadow cast inward from the edges of the region content's alpha (the content itself is
    /// not drawn, only the shadow inside it).
    InnerShadow { color: Color, offset: (f64, f64), sigma: f64, spread: f64 },
}

#[derive(Debug, Clone)]
pub enum Cmd {
    /// Start a group composited with `blend` and `alpha`, clipped to `clip` (local path).
    PushLayer { clip: Arc<BezPath>, rule: Fill, xf: Affine, blend: BlendMode, alpha: f32 },
    PopLayer,
    Fill { path: Arc<BezPath>, rule: Fill, xf: Affine, brush: Brush, brush_xf: Affine },
    Stroke { path: Arc<BezPath>, style: Arc<kurbo::Stroke>, xf: Affine, brush: Brush, brush_xf: Affine },
    /// A text run; glyph positions are in local units, `y` on the baseline.
    Glyphs { run: Arc<GlyphRun>, xf: Affine, brush: Brush, brush_xf: Affine },
    /// The analytic blurred rounded rectangle (fast-path shadow).
    BlurredRect { rect: Rect, radius: f64, sigma: f64, color: Color, xf: Affine },
    /// Blur what is already drawn under `clip`, and draw it back inside `clip`.
    BackdropBlur { clip: Arc<BezPath>, xf: Affine, sigma: f64 },
    /// Start an effect region. `bounds` (fragment space) is where its content and result can
    /// draw; `xf` maps the effect's offsets and lengths from the node's local units.
    PushEffect { effect: Effect, bounds: Rect, xf: Affine },
    PopEffect,
}

impl Cmd {
    /// Commands that need offscreen work (cannot go into a cached, effect-free run).
    pub fn needs_offscreen(&self) -> bool {
        matches!(self, Cmd::BackdropBlur { .. } | Cmd::PushEffect { .. } | Cmd::PopEffect)
    }
}

/// A fragment's commands plus what the frame needs to place and cull it.
#[derive(Debug, Clone, Default)]
pub struct DisplayList {
    pub cmds: Vec<Cmd>,
    /// Bounds of everything drawn, in fragment space.
    pub bounds: Option<Rect>,
    /// True if any command needs offscreen work.
    pub has_effects: bool,
    /// Text drawn as greeked bars, items skipped as too small (statistics).
    pub greeked: u32,
    pub skipped: u32,
}

impl DisplayList {
    pub fn push(&mut self, c: Cmd) {
        self.has_effects |= c.needs_offscreen();
        self.cmds.push(c);
    }

    pub fn grow(&mut self, r: Rect) {
        self.bounds = Some(self.bounds.map_or(r, |b| b.union(r)));
    }

    pub fn is_empty(&self) -> bool {
        self.cmds.is_empty()
    }
}
