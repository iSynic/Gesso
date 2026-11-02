//! Builds display lists from the document: one node (and its subtree) at a time.

use std::sync::Arc;

use doc::model::{Effect as DocEffect, ImageFit, Paint, PaintKind, StrokeAlign, StrokeCap, StrokeJoin, TextSpan, TextStyle};
use doc::{Document, Node, NodeKey, NodeKind};
use index::{local_transform, sigma, visual_outset};
use kurbo::{Affine, BezPath, Cap, Join, Rect, Shape};
use peniko::{BlendMode, Color, Fill};
use text::{ShapedText, TextEngine, TextInput, vertical_offset};

use crate::geometry::{self, Outline, TOLERANCE};
use crate::images::ImageCache;
use crate::list::{Brush, Cmd, DisplayList, Effect};
use crate::paint;

/// Zoom bands: each fragment's list is built once per band, with level-of-detail rules for the
/// band's lowest zoom. Each band spans 2× so those rules are never far off: text drawn as bars
/// is under 8 px on screen, and from 100 % on only text under 4 px is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Band {
    /// Zoom ≥ 1.
    Close = 0,
    /// 0.5 ≤ zoom < 1.
    Near = 1,
    /// 0.25 ≤ zoom < 0.5.
    Mid = 2,
    /// 0.125 ≤ zoom < 0.25.
    Wide = 3,
    /// 0.06 ≤ zoom < 0.125.
    Far = 4,
    /// Zoom < 0.06.
    Overview = 5,
}

impl Band {
    pub const ALL: [Band; 6] = [Band::Close, Band::Near, Band::Mid, Band::Wide, Band::Far, Band::Overview];
    pub const COUNT: usize = Self::ALL.len();

    pub fn for_zoom(z: f64) -> Band {
        Self::ALL.into_iter().find(|b| z >= b.lod_zoom()).unwrap_or(Band::Overview)
    }

    /// The zoom level-of-detail decisions assume (the band's lower edge).
    pub fn lod_zoom(self) -> f64 {
        match self {
            Band::Close => 1.0,
            Band::Near => 0.5,
            Band::Mid => 0.25,
            Band::Wide => 0.125,
            Band::Far => 0.06,
            Band::Overview => 0.03,
        }
    }

    /// The largest zoom drawn with this band (sizes image mips): twice the upper edge, for 2×
    /// pixel density.
    fn max_zoom(self) -> f64 {
        match self {
            Band::Close => 4.0,
            Band::Near => 2.0,
            Band::Mid => 1.0,
            Band::Wide => 0.5,
            Band::Far => 0.25,
            Band::Overview => 0.12,
        }
    }

    /// Whether effects are drawn in full (blurs, shadows of any shape); below, only cheap
    /// rectangle shadows.
    fn full_effects(self) -> bool {
        self <= Band::Mid
    }
}

/// Items smaller than this on screen (px) are skipped; text smaller than [`GREEK_PX`] is drawn
/// as bars.
pub const MIN_PX: f64 = 0.5;
pub const GREEK_PX: f64 = 4.0;

/// Which part of a node to build. A node split into several fragments draws its own fills in
/// `Pre` and strokes in `Post`; each child is its own fragment in between.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Part {
    Whole,
    Pre,
    Post,
}

/// Where a builder gets shaped text and decoded images: the caches themselves, or read-only
/// views of them (building fragments in parallel), which note a text miss instead of shaping.
pub enum Caches<'a> {
    Fill { text: &'a mut TextEngine, images: &'a mut ImageCache },
    Read { text: &'a TextEngine, images: &'a ImageCache, missed: bool },
}

