//! SVG import: an SVG document becomes a clipping frame of nodes (via usvg, which resolves
//! styles, `use`, units and text into a simple tree).
//!
//! - Shapes become `path` nodes with their transform baked into the path data (our nodes have
//!   no skew), fills, strokes (centred, as in SVG) and the fill rule.
//! - Groups with an id, opacity or blend mode become groups; others are flattened.
//! - A clip path that is one axis-aligned (rounded) rectangle becomes a clipping frame; content
//!   under any other clip path is drawn into an image with the clip applied. Masks and filters
//!   are dropped. Each loss is reported as a warning.
//! - Gradients are mapped into the node's unit square (a skewed gradient is approximated).
//! - Raster images become image nodes; their bytes are returned to be stored as assets.
//! - Text is converted to outlines with the bundled Inter (and any fonts given).

use std::sync::Arc;

use kurbo::{Affine, BezPath, Point, Rect, Shape, Vec2};
use serde_json::{Map, Value, json};
use render::exec::transform_vector;
use usvg::{Node, Paint};

use crate::svg::{num, path_data};

/// The result of importing an SVG.
#[derive(Debug, Clone)]
pub struct Imported {
    /// A frame node (with children) for `Tx::insert_json`.
    pub node: Value,
    /// Raster images the nodes use: register each with `Session::add_asset`; their ids
    /// (`format::identify_asset`) are already in the nodes.
    pub images: Vec<Vec<u8>>,
    /// What could not be imported exactly.
    pub warnings: Vec<String>,
    /// Shapes imported.
    pub paths: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("not a valid SVG: {0}")]
    Parse(String),
    #[error("the SVG has nothing to import")]
    Empty,
}

/// Whether `text` looks like an SVG document (for paste).
pub fn is_svg(text: &str) -> bool {
    let t = text.trim_start().trim_start_matches('\u{feff}');
    t.starts_with("<svg") || ((t.starts_with("<?xml") || t.starts_with("<!--") || t.starts_with("<!DOCTYPE")) && t.contains("<svg"))
}

#[derive(Default)]
struct Ctx {
    images: Vec<Vec<u8>>,
    warnings: Vec<String>,
    paths: usize,
}

impl Ctx {
    fn warn(&mut self, w: String) {
        if !self.warnings.contains(&w) {
            self.warnings.push(w);
        }
    }
}

fn affine(t: usvg::Transform) -> Affine {
    Affine::new([t.sx, t.ky, t.kx, t.sy, t.tx, t.ty].map(f64::from))
}

fn bez(p: &usvg::tiny_skia_path::Path) -> BezPath {
    use usvg::tiny_skia_path::PathSegment as S;
    let pt = |p: usvg::tiny_skia_path::Point| Point::new(f64::from(p.x), f64::from(p.y));
    let mut out = BezPath::new();
    for s in p.segments() {
        match s {
            S::MoveTo(a) => out.move_to(pt(a)),
            S::LineTo(a) => out.line_to(pt(a)),
            S::QuadTo(a, b) => out.quad_to(pt(a), pt(b)),
            S::CubicTo(a, b, c) => out.curve_to(pt(a), pt(b), pt(c)),
            S::Close => out.close_path(),
        }
    }
    out
}

fn hex(c: usvg::Color, alpha: f32) -> String {
    let a = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
    if a == 255 { format!("#{:02x}{:02x}{:02x}", c.red, c.green, c.blue) } else { format!("#{:02x}{:02x}{:02x}{a:02x}", c.red, c.green, c.blue) }
}

fn blend_name(b: usvg::BlendMode) -> Option<&'static str> {
    use usvg::BlendMode as B;
    Some(match b {
        B::Normal => return None,
        B::Multiply => "multiply",
        B::Screen => "screen",
        B::Overlay => "overlay",
        B::Darken => "darken",
        B::Lighten => "lighten",
        B::ColorDodge => "colorDodge",
        B::ColorBurn => "colorBurn",
        B::HardLight => "hardLight",
        B::SoftLight => "softLight",
        B::Difference => "difference",
        B::Exclusion => "exclusion",
        B::Hue => "hue",
        B::Saturation => "saturation",
        B::Color => "color",
        B::Luminosity => "luminosity",
    })
}

