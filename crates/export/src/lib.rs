//! Export: SVG, PDF and raster images of nodes, from the display lists the screen draws (built at
//! full detail: nothing skipped or greeked, effects in full, images at full resolution).
//!
//! Export space: world units, translated so the content's bounds start at the origin. Writers
//! scale it by [`Options::scale`] (pixels for raster and SVG sizes, points for PDF).
//!
//! What vector formats cannot express is rasterised by `render-cpu` at the export resolution
//! and embedded as an image: effects (shadows, blurs), background blur, sweep gradients, and in
//! PDF gradients with transparent stops or repeating extends.

mod glyphs;
pub mod import;
pub mod pdf;
pub mod raster;
pub mod svg;

use std::sync::Arc;

use doc::{Document, NodeId, NodeKey};
use kurbo::{Affine, Rect};
use render::build::{Builder, Caches};
use render::{AssetSource, DisplayList, ImageCache, Part, Scene};
use text::{FontDb, TextEngine};

/// Largest export, in pixels per side.
pub const MAX_PX: u32 = 16_384;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Png,
    Jpeg,
    Webp,
    Svg,
    Pdf,
}

impl Format {
    pub fn parse(s: &str) -> Option<Format> {
        Some(match s.to_ascii_lowercase().as_str() {
            "png" => Format::Png,
            "jpg" | "jpeg" => Format::Jpeg,
            "webp" => Format::Webp,
            "svg" => Format::Svg,
            "pdf" => Format::Pdf,
            _ => return None,
        })
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Png => "png",
            Format::Jpeg => "jpg",
            Format::Webp => "webp",
            Format::Svg => "svg",
            Format::Pdf => "pdf",
        }
    }

    pub const ALL: [Format; 5] = [Format::Png, Format::Jpeg, Format::Webp, Format::Svg, Format::Pdf];
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Options {
    /// Pixels (or PDF points) per world unit.
    pub scale: f64,
    /// JPEG quality, 1–100.
    pub quality: u8,
}

impl Default for Options {
    fn default() -> Self {
        Options { scale: 1.0, quality: 90 }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("node {0} not found")]
    NotFound(NodeId),
    #[error("nothing to export")]
    Empty,
    #[error("export of {0}×{1} px is larger than {MAX_PX} px per side")]
    TooLarge(u32, u32),
    #[error("scale must be a positive number")]
    Scale,
    #[error(transparent)]
    Render(#[from] render::RenderError),
    #[error("encoding failed: {0}")]
    Encode(String),
}

/// One node's display list, placed in export space.
#[derive(Debug, Clone)]
pub struct Item {
    pub list: Arc<DisplayList>,
    /// List space → export space.
    pub xf: Affine,
}

/// What an export draws: lists in drawing order, and their bounds.
#[derive(Debug, Clone)]
pub struct Content {
    pub items: Vec<Item>,
    /// Export-space size (the bounds start at the origin).
    pub width: f64,
    pub height: f64,
    /// Where the export-space origin is in world space.
    pub origin: (f64, f64),
}

impl Content {
    /// Adds `p` units of empty space on every side.
    pub fn pad(&mut self, p: f64) {
        let p = p.max(0.0).round();
        for it in &mut self.items {
            it.xf = Affine::translate((p, p)) * it.xf;
        }
        self.width += 2.0 * p;
        self.height += 2.0 * p;
        self.origin = (self.origin.0 - p, self.origin.1 - p);
    }

