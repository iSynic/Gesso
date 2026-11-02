//! PDF 1.7 from display lists: one page, vector paths, gradients as shadings, images as image
//! XObjects with soft masks, opacity and blend modes as transparency groups, and text in
//! embedded subset fonts (instanced at the run's variation coordinates) with ToUnicode maps,
//! so it can be selected and searched.
//!
//! Units: one export unit is `scale` points. The page's base transform flips y, so content is
//! written in export space (y down). Every `cm` sits inside `q … Q`, so transparency groups
//! (form XObjects) are written in export space too.
//!
//! Rasterised at the export scale and embedded as images: effects, background blur, blurred
//! rectangles, sweep gradients, gradients with varying transparency or repeating extends, and
//! repeating image fills.

use std::collections::BTreeMap;
use std::io::Write as _;

use kurbo::{Affine, BezPath, PathEl, Rect, Shape};
use pdf_writer::types::{CidFontType, FontFlags, FunctionShadingType, LineCapStyle, LineJoinStyle, SystemInfo, TextRenderingMode, UnicodeCmap};
use pdf_writer::{Chunk, Content, Filter, Finish, Name, Pdf, Ref, Str};
use peniko::color::Srgb;
use peniko::{BlendMode, Extend, Fill, Gradient, GradientKind, ImageQuality, Mix};
use render::{Brush, Cmd};
use rustc_hash::{FxHashMap, FxHashSet};
use skrifa::instance::{LocationRef, NormalizedCoord, Size};
use skrifa::raw::TableProvider;
use skrifa::string::StringId;
use skrifa::{FontRef, GlyphId, MetadataProvider, Tag};
use text::GlyphRun;

use rayon::prelude::*;

use crate::glyphs::Outlines;
use crate::raster;
use crate::{Content as Export, ExportError, Item};

fn deflate(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
    let _ = e.write_all(data);
    e.finish().unwrap_or_default()
}

fn coeffs(xf: Affine) -> [f32; 6] {
    xf.as_coeffs().map(|v| v as f32)
}

fn path(c: &mut Content, p: &BezPath) {
    let mut last = kurbo::Point::ZERO;
    for el in p.elements() {
        match *el {
            PathEl::MoveTo(a) => {
                c.move_to(a.x as f32, a.y as f32);
                last = a;
            }
            PathEl::LineTo(a) => {
                c.line_to(a.x as f32, a.y as f32);
                last = a;
            }
            PathEl::QuadTo(q, a) => {
                // Quadratic → cubic.
                let c1 = last + (q - last) * (2.0 / 3.0);
                let c2 = a + (q - a) * (2.0 / 3.0);
                c.cubic_to(c1.x as f32, c1.y as f32, c2.x as f32, c2.y as f32, a.x as f32, a.y as f32);
                last = a;
            }
            PathEl::CurveTo(a, b, d) => {
                c.cubic_to(a.x as f32, a.y as f32, b.x as f32, b.y as f32, d.x as f32, d.y as f32);
                last = d;
            }
            PathEl::ClosePath => {
                c.close_path();
            }
        }
    }
}

fn blend(b: BlendMode) -> Option<pdf_writer::types::BlendMode> {
    use pdf_writer::types::BlendMode as B;
    Some(match b.mix {
        Mix::Normal => return None,
        Mix::Multiply => B::Multiply,
        Mix::Screen => B::Screen,
        Mix::Overlay => B::Overlay,
        Mix::Darken => B::Darken,
        Mix::Lighten => B::Lighten,
        Mix::ColorDodge => B::ColorDodge,
        Mix::ColorBurn => B::ColorBurn,
        Mix::HardLight => B::HardLight,
        Mix::SoftLight => B::SoftLight,
        Mix::Difference => B::Difference,
        Mix::Exclusion => B::Exclusion,
        Mix::Hue => B::Hue,
        Mix::Saturation => B::Saturation,
        Mix::Color => B::Color,
        Mix::Luminosity => B::Luminosity,
        #[allow(unreachable_patterns, deprecated)]
        _ => return None,
    })
}

/// The stops' colours (opaque) and their one shared alpha, if the gradient is expressible as a
/// PDF shading: linear or radial, padded, and uniformly transparent.
/// `(offset, rgb)` gradient stops.
type Stops = Vec<(f32, [f32; 3])>;

fn shading_stops(g: &Gradient) -> Option<(Stops, f32)> {
    if g.extend != Extend::Pad || matches!(g.kind, GradientKind::Sweep(_)) || g.stops.is_empty() {
        return None;
    }
    let stops: Vec<(f32, [f32; 4])> = g.stops.iter().map(|s| (s.offset, s.color.to_alpha_color::<Srgb>().components)).collect();
    let a = stops[0].1[3];
    if stops.iter().any(|s| (s.1[3] - a).abs() > 1.0 / 512.0) {
        return None;
    }
    Some((stops.into_iter().map(|(o, c)| (o, [c[0], c[1], c[2]])).collect(), a))
}