/// A paint for a node whose box (absolute) is `bbox`; `user` maps the shape's user space there.
fn paint(cx: &mut Ctx, p: &Paint, opacity: f32, user: Affine, bbox: Rect) -> Option<Value> {
    let unit = |pt: Point| -> [f64; 2] {
        let q = user * pt;
        [(q.x - bbox.x0) / bbox.width().max(1e-9), (q.y - bbox.y0) / bbox.height().max(1e-9)]
    };
    let stops = |g: &usvg::BaseGradient| -> Vec<Value> { g.stops().iter().map(|s| json!({"offset": f64::from(s.offset().get()), "color": hex(s.color(), s.opacity().get())})).collect() };
    let spread = |cx: &mut Ctx, g: &usvg::BaseGradient| {
        if g.spread_method() != usvg::SpreadMethod::Pad {
            cx.warn("repeating gradients are imported as padded".into());
        }
    };
    let mut v = match p {
        Paint::Color(c) => json!({"type": "solid", "color": hex(*c, 1.0)}),
        Paint::LinearGradient(g) => {
            spread(cx, g);
            let to = |pt: Point| unit(affine(g.transform()) * pt);
            json!({"type": "linearGradient", "start": to(Point::new(f64::from(g.x1()), f64::from(g.y1()))), "end": to(Point::new(f64::from(g.x2()), f64::from(g.y2()))), "stops": stops(g)})
        }
        Paint::RadialGradient(g) => {
            spread(cx, g);
            if (g.fx(), g.fy()) != (g.cx(), g.cy()) {
                cx.warn("radial gradients with a focal point are imported centred".into());
            }
            let gt = affine(g.transform());
            let full = user * gt;
            let r = f64::from(g.r().get());
            let rx = transform_vector(full, Vec2::new(r, 0.0)).hypot();
            let ry = transform_vector(full, Vec2::new(0.0, r)).hypot();
            let c = unit(gt * Point::new(f64::from(g.cx()), f64::from(g.cy())));
            json!({"type": "radialGradient", "center": c, "radius": [rx / bbox.width().max(1e-9), ry / bbox.height().max(1e-9)], "stops": stops(g)})
        }
        Paint::Pattern(_) => {
            cx.warn("pattern paints are not supported and were left out".into());
            return None;
        }
    };
    if opacity < 1.0
        && let Some(o) = v.as_object_mut()
    {
        o.insert("opacity".into(), json!(f64::from(opacity)));
    }
    Some(v)
}

fn path_node(cx: &mut Ctx, p: &usvg::Path, origin: Point) -> Option<Value> {
    if !p.is_visible() {
        return None;
    }
    let xf = affine(p.abs_transform());
    let mut path = bez(p.data());
    path.apply_affine(xf);
    let b = path.bounding_box();
    if !(b.x0.is_finite() && b.y0.is_finite()) {
        return None;
    }
    // A zero-width or zero-height box (a straight line) still needs a size.
    let bbox = Rect::new(b.x0, b.y0, b.x1.max(b.x0 + 1e-3), b.y1.max(b.y0 + 1e-3));
    path.apply_affine(Affine::translate(-bbox.origin().to_vec2()));
    let mut fills = Vec::new();
    let mut rule = "nonZero";
    if let Some(f) = p.fill() {
        if f.rule() == usvg::FillRule::EvenOdd {
            rule = "evenOdd";
        }
        fills.extend(paint(cx, f.paint(), f.opacity().get(), xf, bbox));
    }
    let mut strokes = Vec::new();
    if let Some(s) = p.stroke()
        && let Some(pv) = paint(cx, s.paint(), s.opacity().get(), xf, bbox)
    {
        // Lengths scale with the transform (uniformly, approximately).
        let k = xf.determinant().abs().sqrt();
        let mut st = Map::new();
        st.insert("paint".into(), pv);
        st.insert("weight".into(), json!(f64::from(s.width().get()) * k));
        st.insert("align".into(), json!("center"));
        match s.linecap() {
            usvg::LineCap::Butt => {}
            usvg::LineCap::Round => {
                st.insert("cap".into(), json!("round"));
            }
            usvg::LineCap::Square => {
                st.insert("cap".into(), json!("square"));
            }
        }
        match s.linejoin() {
            usvg::LineJoin::Miter | usvg::LineJoin::MiterClip => {}
            usvg::LineJoin::Round => {
                st.insert("join".into(), json!("round"));
            }
            usvg::LineJoin::Bevel => {
                st.insert("join".into(), json!("bevel"));
            }
        }
        if let Some(d) = s.dasharray() {
            st.insert("dash".into(), json!(d.iter().map(|v| f64::from(*v) * k).collect::<Vec<_>>()));
        }
        strokes.push(Value::Object(st));
    }
    if fills.is_empty() && strokes.is_empty() {
        return None;
    }
    cx.paths += 1;
    let mut n = json!({
        "type": "path",
        "name": if p.id().is_empty() { "Path" } else { p.id() },
        "x": round(bbox.x0 - origin.x), "y": round(bbox.y0 - origin.y),
        "width": round(bbox.width()), "height": round(bbox.height()),
        "paths": [{"d": path_data(&path), "fillRule": rule}],
    });
    if let Some(o) = n.as_object_mut() {
        if !fills.is_empty() {
            o.insert("fills".into(), Value::Array(fills));
        }
        if !strokes.is_empty() {
            o.insert("strokes".into(), Value::Array(strokes));
        }
    }
    Some(n)
}

