//! Glyph outlines from the font files, for writers that draw text as paths.

use std::sync::Arc;

use kurbo::{Affine, BezPath};
use rustc_hash::FxHashMap;
use skrifa::instance::{LocationRef, NormalizedCoord, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{FontRef, GlyphId, MetadataProvider};
use text::GlyphRun;

/// Outlines by (font blob, index, coordinates, size, glyph), in pixels at the run's size,
/// y down, synthetic slant applied, origin at the glyph's origin.
#[derive(Default)]
pub struct Outlines {
    cache: FxHashMap<GlyphKey, Option<Arc<BezPath>>>,
}

/// Font blob, font index, coordinates, size bits, glyph.
type GlyphKey = (u64, u32, Vec<i16>, u32, u32);

struct Pen(BezPath);

impl OutlinePen for Pen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.move_to((f64::from(x), -f64::from(y)));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.line_to((f64::from(x), -f64::from(y)));
    }
    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.0.quad_to((f64::from(cx0), -f64::from(cy0)), (f64::from(x), -f64::from(y)));
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.0.curve_to((f64::from(cx0), -f64::from(cy0)), (f64::from(cx1), -f64::from(cy1)), (f64::from(x), -f64::from(y)));
    }
    fn close(&mut self) {
        self.0.close_path();
    }
}

/// The synthetic slant the renderers apply to glyph outlines (y down).
pub fn skew(run: &GlyphRun) -> Option<Affine> {
    run.skew.map(|deg| Affine::skew((-f64::from(deg)).to_radians().tan(), 0.0))
}

impl Outlines {
    pub fn glyph(&mut self, run: &GlyphRun, gid: u32) -> Option<Arc<BezPath>> {
        let key = (run.font.data.id(), run.font.index, run.coords.clone(), run.size.to_bits(), gid);
        self.cache
            .entry(key)
            .or_insert_with(|| {
                let font = FontRef::from_index(run.font.data.data(), run.font.index).ok()?;
                let glyph = font.outline_glyphs().get(GlyphId::new(gid))?;
                let coords: Vec<NormalizedCoord> = run.coords.iter().map(|c| NormalizedCoord::from_bits(*c)).collect();
                let mut pen = Pen(BezPath::new());
                glyph.draw(DrawSettings::unhinted(Size::new(run.size), LocationRef::new(&coords)), &mut pen).ok()?;
                let mut p = pen.0;
                if let Some(k) = skew(run) {
                    p.apply_affine(k);
                }
                Some(Arc::new(p))
            })
            .clone()
    }

    /// All of a run's glyphs as one path, in the run's local units.
    pub fn run(&mut self, run: &GlyphRun) -> BezPath {
        let mut out = BezPath::new();
        for &(gid, x, y) in &run.glyphs {
            if let Some(g) = self.glyph(run, gid) {
                let at = Affine::translate((f64::from(x), f64::from(y)));
                out.extend(g.iter().map(|e| at * e));
            }
        }
        out
    }
}