/// A brush PDF can paint directly (else it is rasterised).
fn vector_brush(b: &Brush) -> bool {
    match b {
        Brush::Solid(_) => true,
        Brush::Gradient(g) => shading_stops(g).is_some(),
        Brush::Image(ip) => ip.extend == Extend::Pad,
    }
}

/// One embedded font: a font file at one variation instance.
struct FontUse {
    name: String,
    run: GlyphRun,
    remap: subsetter::GlyphRemapper,
    /// New glyph id → text.
    text: BTreeMap<u16, String>,
    /// New glyph id → advance in thousandths of the size.
    widths: BTreeMap<u16, f32>,
    upem: f32,
}

type FontKey = (u64, u32, Vec<i16>);

/// Variation coordinates in user units, recovered from the normalized ones (normalization,
/// with `avar`, is monotonic per axis).
fn user_coords(font: &FontRef<'_>, coords: &[i16]) -> Vec<(Tag, f32)> {
    let axes = font.axes();
    let mut out = Vec::new();
    for (i, axis) in axes.iter().enumerate() {
        let want = coords.get(i).copied().unwrap_or(0);
        let norm = |v: f32| -> i16 {
            let loc = axes.location([(axis.tag(), v)]);
            loc.coords().get(i).map_or(0, |c| c.to_bits())
        };
        let (mut lo, mut hi) = (axis.min_value(), axis.max_value());
        for _ in 0..40 {
            let mid = (lo + hi) / 2.0;
            if norm(mid) < want { lo = mid } else { hi = mid }
        }
        out.push((subsetter::Tag::new(&axis.tag().to_be_bytes()), (lo + hi) / 2.0));
    }
    out.into_iter().map(|(t, v)| (Tag::new(t.get()), v)).collect()
}

/// A transparency group being written: its content, and its clip (export space), rule,
/// blend and alpha.
struct Group {
    content: Content,
    layer: Option<(BezPath, Fill, BlendMode, f32)>,
}

struct Writer<'c> {
    export: &'c Export,
    scale: f64,
    chunk: Chunk,
    next: Ref,
    stack: Vec<Group>,
    /// Open layers, innermost last: `true` for a transparency group, `false` for a plain clip
    /// (`q … Q` in the current content).
    open: Vec<bool>,
    /// Form XObjects (name, ref, content, bbox), written at the end with the shared resources.
    forms: Vec<(Ref, Vec<u8>, Rect)>,
    xobjects: Vec<(String, Ref)>,
    images: FxHashMap<u64, String>,
    states: FxHashMap<(u32, u32, Option<u8>), String>,
    state_refs: Vec<(String, Ref)>,
    shadings: Vec<(String, Ref)>,
    /// Font instances seen, and their use (`None`: can't be subset, drawn as outlines).
    fonts: FxHashMap<FontKey, Option<usize>>,
    font_uses: Vec<FontUse>,
    outlines: Outlines,
    /// Effect regions by (item, command index), rendered ahead in parallel: straight RGBA, its
    /// size and the snapped rectangle it covers (`None` when empty).
    pre: FxHashMap<(usize, usize), Rendered>,
    /// Blurred-rect shadows rendered so far, by look and pixel offset.
    blurred: FxHashMap<BlurKey, String>,
    /// Distinct blurred-rect shadows rendered ahead in parallel.
    blurred_pre: FxHashMap<BlurKey, Rendered>,
    error: Option<ExportError>,
}

type Pixels = (Vec<u8>, u32, u32, Rect);
type Rendered = Result<Option<Pixels>, ExportError>;

/// A blurred-rect shadow's look and pixel offset: equal keys render equal pixels.
type BlurKey = [i64; 13];

/// The key and snapped export-space area of a blurred-rect shadow (`None` when off the page).
fn blurred_key(export: &Export, scale: f64, rect: Rect, radius: f64, sigma: f64, color: peniko::Color, full: Affine) -> Option<(BlurKey, Rect)> {
    let m = 3.0 * sigma;
    let r = raster::snap(export, full.transform_rect_bbox(rect.inflate(m, m)), scale);
    if r.width() <= 0.0 || r.height() <= 0.0 {
        return None;
    }
    let q = |v: f64| (v * scale * 256.0).round() as i64;
    let at = full * rect.origin() - r.origin();
    let c = full.as_coeffs();
    let key = [
        q(rect.width()), q(rect.height()), q(radius), q(sigma), i64::from(u32::from_ne_bytes(color.to_rgba8().to_u8_array())),
        q(c[0]), q(c[1]), q(c[2]), q(c[3]), q(at.x), q(at.y), q(r.width()), q(r.height()),
    ];
    Some((key, r))
}