fn round(v: f64) -> f64 {
    num(v).parse().unwrap_or(v)
}

fn image_node(cx: &mut Ctx, im: &usvg::Image, origin: Point) -> Option<Value> {
    if !im.is_visible() {
        return None;
    }
    let bytes: &Arc<Vec<u8>> = match im.kind() {
        usvg::ImageKind::PNG(b) | usvg::ImageKind::JPEG(b) | usvg::ImageKind::WEBP(b) | usvg::ImageKind::GIF(b) => b,
        usvg::ImageKind::SVG(_) => {
            cx.warn("SVG images inside the SVG are not imported".into());
            return None;
        }
    };
    let Ok((id, _)) = format::identify_asset(bytes) else {
        cx.warn("an image in an unsupported format was left out".into());
        return None;
    };
    let xf = affine(im.abs_transform());
    let size = im.size();
    let local = Rect::new(0.0, 0.0, f64::from(size.width()), f64::from(size.height()));
    let c = xf.as_coeffs();
    if c[1].abs() > 1e-6 || c[2].abs() > 1e-6 {
        cx.warn("rotated or skewed images are imported unrotated".into());
    }
    let b = xf.transform_rect_bbox(local);
    cx.images.push(bytes.to_vec());
    Some(json!({"type": "image", "name": if im.id().is_empty() { "Image" } else { im.id() }, "asset": id.0, "fit": "crop",
        "x": round(b.x0 - origin.x), "y": round(b.y0 - origin.y), "width": round(b.width()), "height": round(b.height())}))
}

/// What a group's clip path becomes.
enum Clip {
    /// No clip, or one that cuts nothing.
    None,
    /// A clipping frame: an axis-aligned (rounded) rectangle, absolute, and its corner radius.
    Frame(Rect, f64),
    Unsupported,
}

/// Distance from `p` to the outline of the rounded rectangle `r` with radius `rad`.
fn rounded_rect_distance(p: Point, r: Rect, rad: f64) -> f64 {
    let c = r.center();
    let half = Vec2::new(r.width() / 2.0 - rad, r.height() / 2.0 - rad);
    let q = Vec2::new((p.x - c.x).abs() - half.x, (p.y - c.y).abs() - half.y);
    (Vec2::new(q.x.max(0.0), q.y.max(0.0)).hypot() + q.x.max(q.y).min(0.0) - rad).abs()
}

/// The one path under `g`, through groups that only hold it (usvg wraps transformed shapes in
/// groups; the path's absolute transform includes theirs).
fn only_path(g: &usvg::Group) -> Option<&usvg::Path> {
    match g.children() {
        [Node::Path(p)] => Some(p),
        [Node::Group(sub)] if sub.clip_path().is_none() && sub.mask().is_none() && sub.filters().is_empty() => only_path(sub),
        _ => None,
    }
}

