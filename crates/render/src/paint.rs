//! Document paints → display-list brushes. Gradient geometry is defined in the node's unit
//! square and mapped onto its box; colours blend and interpolate in sRGB.

use std::sync::Arc;

use doc::model::{BlendMode as DocBlend, Color as DocColor, GradientStop, ImageFit, Paint, PaintKind};
use kurbo::{Affine, Rect};
use peniko::color::{ColorSpaceTag, HueDirection};
use peniko::{BlendMode, Color, ColorStop, Compose, Extend, Gradient, ImageQuality, Mix};

use crate::list::{Brush, ImagePaint};

pub fn color(c: DocColor, opacity: f64) -> Color {
    Color::from_rgba8(c.r, c.g, c.b, c.a).multiply_alpha(opacity.clamp(0.0, 1.0) as f32)
}

pub fn blend(b: DocBlend) -> BlendMode {
    let mix = match b {
        DocBlend::Normal => Mix::Normal,
        DocBlend::Darken => Mix::Darken,
        DocBlend::Multiply => Mix::Multiply,
        DocBlend::ColorBurn => Mix::ColorBurn,
        DocBlend::Lighten => Mix::Lighten,
        DocBlend::Screen => Mix::Screen,
        DocBlend::ColorDodge => Mix::ColorDodge,
        DocBlend::Overlay => Mix::Overlay,
        DocBlend::SoftLight => Mix::SoftLight,
        DocBlend::HardLight => Mix::HardLight,
        DocBlend::Difference => Mix::Difference,
        DocBlend::Exclusion => Mix::Exclusion,
        DocBlend::Hue => Mix::Hue,
        DocBlend::Saturation => Mix::Saturation,
        DocBlend::Color => Mix::Color,
        DocBlend::Luminosity => Mix::Luminosity,
    };
    BlendMode::new(mix, Compose::SrcOver)
}

fn stops(s: &[GradientStop], opacity: f64) -> Vec<ColorStop> {
    let mut v: Vec<ColorStop> = s.iter().map(|g| ColorStop::from((g.offset.clamp(0.0, 1.0) as f32, color(g.color, opacity)))).collect();
    v.sort_by(|a, b| a.offset.total_cmp(&b.offset));
    v
}

fn srgb(g: Gradient) -> Gradient {
    let mut g = g.with_interpolation_cs(ColorSpaceTag::Srgb);
    g.hue_direction = HueDirection::Shorter;
    g
}

/// How an image of `iw × ih` pixels sits in a `w × h` box, and the part of the box it covers.
pub fn image_placement(fit: ImageFit, iw: f64, ih: f64, w: f64, h: f64) -> (Affine, Rect, Extend) {
    let full = Rect::new(0.0, 0.0, w, h);
    if iw <= 0.0 || ih <= 0.0 {
        return (Affine::IDENTITY, full, Extend::Pad);
    }
    match fit {
        ImageFit::Fill => {
            let s = (w / iw).max(h / ih);
            (Affine::translate(((w - iw * s) / 2.0, (h - ih * s) / 2.0)) * Affine::scale(s), full, Extend::Pad)
        }
        ImageFit::Fit => {
            let s = (w / iw).min(h / ih);
            let (dx, dy) = ((w - iw * s) / 2.0, (h - ih * s) / 2.0);
            (Affine::translate((dx, dy)) * Affine::scale(s), Rect::new(dx, dy, dx + iw * s, dy + ih * s), Extend::Pad)
        }
        ImageFit::Crop => (Affine::scale_non_uniform(w / iw, h / ih), full, Extend::Pad),
        ImageFit::Tile => (Affine::IDENTITY, full, Extend::Repeat),
    }
}

/// A brush for a non-image paint on a `w × h` box: `(brush, brush transform)`.
pub fn brush(p: &Paint, w: f64, h: f64) -> Option<(Brush, Affine)> {
    let unit = Affine::scale_non_uniform(w, h);
    Some(match &p.kind {
        PaintKind::Solid { color: c } => (Brush::Solid(color(*c, p.opacity)), Affine::IDENTITY),
        PaintKind::LinearGradient { stops: s, start, end } => {
            (Brush::Gradient(srgb(Gradient::new_linear((start[0], start[1]), (end[0], end[1])).with_stops(stops(s, p.opacity).as_slice()))), unit)
        }
        PaintKind::RadialGradient { stops: s, center, radius } => {
            let xf = unit * Affine::translate((center[0], center[1])) * Affine::scale_non_uniform(radius[0].max(1e-6), radius[1].max(1e-6));
            (Brush::Gradient(srgb(Gradient::new_radial((0.0, 0.0), 1.0).with_stops(stops(s, p.opacity).as_slice()))), xf)
        }
        PaintKind::AngularGradient { stops: s, center, angle } => {
            let a = angle.to_radians() as f32;
            let g = Gradient::new_sweep((center[0], center[1]), a, a + std::f32::consts::TAU).with_stops(stops(s, p.opacity).as_slice());
            (Brush::Gradient(srgb(g)), unit)
        }
        PaintKind::Image { .. } => return None,
    })
}

/// An image brush, placed for `fit`. Returns the brush, its transform and the region to fill.
pub fn image_brush(img: Arc<crate::list::Image>, full_w: u32, full_h: u32, fit: ImageFit, opacity: f64, w: f64, h: f64) -> (Brush, Affine, Rect) {
    let (place, region, extend) = image_placement(fit, full_w as f64, full_h as f64, w, h);
    // A smaller mip covers the same area: scale it up to the full image's size.
    let mip = Affine::scale_non_uniform(full_w as f64 / img.width as f64, full_h as f64 / img.height as f64);
    let paint = ImagePaint { image: img, extend, quality: ImageQuality::Medium, alpha: opacity.clamp(0.0, 1.0) as f32 };
    (Brush::Image(paint), place * mip, region)
}

/// The neutral checker drawn while an image isn't available.
pub fn placeholder() -> Brush {
    Brush::Solid(Color::from_rgba8(0xd0, 0xd0, 0xd0, 0xff))
}
