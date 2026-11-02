//! Vello encoding of the shared spike scene, used by the renderer spike and both shell spikes.

use common::*;
use std::sync::Arc;
use vello::kurbo::{Affine, BezPath, Ellipse, Rect as KRect, RoundedRect, Stroke};
use vello::peniko::{Blob, Color, Fill, ImageAlphaType, ImageBrush, ImageData as PImage, ImageFormat};
use vello::Scene as VScene;


/// Retained, renderer-specific data built once (paths, image brushes).
pub struct Cache {
    pub icons: Vec<BezPath>,
    pub images: Vec<ImageBrush>,
}

impl Cache {
    pub fn new(scene: &Scene) -> Self {
        let icons = scene.icons.iter().map(|els| to_bez(els)).collect();
        let images = scene
            .images
            .iter()
            .map(|img| {
                ImageBrush::new(PImage {
                    data: Blob::new(Arc::new(img.rgba.clone())),
                    format: ImageFormat::Rgba8,
                    alpha_type: ImageAlphaType::Alpha,
                    width: img.width,
                    height: img.height,
                })
            })
            .collect();
        Cache { icons, images }
    }
}

pub fn to_bez(els: &[PathEl]) -> BezPath {
    let mut p = BezPath::new();
    for e in els {
        match *e {
            PathEl::Move(x, y) => p.move_to((x as f64, y as f64)),
            PathEl::Line(x, y) => p.line_to((x as f64, y as f64)),
            PathEl::Cubic(a, b, c, d, e, f) => p.curve_to((a as f64, b as f64), (c as f64, d as f64), (e as f64, f as f64)),
            PathEl::Close => p.close_path(),
        }
    }
    p
}

pub fn color(c: Rgba, opacity: f32) -> Color {
    Color::from_rgba8(c[0], c[1], c[2], (c[3] as f32 * opacity) as u8)
}

/// Encode the visible items into a Vello scene. Layer blur is not supported by Vello's GPU
/// pipeline (0.11); blurred items are drawn sharp and counted in the report.
pub fn encode(vs: &mut VScene, scene: &Scene, visible: impl Iterator<Item = (u32, Lod)>, view: Affine, cache: &Cache) {
    for (i, lod) in visible {
        let it = &scene.items[i as usize];
        let r = KRect::new(it.rect.x, it.rect.y, it.rect.x + it.rect.w, it.rect.y + it.rect.h);
        let fill = color(it.fill, it.opacity);
        if lod == Lod::Greek {
            let g = KRect::new(r.x0, r.y0 + r.height() * 0.2, r.x1, r.y1 - r.height() * 0.2);
            vs.fill(Fill::NonZero, view, color(it.fill, 0.35), None, &g);
            continue;
        }
        if lod == Lod::Full {
            if let Some(sh) = it.shadow {
                vs.draw_blurred_rounded_rect(
                    view,
                    r + vello::kurbo::Vec2::new(sh.dx as f64, sh.dy as f64),
                    color(sh.color, 1.0),
                    it.radius as f64,
                    sh.blur as f64 / 2.0,
                );
            }
        }
        match it.kind {
            Kind::Rect => {
                let rr = RoundedRect::from_rect(r, it.radius as f64);
                vs.fill(Fill::NonZero, view, fill, None, &rr);
                if let (Some((sc, sw)), Lod::Full) = (it.stroke, lod) {
                    vs.stroke(&Stroke::new(sw as f64), view, color(sc, it.opacity), None, &rr);
                }
            }
            Kind::Ellipse => {
                let e = Ellipse::from_rect(r);
                vs.fill(Fill::NonZero, view, fill, None, &e);
                if let (Some((sc, sw)), Lod::Full) = (it.stroke, lod) {
                    vs.stroke(&Stroke::new(sw as f64), view, color(sc, it.opacity), None, &e);
                }
            }
            Kind::Path(p) => {
                let t = view * Affine::translate((r.x0, r.y0)) * Affine::scale_non_uniform(r.width(), r.height());
                vs.fill(Fill::NonZero, t, fill, None, &cache.icons[p as usize]);
            }
            Kind::Image(m) => {
                let img = &cache.images[m as usize];
                let t = view * Affine::translate((r.x0, r.y0)) * Affine::scale_non_uniform(r.width() / img.image.width as f64, r.height() / img.image.height as f64);
                if it.radius > 0.0 && lod == Lod::Full {
                    let rr = RoundedRect::from_rect(r, it.radius as f64);
                    vs.push_clip_layer(Fill::NonZero, view, &rr);
                    vs.draw_image(img, t);
                    vs.pop_layer();
                } else {
                    vs.draw_image(img, t);
                }
            }
            Kind::Text(ti) => {
                let tl = &scene.texts[ti as usize];
                let t = view * Affine::translate((r.x0, r.y0));
                for run in &tl.runs {
                    vs.draw_glyphs(&run.font)
                        .font_size(run.size)
                        .transform(t)
                        .brush(fill)
                        .draw(Fill::NonZero, run.glyphs.iter().map(|&(id, x, y)| vello::Glyph { id, x, y }));
                }
            }
        }
    }
}