    /// Size in pixels at `scale`.
    pub fn pixels(&self, scale: f64) -> (u32, u32) {
        ((self.width * scale).ceil().max(1.0) as u32, (self.height * scale).ceil().max(1.0) as u32)
    }
}

fn world(doc: &Document, key: NodeKey) -> Affine {
    let mut t = doc.node(key).map_or(Affine::IDENTITY, index::local_transform);
    for a in doc.ancestors(key) {
        if let Some(n) = doc.node(a) {
            t = index::local_transform(n) * t;
        }
    }
    t
}

/// Builds the display lists of `ids` (each with its subtree) at full detail. A node inside
/// another listed node is drawn once, by its ancestor. Lists are in document order.
pub fn collect(doc: &Document, ids: &[NodeId], fonts: FontDb, assets: &dyn AssetSource) -> Result<Content, ExportError> {
    collect_with(doc, ids, &mut TextEngine::new(fonts), &mut ImageCache::with_budget(usize::MAX), assets)
}

/// [`collect`] with existing caches (an open document's scene): text already shaped and images
/// already decoded are reused.
pub fn collect_with(doc: &Document, ids: &[NodeId], text: &mut TextEngine, images: &mut ImageCache, assets: &dyn AssetSource) -> Result<Content, ExportError> {
    let mut keys = Vec::new();
    for id in ids {
        keys.push(doc.key(*id).ok_or(ExportError::NotFound(*id))?);
    }
    let listed = keys.clone();
    keys.retain(|k| !doc.ancestors(*k).any(|a| listed.contains(&a)));
    keys.dedup();
    // Document (drawing) order: by path of child indices from the root.
    let path = |k: NodeKey| -> Vec<usize> {
        let mut p = Vec::new();
        let mut cur = k;
        while let Some(parent) = doc.node(cur).and_then(|n| n.parent) {
            let i = doc.node(parent).and_then(|n| n.children.iter().position(|c| *c == cur)).unwrap_or(0);
            p.push(i);
            cur = parent;
        }
        p.reverse();
        p
    };
    keys.sort_by_cached_key(|k| path(*k));

    let mut wanted = Vec::new();
    for k in &keys {
        Scene::assets_in(doc, *k, &mut wanted);
    }
    images.prefetch(assets, &wanted);

    let mut items = Vec::new();
    let mut bounds: Option<Rect> = None;
    for k in keys {
        let mut list = DisplayList::default();
        Builder::exact(doc, Caches::Fill { text: &mut *text, images: &mut *images }).root(k, Part::Whole, &mut list);
        let xf = world(doc, k);
        if let Some(b) = list.bounds {
            let w = xf.transform_rect_bbox(b);
            bounds = Some(bounds.map_or(w, |x| x.union(w)));
        }
        items.push((list, xf));
    }
    let b = bounds.ok_or(ExportError::Empty)?;
    // Whole units, so a 1× export lands on pixel edges as on screen.
    let b = Rect::new(b.x0.floor(), b.y0.floor(), b.x1.ceil().max(b.x0.floor() + 1.0), b.y1.ceil().max(b.y0.floor() + 1.0));
    let to_export = Affine::translate((-b.x0, -b.y0));
    Ok(Content {
        items: items.into_iter().map(|(list, xf)| Item { list: Arc::new(list), xf: to_export * xf }).collect(),
        width: b.width(),
        height: b.height(),
        origin: (b.x0, b.y0),
    })
}

/// Exports `ids` from `doc` as `format`.
pub fn export(doc: &Document, ids: &[NodeId], format: Format, opts: Options, fonts: FontDb, assets: &dyn AssetSource) -> Result<Vec<u8>, ExportError> {
    write(&collect(doc, ids, fonts, assets)?, format, opts)
}

/// Writes collected content as `format`.
pub fn write(content: &Content, format: Format, opts: Options) -> Result<Vec<u8>, ExportError> {
    if !(opts.scale.is_finite() && opts.scale > 0.0) {
        return Err(ExportError::Scale);
    }
    let (w, h) = content.pixels(opts.scale);
    if w > MAX_PX || h > MAX_PX {
        return Err(ExportError::TooLarge(w, h));
    }
    match format {
        Format::Png | Format::Jpeg | Format::Webp => {
            let px = raster::render(content, opts.scale)?;
            raster::encode(&px, format, opts.quality)
        }
        Format::Svg => Ok(svg::write(content, opts.scale)?.into_bytes()),
        Format::Pdf => pdf::write(content, opts.scale),
    }
}
