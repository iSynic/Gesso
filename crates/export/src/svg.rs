//! SVG 1.1 (plus CSS blend modes) from display lists.
//!
//! Every shape carries its full transform; groups (layers, effects) carry none, so filter
//! regions and lengths are in export units. Text is drawn as outlined paths, so it looks the
//! same everywhere. Shadows and layer blur become SVG filters; background blur and sweep
//! gradients are rasterised and embedded as PNG.

use std::fmt::Write as _;

use kurbo::{Affine, BezPath, PathEl, Rect, Shape};
use peniko::color::Srgb;
use peniko::{BlendMode, Color, Extend, Fill, Gradient, GradientKind, ImageQuality, Mix};
use render::exec::{scale as xf_scale, transform_vector};
use render::{Brush, Cmd, Effect};
use rustc_hash::FxHashMap;

use crate::glyphs::Outlines;
use crate::raster;
use crate::{Content, ExportError, Item};

/// A number with at most 4 decimals, without trailing zeros.
pub(crate) fn num(v: f64) -> String {
    if !v.is_finite() {
        return "0".into();
    }
    let s = format!("{:.4}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" || s.is_empty() { "0".into() } else { s.to_string() }
}

pub(crate) fn path_data(p: &BezPath) -> String {
    let mut d = String::new();
    let pt = |d: &mut String, p: kurbo::Point| {
        let _ = write!(d, "{} {}", num(p.x), num(p.y));
    };
    for el in p.elements() {
        match *el {
            PathEl::MoveTo(a) => {
                d.push('M');
                pt(&mut d, a);
            }
            PathEl::LineTo(a) => {
                d.push('L');
                pt(&mut d, a);
            }
            PathEl::QuadTo(a, b) => {
                d.push('Q');
                pt(&mut d, a);
                d.push(' ');
                pt(&mut d, b);
            }
            PathEl::CurveTo(a, b, c) => {
                d.push('C');
                pt(&mut d, a);
                d.push(' ');
                pt(&mut d, b);
                d.push(' ');
                pt(&mut d, c);
            }
            PathEl::ClosePath => d.push('Z'),
        }
    }
    d
}

fn matrix(xf: Affine) -> String {
    if xf == Affine::IDENTITY {
        return String::new();
    }
    let c = xf.as_coeffs();
    format!(" transform=\"matrix({} {} {} {} {} {})\"", num(c[0]), num(c[1]), num(c[2]), num(c[3]), num(c[4]), num(c[5]))
}

/// `#rrggbb` and the alpha of a straight-alpha sRGB colour.
pub(crate) fn rgb(c: Color) -> (String, f32) {
    let [r, g, b, a] = c.components;
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    (format!("#{:02x}{:02x}{:02x}", q(r), q(g), q(b)), a.clamp(0.0, 1.0))
}

fn blend_name(b: BlendMode) -> Option<&'static str> {
    Some(match b.mix {
        Mix::Normal => return None,
        Mix::Multiply => "multiply",
        Mix::Screen => "screen",
        Mix::Overlay => "overlay",
        Mix::Darken => "darken",
        Mix::Lighten => "lighten",
        Mix::ColorDodge => "color-dodge",
        Mix::ColorBurn => "color-burn",
        Mix::HardLight => "hard-light",
        Mix::SoftLight => "soft-light",
        Mix::Difference => "difference",
        Mix::Exclusion => "exclusion",
        Mix::Hue => "hue",
        Mix::Saturation => "saturation",
        Mix::Color => "color",
        Mix::Luminosity => "luminosity",
        #[allow(unreachable_patterns, deprecated)]
        _ => return None,
    })
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub(crate) fn base64(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= c.len() {
                out.push(B64[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn png_rgba(rgba: &[u8], w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let enc = image::codecs::png::PngEncoder::new(&mut out);
    let _ = image::ImageEncoder::write_image(enc, rgba, w, h, image::ExtendedColorType::Rgba8);
    out
}

/// A paint that SVG can't express: drawn as a raster image instead.
fn needs_raster(b: &Brush) -> bool {
    matches!(b, Brush::Gradient(g) if matches!(g.kind, GradientKind::Sweep(_)))
}

struct Writer<'c> {
    content: &'c Content,
    scale: f64,
    defs: String,
    body: String,
    next: usize,
    images: FxHashMap<u64, String>,
    outlines: Outlines,
    error: Option<ExportError>,
}

impl Writer<'_> {
    fn id(&mut self, prefix: &str) -> String {
        self.next += 1;
        format!("{prefix}{}", self.next)
    }

    fn gradient(&mut self, g: &Gradient, brush_xf: Affine) -> String {
        let spread = match g.extend {
            Extend::Pad => "",
            Extend::Repeat => " spreadMethod=\"repeat\"",
            Extend::Reflect => " spreadMethod=\"reflect\"",
        };
        let gt = matrix(brush_xf).replace("transform=", "gradientTransform=");
        let id = self.id("g");
        match g.kind {
            GradientKind::Linear(l) => {
                let _ = write!(
                    self.defs,
                    "<linearGradient id=\"{id}\" gradientUnits=\"userSpaceOnUse\" x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\"{gt}{spread}>",
                    num(l.start.x),
                    num(l.start.y),
                    num(l.end.x),
                    num(l.end.y)
                );
            }
            GradientKind::Radial(r) => {
                let fr = if r.start_radius > 0.0 { format!(" fr=\"{}\"", num(f64::from(r.start_radius))) } else { String::new() };
                let _ = write!(
                    self.defs,
                    "<radialGradient id=\"{id}\" gradientUnits=\"userSpaceOnUse\" cx=\"{}\" cy=\"{}\" r=\"{}\" fx=\"{}\" fy=\"{}\"{fr}{gt}{spread}>",
                    num(r.end_center.x),
                    num(r.end_center.y),
                    num(f64::from(r.end_radius)),
                    num(r.start_center.x),
                    num(r.start_center.y)
                );
            }
            GradientKind::Sweep(_) => {}
        }
        for s in g.stops.iter() {
            let (c, a) = rgb(s.color.to_alpha_color::<Srgb>());
            let op = if a < 1.0 { format!(" stop-opacity=\"{}\"", num(f64::from(a))) } else { String::new() };
            let _ = write!(self.defs, "<stop offset=\"{}\" stop-color=\"{c}\"{op}/>", num(f64::from(s.offset)));
        }
        self.defs.push_str(if matches!(g.kind, GradientKind::Linear(_)) { "</linearGradient>" } else { "</radialGradient>" });
        format!("url(#{id})")
    }

    fn image_def(&mut self, img: &render::Image) -> String {
        if let Some(id) = self.images.get(&img.key) {
            return id.clone();
        }
        let id = self.id("i");
        let data = base64(&png_rgba(img.rgba.data(), img.width, img.height));
        let _ = write!(self.defs, "<image id=\"{id}\" width=\"{}\" height=\"{}\" preserveAspectRatio=\"none\" xlink:href=\"data:image/png;base64,{data}\"/>", img.width, img.height);
        self.images.insert(img.key, id.clone());
        id
    }

    /// The value of a `fill` or `stroke` attribute, plus its opacity attribute.
    fn paint(&mut self, b: &Brush, brush_xf: Affine, what: &str) -> String {
        match b {
            Brush::Solid(c) => {
                let (c, a) = rgb(*c);
                if a < 1.0 { format!("{what}=\"{c}\" {what}-opacity=\"{}\"", num(f64::from(a))) } else { format!("{what}=\"{c}\"") }
            }
            Brush::Gradient(g) => format!("{what}=\"{}\"", self.gradient(g, brush_xf)),
            Brush::Image(ip) => {
                let img = self.image_def(&ip.image);
                let id = self.id("p");
                let pt = matrix(brush_xf).replace("transform=", "patternTransform=");
                let op = if ip.alpha < 1.0 { format!(" opacity=\"{}\"", num(f64::from(ip.alpha))) } else { String::new() };
                let style = if ip.quality == ImageQuality::Low { " style=\"image-rendering:pixelated\"" } else { "" };
                let _ = write!(
                    self.defs,
                    "<pattern id=\"{id}\" patternUnits=\"userSpaceOnUse\" width=\"{}\" height=\"{}\"{pt}><use xlink:href=\"#{img}\"{op}{style}/></pattern>",
                    ip.image.width, ip.image.height
                );
                format!("{what}=\"url(#{id})\"")
            }
        }
    }

    fn clip(&mut self, path: &BezPath, rule: Fill, xf: Affine) -> String {
        let id = self.id("c");
        let cr = if rule == Fill::EvenOdd { " clip-rule=\"evenodd\"" } else { "" };
        let _ = write!(self.defs, "<clipPath id=\"{id}\" clipPathUnits=\"userSpaceOnUse\"><path d=\"{}\"{}{cr}/></clipPath>", path_data(path), matrix(xf));
        id
    }

    /// Embeds pixels rendered for the export-space rectangle `r`.
    fn raster(&mut self, items: &[Item], r: Rect) {
        let s = self.scale;
        // Whole pixels, so the image lands on the pixel grid at the export scale.
        let r = raster::snap(self.content, r, s);
        if r.width() <= 0.0 || r.height() <= 0.0 {
            return;
        }
        match raster::region(items, r, s) {
            Ok(px) => {
                let data = base64(&render_cpu::png(&px));
                let _ = write!(
                    self.body,
                    "<image x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" preserveAspectRatio=\"none\" xlink:href=\"data:image/png;base64,{data}\"/>",
                    num(r.x0),
                    num(r.y0),
                    num(r.width()),
                    num(r.height())
                );
            }
            Err(e) => self.error = Some(e),
        }
    }

    fn shape(&mut self, c: &Cmd, xf: Affine) {
        match c {
            Cmd::Fill { path, rule, xf: l, brush, brush_xf } => {
                if needs_raster(brush) {
                    let r = (xf * *l).transform_rect_bbox(path.bounding_box());
                    return self.raster(&[raster::item(vec![c.clone()], xf)], r);
                }
                let fr = if *rule == Fill::EvenOdd { " fill-rule=\"evenodd\"" } else { "" };
                // A placed (not tiled) image: the image itself, clipped to the shape.
                if let Brush::Image(ip) = brush
                    && ip.extend == Extend::Pad
                {
                    let img = self.image_def(&ip.image);
                    let clip = self.clip(path, *rule, xf * *l);
                    let op = if ip.alpha < 1.0 { format!(" opacity=\"{}\"", num(f64::from(ip.alpha))) } else { String::new() };
                    let style = if ip.quality == ImageQuality::Low { " style=\"image-rendering:pixelated\"" } else { "" };
                    let _ = write!(self.body, "<g clip-path=\"url(#{clip})\"><use xlink:href=\"#{img}\"{op}{style}{}/></g>", matrix(xf * *l * *brush_xf));
                    return;
                }
                let paint = self.paint(brush, *brush_xf, "fill");
                let _ = write!(self.body, "<path d=\"{}\" {paint}{fr}{}/>", path_data(path), matrix(xf * *l));
            }
            Cmd::Stroke { path, style, xf: l, brush, brush_xf } => {
                if needs_raster(brush) {
                    let r = (xf * *l).transform_rect_bbox(path.bounding_box().inflate(style.width * 2.0, style.width * 2.0));
                    return self.raster(&[raster::item(vec![c.clone()], xf)], r);
                }
                let paint = self.paint(brush, *brush_xf, "stroke");
                let mut a = format!(" fill=\"none\" {paint} stroke-width=\"{}\"", num(style.width));
                let cap = match style.start_cap {
                    kurbo::Cap::Butt => "",
                    kurbo::Cap::Round => " stroke-linecap=\"round\"",
                    kurbo::Cap::Square => " stroke-linecap=\"square\"",
                };
                a.push_str(cap);
                match style.join {
                    kurbo::Join::Miter => {
                        if (style.miter_limit - 4.0).abs() > 1e-9 {
                            let _ = write!(a, " stroke-miterlimit=\"{}\"", num(style.miter_limit.max(1.0)));
                        }
                    }
                    kurbo::Join::Round => a.push_str(" stroke-linejoin=\"round\""),
                    kurbo::Join::Bevel => a.push_str(" stroke-linejoin=\"bevel\""),
                }
                if !style.dash_pattern.is_empty() {
                    let d: Vec<String> = style.dash_pattern.iter().map(|v| num(*v)).collect();
                    let _ = write!(a, " stroke-dasharray=\"{}\"", d.join(" "));
                    if style.dash_offset != 0.0 {
                        let _ = write!(a, " stroke-dashoffset=\"{}\"", num(style.dash_offset));
                    }
                }
                let _ = write!(self.body, "<path d=\"{}\"{a}{}/>", path_data(path), matrix(xf * *l));
            }
            Cmd::Glyphs { run, xf: l, brush, brush_xf } => {
                let path = self.outlines.run(run);
                if path.elements().is_empty() {
                    return;
                }
                if needs_raster(brush) {
                    let r = (xf * *l).transform_rect_bbox(path.bounding_box());
                    return self.raster(&[raster::item(vec![c.clone()], xf)], r);
                }
                let paint = self.paint(brush, *brush_xf, "fill");
                let _ = write!(self.body, "<path d=\"{}\" {paint}{}/>", path_data(&path), matrix(xf * *l));
            }
            Cmd::BlurredRect { rect, radius, sigma, color, xf: l } => {
                let id = self.id("f");
                let m = 3.0 * sigma;
                let fr = rect.inflate(m, m);
                let _ = write!(
                    self.defs,
                    "<filter id=\"{id}\" filterUnits=\"userSpaceOnUse\" x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\"><feGaussianBlur stdDeviation=\"{}\"/></filter>",
                    num(fr.x0),
                    num(fr.y0),
                    num(fr.width()),
                    num(fr.height()),
                    num(*sigma)
                );
                let (c, a) = rgb(*color);
                let op = if a < 1.0 { format!(" fill-opacity=\"{}\"", num(f64::from(a))) } else { String::new() };
                let rr = if *radius > 0.0 { format!(" rx=\"{}\"", num(*radius)) } else { String::new() };
                let _ = write!(
                    self.body,
                    "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\"{rr} fill=\"{c}\"{op} filter=\"url(#{id})\"{}/>",
                    num(rect.x0),
                    num(rect.y0),
                    num(rect.width()),
                    num(rect.height()),
                    matrix(xf * *l)
                );
            }
            _ => {}
        }
    }

    /// Opens a filter group for an effect region (`bounds` in list space).
    fn effect(&mut self, e: &Effect, bounds: Rect, xf: Affine, l: Affine) {
        let full = xf * l;
        let s = xf_scale(full);
        let r = xf.transform_rect_bbox(bounds);
        let v = |o: (f64, f64)| transform_vector(full, kurbo::Vec2::new(o.0, o.1));
        let id = self.id("f");
        let (filter, region) = match *e {
            Effect::LayerBlur { sigma } => (format!("<feGaussianBlur in=\"SourceGraphic\" stdDeviation=\"{}\"/>", num(sigma * s)), r),
            Effect::DropShadow { color, offset, sigma, spread } => {
                let d = v(offset);
                let (c, a) = rgb(color);
                let morph = morphology("SourceAlpha", spread * s, "dilate");
                let src = if morph.is_empty() { "SourceAlpha" } else { "m" };
                (
                    format!(
                        "{morph}<feOffset in=\"{src}\" dx=\"{}\" dy=\"{}\" result=\"o\"/><feGaussianBlur in=\"o\" stdDeviation=\"{}\" result=\"b\"/><feFlood flood-color=\"{c}\" flood-opacity=\"{}\"/><feComposite in2=\"b\" operator=\"in\"/>",
                        num(d.x),
                        num(d.y),
                        num(sigma * s),
                        num(f64::from(a))
                    ),
                    r,
                )
            }
            Effect::InnerShadow { color, offset, sigma, spread } => {
                let d = v(offset);
                let (c, a) = rgb(color);
                // The outside counts as 1 when blurred inward: give the filter room around it.
                let reach = 3.0 * sigma * s + d.x.abs().max(d.y.abs()) + (spread * s).max(0.0) + 1.0;
                let morph = morphology("inv", spread * s, "dilate");
                let src = if morph.is_empty() { "inv" } else { "m" };
                (
                    format!(
                        "<feComponentTransfer in=\"SourceAlpha\" result=\"inv\"><feFuncA type=\"table\" tableValues=\"1 0\"/></feComponentTransfer>{morph}<feOffset in=\"{src}\" dx=\"{}\" dy=\"{}\" result=\"o\"/><feGaussianBlur in=\"o\" stdDeviation=\"{}\" result=\"b\"/><feFlood flood-color=\"{c}\" flood-opacity=\"{}\"/><feComposite in2=\"b\" operator=\"in\"/><feComposite in2=\"SourceAlpha\" operator=\"in\"/>",
                        num(d.x),
                        num(d.y),
                        num(sigma * s),
                        num(f64::from(a))
                    ),
                    r.inflate(reach, reach),
                )
            }
        };
        let _ = write!(
            self.defs,
            "<filter id=\"{id}\" filterUnits=\"userSpaceOnUse\" x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" color-interpolation-filters=\"sRGB\">{filter}</filter>",
            num(region.x0),
            num(region.y0),
            num(region.width()),
            num(region.height())
        );
        // The renderers clip effect content and results to the region's bounds.
        let clip = self.clip(&r.to_path(0.1), Fill::NonZero, Affine::IDENTITY);
        let _ = write!(self.body, "<g clip-path=\"url(#{clip})\"><g filter=\"url(#{id})\">");
    }

    /// Background blur: what is drawn so far (rasterised), blurred, inside the clip.
    fn backdrop(&mut self, item: usize, at: usize, open: &[&Cmd]) {
        let Some((items, r, clip, full)) = raster::backdrop(self.content, item, at, open) else { return };
        let id = self.clip(&clip, Fill::NonZero, full);
        let _ = write!(self.body, "<g clip-path=\"url(#{id})\">");
        self.raster(&items, r);
        self.body.push_str("</g>");
    }

    fn item(&mut self, index: usize) {
        let it = self.content.items[index].clone();
        let xf = it.xf;
        let mut open: Vec<&Cmd> = Vec::new();
        for (i, c) in it.list.cmds.iter().enumerate() {
            match c {
                Cmd::PushLayer { clip, rule, xf: l, blend, alpha } => {
                    let id = self.clip(clip, *rule, xf * *l);
                    let mut a = format!("<g clip-path=\"url(#{id})\"");
                    if *alpha < 1.0 {
                        let _ = write!(a, " opacity=\"{}\"", num(f64::from(*alpha)));
                    }
                    if let Some(m) = blend_name(*blend) {
                        let _ = write!(a, " style=\"mix-blend-mode:{m}\"");
                    }
                    a.push('>');
                    self.body.push_str(&a);
                    open.push(c);
                }
                Cmd::PopLayer => {
                    if open.pop().is_some() {
                        self.body.push_str("</g>");
                    }
                }
                Cmd::PushEffect { effect, bounds, xf: l } => {
                    self.effect(effect, *bounds, xf, *l);
                    open.push(c);
                }
                Cmd::PopEffect => {
                    if open.pop().is_some() {
                        self.body.push_str("</g></g>");
                    }
                }
                Cmd::BackdropBlur { .. } => self.backdrop(index, i, &open),
                other => self.shape(other, xf),
            }
        }
        for c in open.iter().rev() {
            self.body.push_str(if matches!(c, Cmd::PushEffect { .. }) { "</g></g>" } else { "</g>" });
        }
    }
}

fn morphology(input: &str, r: f64, op: &str) -> String {
    if r.abs() < 1e-6 {
        return String::new();
    }
    let op = if r < 0.0 { if op == "dilate" { "erode" } else { "dilate" } } else { op };
    format!("<feMorphology in=\"{input}\" operator=\"{op}\" radius=\"{}\" result=\"m\"/>", num(r.abs()))
}

/// Writes `content` as an SVG document, `scale` pixels per unit.
pub fn write(content: &Content, scale: f64) -> Result<String, ExportError> {
    let mut w = Writer { content, scale, defs: String::new(), body: String::new(), next: 0, images: FxHashMap::default(), outlines: Outlines::default(), error: None };
    for i in 0..content.items.len() {
        w.item(i);
    }
    if let Some(e) = w.error {
        return Err(e);
    }
    let (pw, ph) = (content.width * scale, content.height * scale);
    let mut out = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" width=\"{}\" height=\"{}\" viewBox=\"0 0 {} {}\">",
        num(pw),
        num(ph),
        num(content.width),
        num(content.height)
    );
    if !w.defs.is_empty() {
        let _ = write!(out, "<defs>{}</defs>", w.defs);
    }
    out.push_str(&w.body);
    out.push_str("</svg>\n");
    Ok(out)
}