fn clip_of(g: &usvg::Group) -> Clip {
    let Some(cp) = g.clip_path() else { return Clip::None };
    let Some(p) = only_path(cp.root()) else { return Clip::Unsupported };
    if cp.clip_path().is_some() {
        return Clip::Unsupported;
    }
    let mut path = bez(p.data());
    path.apply_affine(affine(g.abs_transform()) * affine(cp.transform()) * affine(p.abs_transform()));
    let r = path.bounding_box();
    // A rounded rectangle has area w·h − (4 − π)·r².
    let rad = ((r.area() - path.area().abs()).max(0.0) / (4.0 - std::f64::consts::PI)).sqrt().min(r.width().min(r.height()) / 2.0);
    let tol = 0.5f64.max(r.width().max(r.height()) * 0.005);
    let fits = path.segments().all(|seg| [0.0, 0.5, 1.0].iter().all(|t| rounded_rect_distance(kurbo::ParamCurve::eval(&seg, *t), r, rad) < tol));
    if !fits {
        return Clip::Unsupported;
    }
    // A clip around everything the group draws cuts nothing (as exports of opacity layers do).
    let b = g.abs_stroke_bounding_box();
    let inset = rad * (1.0 - std::f64::consts::FRAC_1_SQRT_2);
    let content = Rect::new(f64::from(b.left()), f64::from(b.top()), f64::from(b.right()), f64::from(b.bottom()));
    let safe = r.inset(-inset);
    if safe.x0 <= content.x0 + 1e-3 && safe.y0 <= content.y0 + 1e-3 && safe.x1 >= content.x1 - 1e-3 && safe.y1 >= content.y1 - 1e-3 {
        return Clip::None;
    }
    Clip::Frame(r, rad)
}

/// One image clipped to one shape (how image fills are written): a shape with an image fill.
fn clipped_image(cx: &mut Ctx, g: &usvg::Group, origin: Point) -> Option<Value> {
    let cp = g.clip_path()?;
    let ([Node::Image(im)], Some(p)) = (g.children(), only_path(cp.root())) else { return None };
    if cp.clip_path().is_some() || g.mask().is_some() || !g.filters().is_empty() || !im.is_visible() {
        return None;
    }
    let bytes = match im.kind() {
        usvg::ImageKind::PNG(b) | usvg::ImageKind::JPEG(b) | usvg::ImageKind::WEBP(b) | usvg::ImageKind::GIF(b) => b,
        usvg::ImageKind::SVG(_) => return None,
    };
    let (id, _) = format::identify_asset(bytes).ok()?;
    let mut shape = bez(p.data());
    shape.apply_affine(affine(g.abs_transform()) * affine(cp.transform()) * affine(p.abs_transform()));
    let b = shape.bounding_box();
    let size = im.size();
    let ib = affine(im.abs_transform()).transform_rect_bbox(Rect::new(0.0, 0.0, f64::from(size.width()), f64::from(size.height())));
    // The image fills the shape's box exactly (stretched), or covers it.
    let close = |a: f64, c: f64| (a - c).abs() < 0.5;
    let fit = if close(ib.x0, b.x0) && close(ib.y0, b.y0) && close(ib.x1, b.x1) && close(ib.y1, b.y1) { "crop" } else { "fill" };
    shape.apply_affine(Affine::translate(-b.origin().to_vec2()));
    cx.images.push(bytes.to_vec());
    cx.paths += 1;
    let mut fill = json!({"type": "image", "asset": id.0, "fit": fit});
    let alpha = g.opacity().get();
    if alpha < 1.0
        && let Some(o) = fill.as_object_mut()
    {
        o.insert("opacity".into(), json!(f64::from(alpha)));
    }
    let name = [g.id(), im.id()].into_iter().find(|s| !s.is_empty()).unwrap_or("Image");
    Some(json!({"type": "path", "name": name, "x": round(b.x0 - origin.x), "y": round(b.y0 - origin.y), "width": round(b.width()), "height": round(b.height()),
        "paths": [{"d": path_data(&shape)}], "fills": [fill]}))
}

/// The shapes of a clip path, absolute, with their fill rules.
fn clip_shapes(g: &usvg::Group, xf: Affine, out: &mut Vec<(BezPath, bool)>) {
    for node in g.children() {
        match node {
            Node::Path(p) if p.is_visible() => {
                let mut path = bez(p.data());
                path.apply_affine(xf * affine(p.abs_transform()));
                out.push((path, p.fill().is_some_and(|f| f.rule() == usvg::FillRule::EvenOdd)));
            }
            Node::Text(t) => clip_shapes(t.flattened(), xf, out),
            Node::Group(sub) => clip_shapes(sub, xf, out),
            _ => {}
        }
    }
}

/// The largest side, in pixels, of an image made from clipped content.
const RASTER_MAX: f64 = 4096.0;