impl Caches<'_> {
    fn shape(&mut self, input: TextInput<'_>, max_width: Option<f32>) -> Option<Arc<ShapedText>> {
        match self {
            Caches::Fill { text, .. } => Some(text.shape(input, max_width)),
            Caches::Read { text, missed, .. } => {
                let t = text.cached(input, max_width);
                *missed |= t.is_none();
                t
            }
        }
    }

    fn image(&mut self, asset: &doc::model::AssetId, want: f64) -> Option<Arc<crate::list::Image>> {
        match self {
            Caches::Fill { images, .. } => images.get(asset, want),
            Caches::Read { images, .. } => images.peek(asset, want),
        }
    }

    fn image_size(&self, asset: &doc::model::AssetId) -> Option<(u32, u32)> {
        match self {
            Caches::Fill { images, .. } => images.size(asset),
            Caches::Read { images, .. } => images.size(asset),
        }
    }

    /// Whether a read-only build needed text that wasn't shaped (its list is incomplete).
    pub fn missed(&self) -> bool {
        matches!(self, Caches::Read { missed: true, .. })
    }
}

pub struct Builder<'a> {
    pub doc: &'a Document,
    pub caches: Caches<'a>,
    pub band: Band,
    /// Exports: draw everything at full detail, whatever the band (no skipping, no greeking,
    /// effects in full, images at full resolution).
    pub exact: bool,
}

fn rect_path(r: Rect) -> Arc<BezPath> {
    Arc::new(r.to_path(TOLERANCE))
}

fn cap(c: StrokeCap) -> Cap {
    match c {
        StrokeCap::Butt => Cap::Butt,
        StrokeCap::Round => Cap::Round,
        StrokeCap::Square => Cap::Square,
    }
}

fn join(j: StrokeJoin) -> Join {
    match j {
        StrokeJoin::Miter => Join::Miter,
        StrokeJoin::Round => Join::Round,
        StrokeJoin::Bevel => Join::Bevel,
    }
}

/// Bounds of a node's box plus its outsets, in the space `xf` maps to.
fn visual_box(n: &Node, xf: Affine) -> Rect {
    let o = visual_outset(n);
    xf.transform_rect_bbox(Rect::new(-o[0], -o[1], n.props.width + o[2], n.props.height + o[3]))
}

/// Whether the node draws exactly an opaque box, so its shadow is the analytic blurred
/// rectangle. Otherwise the shadow is cast by whatever it draws (offscreen path); both follow the
/// same rule: the shadow is the blurred silhouette of the drawn content.
fn opaque_box(n: &Node) -> bool {
    let rect_like = matches!(n.kind, NodeKind::Frame { .. } | NodeKind::Component { .. } | NodeKind::Rectangle {} | NodeKind::Note { .. });
    // A note without fills draws its default opaque card.
    let default_card = matches!(n.kind, NodeKind::Note { .. }) && n.props.fills.is_empty();
    let opaque_fill = default_card || n.props.fills.iter().any(|p| {
        p.visible && p.opacity >= 1.0 && p.blend == doc::model::BlendMode::Normal && matches!(p.kind, PaintKind::Solid { color } if color.a == 255)
    });
    let strokes_inside = n.props.strokes.iter().all(|s| !s.paint.visible || s.weight <= 0.0 || s.align == StrokeAlign::Inside);
    let children_inside = n.children.is_empty() || index::clips(n);
    rect_like && opaque_fill && strokes_inside && children_inside
}

/// Whether a node can be split into several fragments: nothing about it composites its
/// children as a group.
pub fn splittable(n: &Node) -> bool {
    n.props.opacity >= 1.0
        && n.props.blend == doc::model::BlendMode::Normal
        && n.props.effects.iter().all(|e| !e_visible(e))
        && matches!(n.kind, NodeKind::Frame { .. } | NodeKind::Component { .. } | NodeKind::Group {})
}

fn e_visible(e: &DocEffect) -> bool {
    match e {
        DocEffect::DropShadow(s) | DocEffect::InnerShadow(s) => s.visible,
        DocEffect::LayerBlur { visible, .. } | DocEffect::BackgroundBlur { visible, .. } => *visible,
    }
}

/// The paint text is drawn with: the span's colour, else the node's fills, else the style colour.
fn text_brushes(n: &Node, span: Option<&TextSpan>, style: &TextStyle) -> Vec<(Brush, Affine)> {
    if let Some(c) = span.and_then(|s| s.color) {
        return vec![(Brush::Solid(paint::color(c, 1.0)), Affine::IDENTITY)];
    }
    let fills: Vec<(Brush, Affine)> = n.props.fills.iter().filter(|p| p.visible).filter_map(|p| paint::brush(p, n.props.width, n.props.height)).collect();
    if fills.is_empty() { vec![(Brush::Solid(paint::color(style.color, 1.0)), Affine::IDENTITY)] } else { fills }
}