impl Writer<'_> {
    fn alloc(&mut self) -> Ref {
        self.next.bump()
    }

    fn c(&mut self) -> &mut Content {
        let n = self.stack.len() - 1;
        &mut self.stack[n].content
    }

    /// An ExtGState with fill alpha, stroke alpha and blend mode.
    fn state(&mut self, fill: f32, stroke: f32, mode: Option<pdf_writer::types::BlendMode>) -> String {
        let key = (fill.to_bits(), stroke.to_bits(), mode.map(|m| m as u8));
        if let Some(n) = self.states.get(&key) {
            return n.clone();
        }
        let id = self.alloc();
        let name = format!("G{}", self.state_refs.len());
        let mut gs = self.chunk.ext_graphics(id);
        gs.non_stroking_alpha(fill).stroking_alpha(stroke);
        if let Some(m) = mode {
            gs.blend_mode(m);
        }
        gs.finish();
        self.states.insert(key, name.clone());
        self.state_refs.push((name.clone(), id));
        name
    }

    fn set_alpha(&mut self, fill: f32, stroke: f32) {
        if fill < 1.0 || stroke < 1.0 {
            let g = self.state(fill, stroke, None);
            self.c().set_parameters(Name(g.as_bytes()));
        }
    }

    /// An image XObject from straight-alpha RGBA, with a soft mask if it has transparency.
    fn image(&mut self, rgba: &[u8], w: u32, h: u32, smooth: bool) -> String {
        let rgb: Vec<u8> = rgba.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
        let alpha: Vec<u8> = rgba.as_chunks::<4>().0.iter().map(|p| p[3]).collect();
        let mask = alpha.iter().any(|a| *a < 255).then(|| {
            let id = self.alloc();
            let data = deflate(&alpha);
            let mut m = self.chunk.image_xobject(id, &data);
            m.width(w as i32).height(h as i32).bits_per_component(8).interpolate(smooth);
            m.filter(Filter::FlateDecode);
            m.color_space().device_gray();
            m.finish();
            id
        });
        let id = self.alloc();
        let data = deflate(&rgb);
        let mut im = self.chunk.image_xobject(id, &data);
        im.width(w as i32).height(h as i32).bits_per_component(8).interpolate(smooth);
        im.filter(Filter::FlateDecode);
        im.color_space().device_rgb();
        if let Some(m) = mask {
            im.s_mask(m);
        }
        im.finish();
        let name = format!("X{}", self.xobjects.len());
        self.xobjects.push((name.clone(), id));
        name
    }

    /// Draws an image XObject over the export-space rectangle `r`.
    fn place(&mut self, name: &str, r: Rect) {
        let c = self.c();
        c.save_state();
        c.transform([r.width() as f32, 0.0, 0.0, -r.height() as f32, r.x0 as f32, r.y1 as f32]);
        c.x_object(Name(name.as_bytes()));
        c.restore_state();
    }

    /// Renders every effect region (shadows, blurs) and distinct blurred-rect shadow in parallel, each on one thread, before
    /// the sequential writing pass. Backdrop blurs need what is drawn under them, so they stay
    /// in the pass.
    fn prerender(&mut self) {
        let (export, scale) = (self.export, self.scale);
        let (mut jobs, mut blurs, mut seen) = (Vec::new(), Vec::new(), FxHashSet::default());
        for (index, it) in export.items.iter().enumerate() {
            let cmds = &it.list.cmds;
            let mut i = 0;
            while i < cmds.len() {
                match &cmds[i] {
                    Cmd::PushEffect { bounds, .. } => {
                        let end = raster::effect_end(cmds, i);
                        let r = raster::snap(export, it.xf.transform_rect_bbox(*bounds), scale);
                        jobs.push(((index, i), (index, i, end), r));
                        i = end;
                    }
                    Cmd::BlurredRect { rect, radius, sigma, color, xf: l } => {
                        if let Some((key, r)) = blurred_key(export, scale, *rect, *radius, *sigma, *color, it.xf * *l)
                            && seen.insert(key)
                        {
                            blurs.push((key, (index, i), r));
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
        }
        let render_one = |index: usize, from: usize, end: usize, r: Rect| -> Rendered {
            let it = &export.items[index];
            let region = raster::item(it.list.cmds[from..=end].to_vec(), it.xf);
            let px = raster::region_with(&[region], r, scale, &mut render_cpu::CpuRenderer::new(0))?;
            Ok(Some((raster::rgba(&px), u32::from(px.width()), u32::from(px.height()), r)))
        };
        self.blurred_pre = blurs.into_par_iter().map(|(key, (index, i), r)| (key, render_one(index, i, i, r))).collect::<Vec<_>>().into_iter().collect();
        let done: Vec<((usize, usize), Rendered)> = jobs
            .into_par_iter()
            .map(|(key, (index, from, end), r)| {
                if r.width() <= 0.0 || r.height() <= 0.0 {
                    return (key, Ok(None));
                }
                (key, render_one(index, from, end, r))
            })
            .collect();
        self.pre = done.into_iter().collect();
    }

    /// Embeds pixels rendered for the export-space rectangle `r`.
    fn raster(&mut self, items: &[Item], r: Rect) {
        let r = raster::snap(self.export, r, self.scale);
        if r.width() <= 0.0 || r.height() <= 0.0 {
            return;
        }
        match raster::region(items, r, self.scale) {
            Ok(px) => {
                let name = self.image(&raster::rgba(&px), px.width() as u32, px.height() as u32, true);
                self.place(&name, r);
            }
            Err(e) => self.error = Some(e),
        }
    }

    fn function(&mut self, stops: &[(f32, [f32; 3])]) -> Ref {
        let exp = |w: &mut Self, a: [f32; 3], b: [f32; 3]| {
            let id = w.alloc();
            w.chunk.exponential_function(id).domain([0.0, 1.0]).c0(a).c1(b).n(1.0);
            id
        };
        if stops.len() == 1 {
            return exp(self, stops[0].1, stops[0].1);
        }
        let mut stops = stops.to_vec();
        // Stops cover 0..1; equal offsets (hard edges) become a tiny step.
        if stops[0].0 > 0.0 {
            stops.insert(0, (0.0, stops[0].1));
        }
        if let Some(l) = stops.last().copied()
            && l.0 < 1.0
        {
            stops.push((1.0, l.1));
        }
        for i in 1..stops.len() {
            if stops[i].0 <= stops[i - 1].0 {
                stops[i].0 = (stops[i - 1].0 + 1e-5).min(1.0);
            }
        }
        let parts: Vec<Ref> = stops.windows(2).map(|w| (w[0].1, w[1].1)).collect::<Vec<_>>().into_iter().map(|(a, b)| exp(self, a, b)).collect();
        let bounds: Vec<f32> = stops[1..stops.len() - 1].iter().map(|s| s.0).collect();
        let id = self.alloc();
        self.chunk
            .stitching_function(id)
            .domain([0.0, 1.0])
            .functions(parts.iter().copied())
            .bounds(bounds)
            .encode(std::iter::repeat_n([0.0, 1.0], parts.len()).flatten());
        id
    }

    fn shading(&mut self, g: &Gradient, stops: &[(f32, [f32; 3])]) -> String {
        let f = self.function(stops);
        let id = self.alloc();
        let mut sh = self.chunk.function_shading(id);
        match g.kind {
            GradientKind::Linear(l) => {
                sh.shading_type(FunctionShadingType::Axial).coords([l.start.x as f32, l.start.y as f32, l.end.x as f32, l.end.y as f32]);
            }
            GradientKind::Radial(r) => {
                sh.shading_type(FunctionShadingType::Radial).coords([
                    r.start_center.x as f32,
                    r.start_center.y as f32,
                    r.start_radius,
                    r.end_center.x as f32,
                    r.end_center.y as f32,
                    r.end_radius,
                ]);
            }
            GradientKind::Sweep(_) => {}
        }
        sh.color_space().device_rgb();
        sh.function(f).extend([true, true]);
        sh.finish();
        let name = format!("S{}", self.shadings.len());
        self.shadings.push((name.clone(), id));
        name
    }

    /// Paints a non-solid brush over the current clip, in local space (`cm` already applied).
    fn paint_clipped(&mut self, brush: &Brush, brush_xf: Affine) {
        match brush {
            Brush::Gradient(g) => {
                let Some((stops, a)) = shading_stops(g) else { return };
                let name = self.shading(g, &stops);
                self.set_alpha(a, 1.0);
                let c = self.c();
                c.transform(coeffs(brush_xf));
                c.shading(Name(name.as_bytes()));
            }
            Brush::Image(ip) => {
                let name = match self.images.get(&ip.image.key) {
                    Some(n) => n.clone(),
                    None => {
                        let n = self.image(ip.image.rgba.data(), ip.image.width, ip.image.height, ip.quality != ImageQuality::Low);
                        self.images.insert(ip.image.key, n.clone());
                        n
                    }
                };
                self.set_alpha(ip.alpha, 1.0);
                let (w, h) = (ip.image.width as f32, ip.image.height as f32);
                let c = self.c();
                c.transform(coeffs(brush_xf));
                c.transform([w, 0.0, 0.0, -h, 0.0, h]);
                c.x_object(Name(name.as_bytes()));
            }
            Brush::Solid(_) => {}
        }
    }

    fn fill(&mut self, p: &BezPath, rule: Fill, full: Affine, brush: &Brush, brush_xf: Affine) {
        self.c().save_state();
        self.c().transform(coeffs(full));
        if let Brush::Solid(col) = brush {
            let [r, g, b, a] = col.components;
            self.set_alpha(a, 1.0);
            let c = self.c();
            c.set_fill_rgb(r, g, b);
            path(c, p);
            if rule == Fill::EvenOdd { c.fill_even_odd() } else { c.fill_nonzero() };
        } else {
            let c = self.c();
            path(c, p);
            if rule == Fill::EvenOdd { c.clip_even_odd() } else { c.clip_nonzero() };
            c.end_path();
            self.paint_clipped(brush, brush_xf);
        }
        self.c().restore_state();
    }

    fn font(&mut self, run: &GlyphRun) -> Option<usize> {
        let key = (run.font.data.id(), run.font.index, run.coords.clone());
        if let Some(i) = self.fonts.get(&key) {
            return *i;
        }
        let i = self.embeddable(run);
        self.fonts.insert(key, i);
        i
    }

    /// A new font use for `run`'s font instance, if it can be subset (else its text becomes
    /// outlines). Checked once per instance: the probe costs a full subset.
    fn embeddable(&mut self, run: &GlyphRun) -> Option<usize> {
        let data = run.font.data.data();
        let font = FontRef::from_index(data, run.font.index).ok()?;
        let vars = user_coords(&font, &run.coords);
        let probe = subsetter::GlyphRemapper::new();
        let tags: Vec<(subsetter::Tag, f32)> = vars.iter().map(|(t, v)| (subsetter::Tag::new(&t.to_be_bytes()), *v)).collect();
        let ok = if tags.is_empty() { subsetter::subset(data, run.font.index, &probe) } else { subsetter::subset_with_variations(data, run.font.index, &tags, &probe) };
        ok.ok()?;
        let upem = font.head().ok()?.units_per_em() as f32;
        let i = self.font_uses.len();
        self.font_uses.push(FontUse { name: format!("F{i}"), run: GlyphRun { glyphs: Vec::new(), chars: Vec::new(), ..run.clone() }, remap: subsetter::GlyphRemapper::new(), text: BTreeMap::new(), widths: BTreeMap::new(), upem });
        Some(i)
    }

    fn glyphs(&mut self, run: &GlyphRun, full: Affine, brush: &Brush, brush_xf: Affine) {
        let Some(fi) = self.font(run) else {
            // Not embeddable: outlines.
            let p = self.outlines.run(run);
            return self.fill(&p, Fill::NonZero, full, brush, brush_xf);
        };
        // Glyph ids in the subset, widths and text.
        let font = FontRef::from_index(run.font.data.data(), run.font.index).ok();
        let coords: Vec<NormalizedCoord> = run.coords.iter().map(|c| NormalizedCoord::from_bits(*c)).collect();
        let metrics = font.as_ref().map(|f| f.glyph_metrics(Size::unscaled(), LocationRef::new(&coords)));
        let fu = &mut self.font_uses[fi];
        let mut cids = Vec::with_capacity(run.glyphs.len());
        for &(gid, x, y) in &run.glyphs {
            let new = fu.remap.remap(u16::try_from(gid).unwrap_or(0));
            let adv = metrics.as_ref().and_then(|m| m.advance_width(GlyphId::new(gid))).unwrap_or(0.0);
            fu.widths.insert(new, adv / fu.upem * 1000.0);
            cids.push((new, x, y));
        }
        for (gid, t) in &run.chars {
            let new = fu.remap.remap(u16::try_from(*gid).unwrap_or(0));
            fu.text.entry(new).or_insert_with(|| t.to_string());
        }
        let name = fu.name.clone();
        let widths = fu.widths.clone();
        let k = crate::glyphs::skew(run).map_or(0.0, |s| s.as_coeffs()[2]) as f32;

        self.c().save_state();
        self.c().transform(coeffs(full));
        let solid = if let Brush::Solid(col) = brush {
            let [r, g, b, a] = col.components;
            self.set_alpha(a, 1.0);
            self.c().set_fill_rgb(r, g, b);
            true
        } else {
            false
        };
        let size = run.size;
        let c = self.c();
        c.begin_text();
        c.set_font(Name(name.as_bytes()), size);
        if !solid {
            c.set_text_rendering_mode(TextRenderingMode::Clip);
        }
        // One TJ per baseline, adjusting each advance to the laid-out position.
        let mut i = 0;
        while i < cids.len() {
            let (_, x0, y0) = cids[i];
            let mut j = i + 1;
            while j < cids.len() && (cids[j].2 - y0).abs() < 1e-3 {
                j += 1;
            }
            c.set_text_matrix([1.0, 0.0, k, -1.0, x0, y0]);
            let mut tj = c.show_positioned();
            let mut items = tj.items();
            for n in i..j {
                let (cid, x, _) = cids[n];
                items.show(Str(&cid.to_be_bytes()));
                if n + 1 < j {
                    let adv = widths.get(&cid).copied().unwrap_or(0.0) / 1000.0 * size;
                    let dx = cids[n + 1].1 - x;
                    let adj = -(dx - adv) * 1000.0 / size;
                    if adj.abs() > 1e-3 {
                        items.adjust(adj);
                    }
                }
            }
            items.finish();
            tj.finish();
            i = j;
        }
        c.end_text();
        if !solid {
            self.paint_clipped(brush, brush_xf);
        }
        self.c().restore_state();
    }

    fn stroke(&mut self, p: &BezPath, style: &kurbo::Stroke, full: Affine, brush: &Brush, brush_xf: Affine) {
        let Brush::Solid(col) = brush else {
            let outline = kurbo::stroke(p.iter(), style, &kurbo::StrokeOpts::default(), 0.01);
            return self.fill(&outline, Fill::NonZero, full, brush, brush_xf);
        };
        let [r, g, b, a] = col.components;
        self.c().save_state();
        self.c().transform(coeffs(full));
        self.set_alpha(1.0, a);
        let c = self.c();
        c.set_stroke_rgb(r, g, b);
        c.set_line_width(style.width as f32);
        c.set_line_cap(match style.start_cap {
            kurbo::Cap::Butt => LineCapStyle::ButtCap,
            kurbo::Cap::Round => LineCapStyle::RoundCap,
            kurbo::Cap::Square => LineCapStyle::ProjectingSquareCap,
        });
        c.set_line_join(match style.join {
            kurbo::Join::Miter => LineJoinStyle::MiterJoin,
            kurbo::Join::Round => LineJoinStyle::RoundJoin,
            kurbo::Join::Bevel => LineJoinStyle::BevelJoin,
        });
        c.set_miter_limit(style.miter_limit.max(1.0) as f32);
        if !style.dash_pattern.is_empty() {
            c.set_dash_pattern(style.dash_pattern.iter().map(|v| *v as f32), style.dash_offset as f32);
        }
        path(c, p);
        c.stroke();
        c.restore_state();
    }

    fn shape(&mut self, cmd: &Cmd, xf: Affine) {
        let raster_one = |w: &mut Self, r: Rect| w.raster(&[raster::item(vec![cmd.clone()], xf)], r);
        match cmd {
            Cmd::Fill { path: p, rule, xf: l, brush, brush_xf } => {
                if !vector_brush(brush) {
                    return raster_one(self, (xf * *l).transform_rect_bbox(p.bounding_box()));
                }
                self.fill(p, *rule, xf * *l, brush, *brush_xf);
            }
            Cmd::Stroke { path: p, style, xf: l, brush, brush_xf } => {
                if !vector_brush(brush) {
                    let m = style.width * style.miter_limit.max(1.0);
                    return raster_one(self, (xf * *l).transform_rect_bbox(p.bounding_box().inflate(m, m)));
                }
                self.stroke(p, style, xf * *l, brush, *brush_xf);
            }
            Cmd::Glyphs { run, xf: l, brush, brush_xf } => {
                if !vector_brush(brush) {
                    let p = self.outlines.run(run);
                    return raster_one(self, (xf * *l).transform_rect_bbox(p.bounding_box()));
                }
                self.glyphs(run, xf * *l, brush, *brush_xf);
            }
            Cmd::BlurredRect { rect, radius, sigma, color, xf: l } => {
                // Rendered once per distinct look and pixel offset, then reused (cards share
                // shadows).
                let Some((key, r)) = blurred_key(self.export, self.scale, *rect, *radius, *sigma, *color, xf * *l) else { return };
                if let Some(name) = self.blurred.get(&key).cloned() {
                    self.place(&name, r);
                    return;
                }
                let px = match self.blurred_pre.remove(&key) {
                    Some(p) => p,
                    None => raster::region(&[raster::item(vec![cmd.clone()], xf)], r, self.scale).map(|px| Some((raster::rgba(&px), u32::from(px.width()), u32::from(px.height()), r))),
                };
                match px {
                    Ok(Some((rgba, w, h, _))) => {
                        let name = self.image(&rgba, w, h, true);
                        self.place(&name, r);
                        self.blurred.insert(key, name);
                    }
                    Ok(None) => {}
                    Err(e) => self.error = Some(e),
                }
            }
            _ => {}
        }
    }

    fn push_layer(&mut self, clip: &BezPath, rule: Fill, full: Affine, mode: BlendMode, alpha: f32) {
        let mut p = clip.clone();
        p.apply_affine(full);
        if alpha >= 1.0 && blend(mode).is_none() {
            let c = self.c();
            c.save_state();
            path(c, &p);
            if rule == Fill::EvenOdd { c.clip_even_odd() } else { c.clip_nonzero() };
            c.end_path();
            self.open.push(false);
        } else {
            self.stack.push(Group { content: Content::new(), layer: Some((p, rule, mode, alpha)) });
            self.open.push(true);
        }
    }

    fn pop(&mut self) {
        match self.open.pop() {
            None => {}
            Some(false) => {
                self.c().restore_state();
            }
            Some(true) => {
                let Some(g) = self.stack.pop() else { return };
                let Some((p, rule, mode, alpha)) = g.layer else { return };
                let body = g.content.finish();
                let id = self.alloc();
                let bbox = p.bounding_box();
                self.forms.push((id, body.into_vec(), bbox));
                let name = format!("X{}", self.xobjects.len());
                self.xobjects.push((name.clone(), id));
                let gs = self.state(alpha, alpha, blend(mode));
                let c = self.c();
                c.save_state();
                path(c, &p);
                if rule == Fill::EvenOdd { c.clip_even_odd() } else { c.clip_nonzero() };
                c.end_path();
                c.set_parameters(Name(gs.as_bytes()));
                c.x_object(Name(name.as_bytes()));
                c.restore_state();
            }
        }
    }

    fn item(&mut self, index: usize) {
        let it = self.export.items[index].clone();
        let xf = it.xf;
        let cmds = &it.list.cmds;
        let depth = self.open.len();
        let mut open: Vec<&Cmd> = Vec::new();
        let mut i = 0;
        while i < cmds.len() {
            let c = &cmds[i];
            match c {
                Cmd::PushLayer { clip, rule, xf: l, blend, alpha } => {
                    self.push_layer(clip, *rule, xf * *l, *blend, *alpha);
                    open.push(c);
                }
                Cmd::PopLayer => {
                    if open.pop().is_some() {
                        self.pop();
                    }
                }
                Cmd::PushEffect { bounds, .. } => {
                    // The whole effect region (content and result) as one image.
                    let end = raster::effect_end(cmds, i);
                    match self.pre.remove(&(index, i)) {
                        Some(Ok(Some((rgba, w, h, r)))) => {
                            let name = self.image(&rgba, w, h, true);
                            self.place(&name, r);
                        }
                        Some(Ok(None)) => {}
                        Some(Err(e)) => self.error = Some(e),
                        None => {
                            let region = raster::item(cmds[i..=end].to_vec(), xf);
                            self.raster(&[region], xf.transform_rect_bbox(*bounds));
                        }
                    }
                    i = end;
                }
                Cmd::PopEffect => {}
                Cmd::BackdropBlur { .. } => {
                    if let Some((items, r, clip, full)) = raster::backdrop(self.export, index, i, &open) {
                        self.push_layer(&clip, Fill::NonZero, full, BlendMode::default(), 1.0);
                        self.raster(&items, r);
                        self.pop();
                    }
                }
                other => self.shape(other, xf),
            }
            i += 1;
        }
        while self.open.len() > depth {
            self.pop();
        }
    }


    /// Writes the embedded fonts; returns their resource names and refs.
    fn write_fonts(&mut self) -> Result<Vec<(String, Ref)>, ExportError> {
        let uses = std::mem::take(&mut self.font_uses);
        let mut out = Vec::new();
        for fu in uses {
            let data = fu.run.font.data.data();
            let font = FontRef::from_index(data, fu.run.font.index).map_err(|e| ExportError::Encode(format!("font: {e}")))?;
            let vars: Vec<(subsetter::Tag, f32)> = user_coords(&font, &fu.run.coords).iter().map(|(t, v)| (subsetter::Tag::new(&t.to_be_bytes()), *v)).collect();
            let sub = if vars.is_empty() { subsetter::subset(data, fu.run.font.index, &fu.remap) } else { subsetter::subset_with_variations(data, fu.run.font.index, &vars, &fu.remap) }
                .map_err(|e| ExportError::Encode(format!("font subset: {e}")))?;
            let cff = sub.starts_with(b"OTTO");
            let ps = font
                .localized_strings(StringId::POSTSCRIPT_NAME)
                .english_or_first()
                .map(|s| s.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect::<String>())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "Font".into());
            // A subset tag: six capitals derived from the font and its glyphs.
            let mut h: u64 = 0xcbf2_9ce4_8422_2325;
            for b in fu.run.font.data.id().to_le_bytes().iter().chain(fu.run.coords.iter().flat_map(|c| c.to_le_bytes()).collect::<Vec<_>>().iter()).chain(fu.widths.keys().flat_map(|k| k.to_le_bytes()).collect::<Vec<_>>().iter()) {
                h = (h ^ u64::from(*b)).wrapping_mul(0x100_0000_01b3);
            }
            let tag: String = (0..6).map(|i| (b'A' + ((h >> (i * 5)) % 26) as u8) as char).collect();
            let base = format!("{tag}+{ps}");

            let (type0, cid, desc, file, cmap) = (self.alloc(), self.alloc(), self.alloc(), self.alloc(), self.alloc());
            self.chunk.type0_font(type0).base_font(Name(base.as_bytes())).encoding_predefined(Name(b"Identity-H")).descendant_font(cid).to_unicode(cmap);
            let mut cf = self.chunk.cid_font(cid);
            cf.subtype(if cff { CidFontType::Type0 } else { CidFontType::Type2 });
            cf.base_font(Name(base.as_bytes()));
            cf.system_info(SystemInfo { registry: Str(b"Adobe"), ordering: Str(b"Identity"), supplement: 0 });
            cf.font_descriptor(desc);
            cf.default_width(0.0);
            if !cff {
                cf.cid_to_gid_map_predefined(Name(b"Identity"));
            }
            let mut w = cf.widths();
            for (g, v) in &fu.widths {
                w.consecutive(*g, [*v]);
            }
            w.finish();
            cf.finish();

            let m = font.metrics(Size::unscaled(), LocationRef::default());
            let k = 1000.0 / fu.upem;
            let bb = m.bounds.map_or(pdf_writer::Rect::new(0.0, -200.0, 1000.0, 900.0), |b| pdf_writer::Rect::new(b.x_min * k, b.y_min * k, b.x_max * k, b.y_max * k));
            let mut flags = FontFlags::SYMBOLIC;
            if m.italic_angle != 0.0 {
                flags |= FontFlags::ITALIC;
            }
            let mut d = self.chunk.font_descriptor(desc);
            d.name(Name(base.as_bytes()))
                .flags(flags)
                .bbox(bb)
                .italic_angle(m.italic_angle)
                .ascent(m.ascent * k)
                .descent(m.descent * k)
                .cap_height(m.cap_height.unwrap_or(m.ascent) * k)
                .stem_v(80.0);
            if cff { d.font_file3(file) } else { d.font_file2(file) };
            d.finish();
            let packed = deflate(&sub);
            let mut s = self.chunk.stream(file, &packed);
            s.filter(Filter::FlateDecode);
            if cff {
                s.pair(Name(b"Subtype"), Name(b"OpenType"));
            }
            s.finish();

            let mut u = UnicodeCmap::new(Name(b"Custom"), SystemInfo { registry: Str(b"Adobe"), ordering: Str(b"UCS"), supplement: 0 });
            for (g, t) in &fu.text {
                if !t.is_empty() {
                    u.pair_with_multiple(*g, t.chars());
                }
            }
            let cm = u.finish();
            self.chunk.cmap(cmap, cm.as_slice());
            out.push((fu.name.clone(), type0));
        }
        Ok(out)
    }
}

/// Writes `content` as a one-page PDF, `scale` points per unit.
pub fn write(content: &Export, scale: f64) -> Result<Vec<u8>, ExportError> {
    let mut w = Writer {
        export: content,
        scale,
        chunk: Chunk::new(),
        next: Ref::new(10),
        stack: vec![Group { content: Content::new(), layer: None }],
        open: Vec::new(),
        forms: Vec::new(),
        xobjects: Vec::new(),
        images: FxHashMap::default(),
        states: FxHashMap::default(),
        state_refs: Vec::new(),
        shadings: Vec::new(),
        fonts: FxHashMap::default(),
        font_uses: Vec::new(),
        outlines: Outlines::default(),
        pre: FxHashMap::default(),
        blurred: FxHashMap::default(),
        blurred_pre: FxHashMap::default(),
        error: None,
    };
    w.prerender();
    let (pw, ph) = ((content.width * scale) as f32, (content.height * scale) as f32);
    w.c().transform([scale as f32, 0.0, 0.0, -scale as f32, 0.0, ph]);
    for i in 0..content.items.len() {
        w.item(i);
    }
    if let Some(e) = w.error.take() {
        return Err(e);
    }
    let fonts = w.write_fonts()?;
    let resources = w.alloc();
    {
        let mut r = w.chunk.indirect(resources).start::<pdf_writer::writers::Resources>();
        let mut x = r.x_objects();
        for (n, id) in &w.xobjects {
            x.pair(Name(n.as_bytes()), *id);
        }
        x.finish();
        let mut g = r.ext_g_states();
        for (n, id) in &w.state_refs {
            g.pair(Name(n.as_bytes()), *id);
        }
        g.finish();
        let mut s = r.shadings();
        for (n, id) in &w.shadings {
            s.pair(Name(n.as_bytes()), *id);
        }
        s.finish();
        let mut f = r.fonts();
        for (n, id) in &fonts {
            f.pair(Name(n.as_bytes()), *id);
        }
        f.finish();
        r.finish();
    }
    for (id, body, bbox) in std::mem::take(&mut w.forms) {
        let data = deflate(&body);
        let mut form = w.chunk.form_xobject(id, &data);
        form.filter(Filter::FlateDecode);
        form.bbox(pdf_writer::Rect::new(bbox.x0 as f32, bbox.y0 as f32, bbox.x1 as f32, bbox.y1 as f32));
        form.group().transparency().isolated(true);
        form.pair(Name(b"Resources"), resources);
        form.finish();
    }
    let Some(top) = w.stack.pop() else { return Err(ExportError::Encode("unbalanced layers".into())) };
    let body = deflate(&top.content.finish().into_vec());

    let mut pdf = Pdf::new();
    let (catalog, pages, page, contents) = (Ref::new(1), Ref::new(2), Ref::new(3), Ref::new(4));
    pdf.catalog(catalog).pages(pages);
    pdf.pages(pages).kids([page]).count(1);
    let mut p = pdf.page(page);
    p.parent(pages).media_box(pdf_writer::Rect::new(0.0, 0.0, pw, ph)).contents(contents);
    p.pair(Name(b"Resources"), resources);
    p.group().transparency().isolated(true);
    p.finish();
    pdf.stream(contents, &body).filter(Filter::FlateDecode);
    pdf.document_info(Ref::new(5)).producer(pdf_writer::TextStr("Gesso"));
    pdf.extend(&w.chunk);
    Ok(pdf.finish())
}