/// A group under a clip path we have no node for: its content and its clip, drawn by our own
/// renderer at 2× (or less, for large content), multiplied together into one image node.
fn rasterised(cx: &mut Ctx, g: &usvg::Group, origin: Point) -> Option<Value> {
    let cp = g.clip_path()?;
    if cp.clip_path().is_some() {
        return None;
    }
    let mut sub = Ctx::default();
    let mut content = Vec::new();
    children(&mut sub, g, Point::ZERO, &mut content);
    let mut shapes = Vec::new();
    clip_shapes(cp.root(), affine(g.abs_transform()) * affine(cp.transform()), &mut shapes);
    let b = g.abs_stroke_bounding_box();
    let mut area = Rect::new(f64::from(b.left()), f64::from(b.top()), f64::from(b.right()), f64::from(b.bottom()));
    let clip = shapes.iter().map(|(p, _)| p.bounding_box()).reduce(|a, b| a.union(b))?;
    area = area.intersect(clip).expand();
    if content.is_empty() || area.width() < 1.0 || area.height() < 1.0 {
        return None;
    }
    let scale = (RASTER_MAX / area.width().max(area.height())).min(2.0);
    let mask: Vec<Value> = shapes
        .iter()
        .filter_map(|(p, even)| {
            let bb = p.bounding_box();
            let mut local = p.clone();
            local.apply_affine(Affine::translate(-bb.origin().to_vec2()));
            (bb.width() > 0.0 && bb.height() > 0.0).then(|| {
                json!({"type": "path", "x": bb.x0, "y": bb.y0, "width": bb.width(), "height": bb.height(),
                    "paths": [{"d": path_data(&local), "fillRule": if *even { "evenOdd" } else { "nonZero" }}],
                    "fills": [{"type": "solid", "color": "#000000"}]})
            })
        })
        .collect();
    let draw = |nodes: Vec<Value>| -> Option<(u32, u32, Vec<u8>)> {
        let mut s = doc::Store::default();
        s.set_deriver(Box::new(layout::LayoutDeriver::new(text::TextEngine::new(text::FontDb::bundled_only()))));
        let mut assets = render::MemoryAssets::default();
        let page = s
            .transact(doc::TxMeta::user("rasterise"), |tx| {
                for bytes in &sub.images {
                    let Ok((id, meta)) = format::identify_asset(bytes) else { continue };
                    assets.0.insert(id.clone(), bytes.clone());
                    tx.put_asset(id, meta)?;
                }
                let page = tx.insert_json(None, None, json!({"type": "page"}))?;
                for n in nodes {
                    tx.insert_json(Some(page), None, n)?;
                }
                Ok(page)
            })
            .ok()?
            .0;
        let d = s.into_doc();
        let ids: Vec<_> = d.node(d.key(page)?)?.children.iter().filter_map(|k| d.node(*k).map(|n| n.id)).collect();
        let c = crate::collect(&d, &ids, text::FontDb::bundled_only(), &assets).ok()?;
        let px = crate::raster::region(&c.items, area - Vec2::new(c.origin.0, c.origin.1), scale).ok()?;
        Some((u32::from(px.width()), u32::from(px.height()), crate::raster::rgba(&px)))
    };
    let (w, h, mut rgba) = draw(content)?;
    let (mw, mh, coverage) = draw(mask)?;
    if (w, h) != (mw, mh) {
        return None;
    }
    for (p, m) in rgba.as_chunks_mut::<4>().0.iter_mut().zip(coverage.as_chunks::<4>().0) {
        p[3] = ((u32::from(p[3]) * u32::from(m[3]) + 127) / 255) as u8;
    }
    let img = image::RgbaImage::from_raw(w, h, rgba)?;
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).ok()?;
    let (id, _) = format::identify_asset(&png).ok()?;
    for w in sub.warnings {
        cx.warn(w);
    }
    cx.images.push(png);
    cx.paths += 1;
    Some(json!({"type": "image", "name": "Clipped", "asset": id.0, "fit": "fill",
        "x": round(area.x0 - origin.x), "y": round(area.y0 - origin.y), "width": round(area.width()), "height": round(area.height())}))
}

/// Children of `g`, placed relative to `origin` (absolute coordinates of their parent node).
fn children(cx: &mut Ctx, g: &usvg::Group, origin: Point, out: &mut Vec<Value>) {
    for node in g.children() {
        match node {
            Node::Path(p) => out.extend(path_node(cx, p, origin)),
            Node::Image(im) => out.extend(image_node(cx, im, origin)),
            Node::Text(t) => children(cx, t.flattened(), origin, out),
            Node::Group(sub) => group(cx, sub, origin, out),
        }
    }
}