impl<'a> Builder<'a> {
    pub fn new(doc: &'a Document, caches: Caches<'a>, band: Band) -> Self {
        Builder { doc, caches, band, exact: false }
    }

    /// A builder for exports: see [`Builder::exact`].
    pub fn exact(doc: &'a Document, caches: Caches<'a>) -> Self {
        Builder { doc, caches, band: Band::Close, exact: true }
    }

    /// The zoom level-of-detail decisions assume; infinite when exact.
    fn lod_zoom(&self) -> f64 {
        if self.exact { f64::INFINITY } else { self.band.lod_zoom() }
    }

    fn full_effects(&self) -> bool {
        self.exact || self.band.full_effects()
    }

    fn tiny(&self, n: &Node) -> bool {
        !self.exact && n.props.width.max(n.props.height).max(visual_outset(n).iter().copied().fold(0.0, f64::max)) * self.band.lod_zoom() < MIN_PX
    }

    /// Builds `key` (and, for `Part::Whole`, its subtree) into `out`. `xf` maps the node's
    /// parent space to the fragment's space.
    pub fn node(&mut self, key: NodeKey, parent_xf: Affine, part: Part, out: &mut DisplayList) {
        let Some(n) = self.doc.node(key) else { return };
        self.draw(key, parent_xf * local_transform(n), part, out);
    }

    /// Draws `key` in its own local space (a fragment root): exactly the identity, so the list
    /// doesn't depend on where the root is or how it is rotated.
    pub fn root(&mut self, key: NodeKey, part: Part, out: &mut DisplayList) {
        self.draw(key, Affine::IDENTITY, part, out);
    }

