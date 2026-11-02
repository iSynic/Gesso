//! Rendering, independent of the backend: display lists built from the document, retained per
//! fragment and zoom band ([`Scene`]), and the [`Renderer`] trait that backends implement.
//!
//! Colour: sRGB-encoded values throughout; blending and gradient interpolation happen in sRGB,
//! as in CSS. Blur: a design blur value `v` is a Gaussian with σ = v / 2.

pub mod blur;
pub mod build;
pub mod exec;
pub mod geometry;
pub mod images;
pub mod list;
pub mod paint;
pub mod scene;

use std::sync::Arc;

use kurbo::{Affine, BezPath};
use peniko::Color;

pub use build::{Band, Part};
pub use exec::{Backend, IRect, Process, RunKey, execute};
pub use images::{AssetSource, ImageCache, MemoryAssets, NoAssets, Waker};
pub use list::{Brush, Cmd, DisplayList, Effect, Image, ImagePaint};
pub use scene::{FrameStats, Scene};

/// One fragment as drawn in a frame.
#[derive(Debug, Clone)]
pub struct FrameItem {
    /// Stable per fragment and band; with `version`, identifies the list's content.
    pub fragment: u64,
    pub version: u64,
    pub list: Arc<DisplayList>,
    /// Fragment space → world.
    pub xf: Affine,
    /// Ancestor clips (path, path space → world), outermost first.
    pub clips: Vec<(Arc<BezPath>, Affine)>,
}

/// Everything a renderer needs to draw one image.
#[derive(Debug, Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// World → device pixels.
    pub camera: Affine,
    pub background: Color,
    /// Fragments in drawing order.
    pub items: Vec<FrameItem>,
}

/// What rendering a frame did.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RenderStats {
    pub fragments: usize,
    pub commands: usize,
    /// Effect regions rendered offscreen.
    pub offscreen_passes: usize,
    /// Backdrop snapshots (one per background blur).
    pub backdrop_passes: usize,
    /// Effect-free runs re-encoded vs. reused from a backend cache.
    pub runs_encoded: usize,
    pub runs_reused: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("target size {0}×{1} is not supported")]
    Size(u32, u32),
    #[error("GPU error: {0}")]
    Gpu(String),
}

/// A backend that turns frames into pixels on some target.
pub trait Renderer {
    type Target;
    fn render(&mut self, frame: &Frame, target: &mut Self::Target) -> Result<RenderStats, RenderError>;
}

/// A camera that shows world rectangle `view` in a `width × height` target, preserving aspect.
pub fn fit_camera(view: kurbo::Rect, width: u32, height: u32) -> Affine {
    let s = (width as f64 / view.width().max(1e-9)).min(height as f64 / view.height().max(1e-9));
    let (dx, dy) = ((width as f64 - view.width() * s) / 2.0, (height as f64 - view.height() * s) / 2.0);
    Affine::translate((dx, dy)) * Affine::scale(s) * Affine::translate((-view.x0, -view.y0))
}