fn group(cx: &mut Ctx, g: &usvg::Group, origin: Point, out: &mut Vec<Value>) {
    if g.mask().is_some() {
        cx.warn("masks are not supported; masked content is imported unmasked".into());
    }
    if !g.filters().is_empty() {
        cx.warn("filters (blur, shadows, …) are not imported".into());
    }
    if let Some(n) = clipped_image(cx, g, origin) {
        out.push(n);
        return;
    }
    let clip = match clip_of(g) {
        Clip::None => None,
        Clip::Frame(r, rad) => Some((r, rad)),
        Clip::Unsupported => match rasterised(cx, g, origin) {
            Some(mut img) => {
                cx.warn("content under a non-rectangular clip path was imported as an image".into());
                if let Some(o) = img.as_object_mut() {
                    if g.opacity().get() < 1.0 {
                        o.insert("opacity".into(), json!(f64::from(g.opacity().get())));
                    }
                    if let Some(b) = blend_name(g.blend_mode()) {
                        o.insert("blend".into(), json!(b));
                    }
                    if !g.id().is_empty() {
                        o.insert("name".into(), json!(g.id()));
                    }
                }
                out.push(img);
                return;
            }
            None => {
                cx.warn("a clip path could not be imported and was left out".into());
                None
            }
        },
    };
    let opacity = g.opacity().get();
    let blend = blend_name(g.blend_mode());
    let mut props = Map::new();
    if opacity < 1.0 {
        props.insert("opacity".into(), json!(f64::from(opacity)));
    }
    if let Some(b) = blend {
        props.insert("blend".into(), json!(b));
    }
    if !g.id().is_empty() {
        props.insert("name".into(), json!(g.id()));
    }
    match clip {
        Some((r, rad)) => {
            let mut kids = Vec::new();
            children(cx, g, r.origin(), &mut kids);
            props.insert("type".into(), json!("frame"));
            props.insert("clip".into(), json!(true));
            if rad > 0.01 {
                props.insert("radius".into(), json!(round(rad)));
            }
            props.entry("name").or_insert(json!("Clip"));
            for (k, v) in [("x", r.x0 - origin.x), ("y", r.y0 - origin.y), ("width", r.width()), ("height", r.height())] {
                props.insert(k.into(), json!(round(v)));
            }
            props.insert("children".into(), Value::Array(kids));
            out.push(Value::Object(props));
        }
        None if props.is_empty() => children(cx, g, origin, out),
        None => {
            let mut kids = Vec::new();
            // A group's box follows its children, and they keep their place on the page.
            children(cx, g, origin, &mut kids);
            if kids.is_empty() {
                return;
            }
            props.insert("type".into(), json!("group"));
            props.entry("name").or_insert(json!("Group"));
            props.insert("children".into(), Value::Array(kids));
            out.push(Value::Object(props));
        }
    }
}

/// Imports SVG source as a frame named `name`. `fonts` are extra font files for text.
pub fn import_svg(source: &str, name: &str, fonts: &[&[u8]]) -> Result<Imported, ImportError> {
    let mut opt = usvg::Options::default();
    {
        let db = Arc::make_mut(&mut opt.fontdb);
        db.load_font_data(text::INTER_TTF.to_vec());
        for f in fonts {
            db.load_font_data(f.to_vec());
        }
        // Unknown families fall back to the generic families (serif last), all the bundled Inter
        // under whatever name its file gives.
        let first = db.faces().next().and_then(|f| f.families.first()).map(|f| f.0.clone());
        if let Some(name) = first {
            db.set_serif_family(name.clone());
            db.set_sans_serif_family(name.clone());
            db.set_monospace_family(name.clone());
            db.set_cursive_family(name.clone());
            db.set_fantasy_family(name.clone());
            opt.font_family = name;
        }
    }
    let tree = usvg::Tree::from_str(source, &opt).map_err(|e| ImportError::Parse(e.to_string()))?;
    let mut cx = Ctx::default();
    let mut kids = Vec::new();
    children(&mut cx, tree.root(), Point::ZERO, &mut kids);
    if kids.is_empty() {
        return Err(ImportError::Empty);
    }
    let size = tree.size();
    let node = json!({"type": "frame", "name": name, "clip": true,
        "width": round(f64::from(size.width())), "height": round(f64::from(size.height())), "children": kids});
    Ok(Imported { node, images: cx.images, warnings: cx.warnings, paths: cx.paths })
}