    /// Draws `key` with `xf` as its full transform.
    fn draw(&mut self, key: NodeKey, xf: Affine, part: Part, out: &mut DisplayList) {
        let Some(n) = self.doc.node(key) else { return };
        if !n.props.visible || n.kind.is_page() {
            return;
        }
        match part {
            Part::Pre => return self.fills(n, xf, out),
            Part::Post => return self.strokes(n, xf, out),
            Part::Whole => {}
        }
        // Tiny leaves (and tiny clipping containers) vanish at this band.
        let clips_children = index::clips(n);
        if self.tiny(n) && (n.children.is_empty() || clips_children) && !matches!(n.kind, NodeKind::Group {}) {
            out.skipped += 1;
            return;
        }
        let near = self.full_effects();
        let mut body = DisplayList::default();
        self.body(n, xf, &mut body);
        if body.is_empty() {
            return;
        }
        let effects: Vec<&DocEffect> = n.props.effects.iter().filter(|e| e_visible(e)).collect();
        let layer = n.props.opacity < 1.0 || n.props.blend != doc::model::BlendMode::Normal;
        let content_bounds = body.bounds.unwrap_or_else(|| visual_box(n, xf));

        // Background blur samples what is behind the node, so it goes before the node's layer.
        for e in &effects {
            if let DocEffect::BackgroundBlur { radius, .. } = e
                && near
                && *radius > 0.0
                && let Some(o) = geometry::outline(n).filter(|o| o.closed)
            {
                out.push(Cmd::BackdropBlur { clip: Arc::new(o.path), xf, sigma: sigma(*radius) });
            }
        }
        let mut inner = DisplayList::default();
        // Drop shadows, beneath the content.
        for e in &effects {
            let DocEffect::DropShadow(s) = e else { continue };
            let col = paint::color(s.color, 1.0);
            let (sg, spread) = (sigma(s.blur), s.spread);
            match (opaque_box(n), geometry::uniform_radius(n.props.radius, n.props.width, n.props.height)) {
                (true, Some(r)) => {
                    let rect = Rect::new(0.0, 0.0, n.props.width, n.props.height).inflate(spread, spread) + kurbo::Vec2::new(s.offset[0], s.offset[1]);
                    inner.grow(xf.transform_rect_bbox(rect.inflate(3.0 * sg, 3.0 * sg)));
                    inner.push(Cmd::BlurredRect { rect, radius: (r + spread).max(0.0), sigma: sg, color: col, xf });
                }
                _ if near => {
                    let reach = 3.0 * sg + spread.max(0.0);
                    let off = crate::exec::transform_vector(xf, kurbo::Vec2::new(s.offset[0], s.offset[1]));
                    let bounds = content_bounds.union(content_bounds + off).inflate(reach, reach);
                    inner.grow(bounds);
                    inner.push(Cmd::PushEffect { effect: Effect::DropShadow { color: col, offset: (s.offset[0], s.offset[1]), sigma: sg, spread }, bounds, xf });
                    for c in &body.cmds {
                        inner.push(c.clone());
                    }
                    inner.push(Cmd::PopEffect);
                }
                _ => {}
            }
        }
        let bounds = inner.bounds.map_or(content_bounds, |b| b.union(content_bounds));
        inner.grow(content_bounds);
        inner.greeked += body.greeked;
        inner.skipped += body.skipped;
        for c in body.cmds {
            inner.push(c);
        }
        // Layer blur blurs shadows and content together.
        let blur = effects.iter().find_map(|e| match e {
            DocEffect::LayerBlur { radius, .. } if *radius > 0.0 && near => Some(sigma(*radius)),
            _ => None,
        });
        let mut wrapped = DisplayList::default();
        let bounds = match blur {
            Some(sg) => {
                let b = bounds.inflate(3.0 * sg, 3.0 * sg);
                wrapped.push(Cmd::PushEffect { effect: Effect::LayerBlur { sigma: sg }, bounds: b, xf });
                b
            }
            None => bounds,
        };
        wrapped.bounds = Some(bounds);
        wrapped.greeked = inner.greeked;
        wrapped.skipped = inner.skipped;
        for c in inner.cmds {
            wrapped.push(c);
        }
        if blur.is_some() {
            wrapped.push(Cmd::PopEffect);
        }
        if layer {
            out.push(Cmd::PushLayer {
                clip: rect_path(bounds.inflate(1.0, 1.0)),
                rule: Fill::NonZero,
                xf: Affine::IDENTITY,
                blend: paint::blend(n.props.blend),
                alpha: n.props.opacity.clamp(0.0, 1.0) as f32,
            });
        }
        out.grow(bounds);
        out.greeked += wrapped.greeked;
        out.skipped += wrapped.skipped;
        for c in wrapped.cmds {
            out.push(c);
        }
        if layer {
            out.push(Cmd::PopLayer);
        }
    }

    /// Fills, inner shadows, children and strokes, in drawing order.
    fn body(&mut self, n: &Node, xf: Affine, out: &mut DisplayList) {
        match &n.kind {
            NodeKind::Text { .. } => self.text(n, xf, out),
            NodeKind::Note { text } => {
                let mut card = DisplayList::default();
                self.fills(n, xf, &mut card);
                for c in card.cmds {
                    out.push(c);
                }
                out.grow(xf.transform_rect_bbox(geometry::outline(n).map_or(Rect::ZERO, |o| o.path.bounding_box())));
                self.note_text(n, text, xf, out);
                self.strokes(n, xf, out);
            }
            NodeKind::Group {} => self.children(n, &n.children, xf, false, out),
            // An instance draws its content: the expanded component, whose root carries the
            // fills, strokes and corner radius (with the instance's `[]` overrides) and clips.
            NodeKind::Instance { .. } => self.children(n, &n.children, xf, false, out),
            _ => {
                self.fills(n, xf, out);
                self.inner_shadows(n, xf, out);
                self.children(n, &n.children, xf, index::clips(n), out);
                self.strokes(n, xf, out);
            }
        }
    }

    fn children(&mut self, n: &Node, kids: &[NodeKey], xf: Affine, clip: bool, out: &mut DisplayList) {
        if kids.is_empty() {
            return;
        }
        let mut sub = DisplayList::default();
        for &k in kids {
            self.node(k, xf, Part::Whole, &mut sub);
        }
        if sub.is_empty() {
            return;
        }
        out.greeked += sub.greeked;
        out.skipped += sub.skipped;
        if clip {
            out.push(Cmd::PushLayer { clip: Arc::new(geometry::clip_path(n)), rule: Fill::NonZero, xf, blend: BlendMode::default(), alpha: 1.0 });
            let own = xf.transform_rect_bbox(Rect::new(0.0, 0.0, n.props.width, n.props.height));
            if let Some(b) = sub.bounds {
                let c = b.intersect(own);
                if c.width() >= 0.0 && c.height() >= 0.0 {
                    out.grow(c);
                }
            }
            for c in sub.cmds {
                out.push(c);
            }
            out.push(Cmd::PopLayer);
        } else {
            if let Some(b) = sub.bounds {
                out.grow(b);
            }
            for c in sub.cmds {
                out.push(c);
            }
        }
    }

    /// The node's fills: paints over its outline (or its image, for image nodes).
    pub fn fills(&mut self, n: &Node, xf: Affine, out: &mut DisplayList) {
        let Some(o) = geometry::outline(n) else { return };
        if !o.closed && !matches!(n.kind, NodeKind::Path { .. }) {
            return;
        }
        let path = Arc::new(o.path);
        let (w, h) = (n.props.width, n.props.height);
        let mut any = false;
        if let NodeKind::Image { asset, fit } = &n.kind {
            any |= self.image_fill(&path, o.rule, xf, asset, *fit, 1.0, BlendMode::default(), w, h, out);
        }
        if let NodeKind::Note { .. } = n.kind
            && n.props.fills.is_empty()
        {
            out.push(Cmd::Fill { path: path.clone(), rule: o.rule, xf, brush: Brush::Solid(Color::from_rgba8(0xff, 0xf1, 0xa8, 0xff)), brush_xf: Affine::IDENTITY });
            any = true;
        }
        for p in n.props.fills.iter().filter(|p| p.visible) {
            any |= self.paint(p, &path, o.rule, xf, w, h, out);
        }
        if any {
            out.grow(xf.transform_rect_bbox(path.bounding_box()));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn image_fill(&mut self, path: &Arc<BezPath>, rule: Fill, xf: Affine, asset: &doc::model::AssetId, fit: ImageFit, opacity: f64, blend: BlendMode, w: f64, h: f64, out: &mut DisplayList) -> bool {
        let want = if self.exact { f64::INFINITY } else { w * self.band.max_zoom() };
        let img = self.caches.image(asset, want);
        let size = self.caches.image_size(asset);
        let layered = blend != BlendMode::default();
        match (img, size) {
            (Some(img), Some((iw, ih))) => {
                let (brush, brush_xf, region) = paint::image_brush(img, iw, ih, fit, opacity, w, h);
                let full = region == Rect::new(0.0, 0.0, w, h);
                if layered || !full {
                    out.push(Cmd::PushLayer { clip: path.clone(), rule, xf, blend, alpha: 1.0 });
                    out.push(Cmd::Fill { path: rect_path(region), rule: Fill::NonZero, xf, brush, brush_xf });
                    out.push(Cmd::PopLayer);
                } else {
                    out.push(Cmd::Fill { path: path.clone(), rule, xf, brush, brush_xf });
                }
            }
            _ => out.push(Cmd::Fill { path: path.clone(), rule, xf, brush: paint::placeholder(), brush_xf: Affine::IDENTITY }),
        }
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn paint(&mut self, p: &Paint, path: &Arc<BezPath>, rule: Fill, xf: Affine, w: f64, h: f64, out: &mut DisplayList) -> bool {
        let blend = paint::blend(p.blend);
        if let PaintKind::Image { asset, fit } = &p.kind {
            return self.image_fill(path, rule, xf, asset, *fit, p.opacity, blend, w, h, out);
        }
        let Some((brush, brush_xf)) = paint::brush(p, w, h) else { return false };
        if blend != BlendMode::default() {
            out.push(Cmd::PushLayer { clip: path.clone(), rule, xf, blend, alpha: 1.0 });
            out.push(Cmd::Fill { path: path.clone(), rule, xf, brush, brush_xf });
            out.push(Cmd::PopLayer);
        } else {
            out.push(Cmd::Fill { path: path.clone(), rule, xf, brush, brush_xf });
        }
        true
    }

    /// Inner shadows: drawn over the fills, inside the outline.
    fn inner_shadows(&mut self, n: &Node, xf: Affine, out: &mut DisplayList) {
        if !self.full_effects() {
            return;
        }
        let Some(o) = geometry::outline(n).filter(|o| o.closed) else { return };
        let path = Arc::new(o.path);
        let bounds = xf.transform_rect_bbox(path.bounding_box());
        for e in &n.props.effects {
            let DocEffect::InnerShadow(s) = e else { continue };
            if !s.visible {
                continue;
            }
            let effect = Effect::InnerShadow { color: paint::color(s.color, 1.0), offset: (s.offset[0], s.offset[1]), sigma: sigma(s.blur), spread: s.spread };
            out.push(Cmd::PushEffect { effect, bounds, xf });
            out.push(Cmd::Fill { path: path.clone(), rule: o.rule, xf, brush: Brush::Solid(Color::BLACK), brush_xf: Affine::IDENTITY });
            out.push(Cmd::PopEffect);
        }
    }

    pub fn strokes(&mut self, n: &Node, xf: Affine, out: &mut DisplayList) {
        if n.props.strokes.is_empty() {
            return;
        }
        let Some(o) = geometry::outline(n) else { return };
        let Outline { path, rule, closed } = o;
        let path = Arc::new(path);
        for s in n.props.strokes.iter().filter(|s| s.paint.visible && s.weight > 0.0) {
            let align = if closed { s.align } else { StrokeAlign::Center };
            let width = if align == StrokeAlign::Center { s.weight } else { s.weight * 2.0 };
            let mut style = kurbo::Stroke::new(width).with_caps(cap(s.cap)).with_join(join(s.join));
            if !s.dash.is_empty() && s.dash.iter().any(|d| *d > 0.0) {
                style = style.with_dashes(0.0, s.dash.iter().copied());
            }
            let (brush, brush_xf) = match &s.paint.kind {
                PaintKind::Image { .. } => (paint::placeholder(), Affine::IDENTITY),
                _ => match paint::brush(&s.paint, n.props.width, n.props.height) {
                    Some(b) => b,
                    None => continue,
                },
            };
            let style = Arc::new(style);
            let blend = paint::blend(s.paint.blend);
            let bb = path.bounding_box();
            let reach = s.weight * 10.0;
            let clip = match align {
                StrokeAlign::Center => None,
                StrokeAlign::Inside => Some((path.clone(), rule)),
                StrokeAlign::Outside => Some((Arc::new(geometry::outside_ring(&path, bb.inflate(reach, reach))), Fill::EvenOdd)),
            };
            let layered = clip.is_some() || blend != BlendMode::default();
            if layered {
                let (c, r) = clip.unwrap_or_else(|| (rect_path(bb.inflate(reach, reach)), Fill::NonZero));
                out.push(Cmd::PushLayer { clip: c, rule: r, xf, blend, alpha: 1.0 });
            }
            out.push(Cmd::Stroke { path: path.clone(), style, xf, brush, brush_xf });
            if layered {
                out.push(Cmd::PopLayer);
            }
            let grow = if align == StrokeAlign::Inside { 0.0 } else { s.weight * 2.0 };
            out.grow(xf.transform_rect_bbox(bb.inflate(grow, grow)));
        }
    }

    fn text(&mut self, n: &Node, xf: Affine, out: &mut DisplayList) {
        let NodeKind::Text { spans, style, vertical_align, .. } = &n.kind else { return };
        let (w, h) = (n.props.width, n.props.height);
        let size = spans.iter().filter_map(|s| s.font_size).fold(style.font_size, f64::max);
        if size * self.lod_zoom() < GREEK_PX {
            self.greek(n, style, size, xf, out);
            return;
        }
        let Some(input) = TextInput::from_kind(&n.kind) else { return };
        let wrap = layout::semantics::text_wraps(n);
        let Some(shaped) = self.caches.shape(input, wrap.then_some(w as f32)) else { return };
        // Fixed boxes place the text vertically; auto-sized boxes already fit it.
        let dy = if shaped.height as f64 > h { 0.0 } else { vertical_offset(*vertical_align, h as f32, shaped.height) as f64 };
        let txf = xf * Affine::translate((0.0, dy));
        let draw = shaped.draw();
        for run in &draw.runs {
            let span = spans.get(run.span as usize);
            for (brush, brush_xf) in text_brushes(n, span, style) {
                out.push(Cmd::Glyphs { run: run.clone(), xf: txf, brush, brush_xf: Affine::translate((0.0, -dy)) * brush_xf });
            }
        }
        for d in &draw.decorations {
            let span = spans.get(d.span as usize);
            let r = Rect::new(d.rect[0] as f64, d.rect[1] as f64, d.rect[2] as f64, d.rect[3] as f64);
            for (brush, brush_xf) in text_brushes(n, span, style) {
                out.push(Cmd::Fill { path: rect_path(r), rule: Fill::NonZero, xf: txf, brush, brush_xf: Affine::translate((0.0, -dy)) * brush_xf });
            }
        }
        let tb = Rect::new(0.0, dy, (shaped.width as f64).max(w), dy + shaped.height as f64).union(Rect::new(0.0, 0.0, w, h));
        out.grow(xf.transform_rect_bbox(tb));
    }

    /// Text too small to read: one bar per line, in the text's colour at reduced alpha.
    fn greek(&mut self, n: &Node, style: &TextStyle, size: f64, xf: Affine, out: &mut DisplayList) {
        let (w, h) = (n.props.width, n.props.height);
        let line = style.line_height.unwrap_or(size * 1.2).max(1.0);
        let lines = ((h / line).round() as usize).clamp(1, 200);
        let mut p = BezPath::new();
        for i in 0..lines {
            let y = i as f64 * line + line * 0.25;
            let len = if i + 1 == lines && lines > 1 { w * 0.6 } else { w };
            p.extend(Rect::new(0.0, y, len, y + line * 0.5).to_path(TOLERANCE));
        }
        let c = n.kind_color().unwrap_or(style.color);
        out.push(Cmd::Fill { path: Arc::new(p), rule: Fill::NonZero, xf, brush: Brush::Solid(paint::color(c, 0.35)), brush_xf: Affine::IDENTITY });
        out.grow(xf.transform_rect_bbox(Rect::new(0.0, 0.0, w, h)));
        out.greeked += 1;
    }

    fn note_text(&mut self, n: &Node, body: &str, xf: Affine, out: &mut DisplayList) {
        const PAD: f64 = 12.0;
        let style = TextStyle { font_size: 14.0, color: doc::model::Color::rgb(0x33, 0x30, 0x20), ..TextStyle::default() };
        if style.font_size * self.lod_zoom() < GREEK_PX {
            return;
        }
        let spans = [TextSpan { text: body.to_string(), ..Default::default() }];
        let input = TextInput { spans: &spans, style: &style, align: doc::model::TextAlign::Left };
        let Some(shaped) = self.caches.shape(input, Some((n.props.width - 2.0 * PAD).max(1.0) as f32)) else { return };
        let txf = xf * Affine::translate((PAD, PAD));
        let clip = Arc::new(geometry::clip_path(n));
        out.push(Cmd::PushLayer { clip, rule: Fill::NonZero, xf, blend: BlendMode::default(), alpha: 1.0 });
        for run in &shaped.draw().runs {
            out.push(Cmd::Glyphs { run: run.clone(), xf: txf, brush: Brush::Solid(paint::color(style.color, 1.0)), brush_xf: Affine::IDENTITY });
        }
        out.push(Cmd::PopLayer);
    }
}

trait KindColor {
    fn kind_color(&self) -> Option<doc::model::Color>;
}

impl KindColor for Node {
    /// The first span colour, for greeked text.
    fn kind_color(&self) -> Option<doc::model::Color> {
        match &self.kind {
            NodeKind::Text { spans, .. } => spans.iter().find_map(|s| s.color),
            _ => None,
        }
    }
}
