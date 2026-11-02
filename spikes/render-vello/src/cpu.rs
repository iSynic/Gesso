//! vello_cpu as a headless reference renderer: 4K exports of two viewports.

use crate::{color, to_bez};
use common::*;
use std::sync::Arc;
use vello_cpu::kurbo::{Affine, BezPath, Ellipse, Rect as KRect, RoundedRect, Shape, Stroke};

use vello_cpu::{Pixmap, RenderContext, RenderSettings, Resources};

pub const EW: u32 = 3840;
pub const EH: u32 = 2160;

struct CpuCache {
    icons: Vec<BezPath>,
    images: Vec<(vello_cpu::Image, u32, u32)>,
}

fn draw(ctx: &mut RenderContext, res: &mut Resources, scene: &Scene, visible: &[(u32, Lod)], cam: &Camera, cache: &CpuCache) {
    let view = Affine::new(cam.affine());
    for &(i, lod) in visible {
        let it = &scene.items[i as usize];
        let r = KRect::new(it.rect.x, it.rect.y, it.rect.x + it.rect.w, it.rect.y + it.rect.h);
        ctx.set_transform(view);
        if lod == Lod::Greek {
            ctx.set_paint(color(it.fill, 0.35));
            ctx.fill_rect(&KRect::new(r.x0, r.y0 + r.height() * 0.2, r.x1, r.y1 - r.height() * 0.2));
            continue;
        }
        if let (Some(sh), Lod::Full) = (it.shadow, lod) {
            ctx.set_paint(color(sh.color, 1.0));
            ctx.fill_blurred_rounded_rect(&(r + vello_cpu::kurbo::Vec2::new(sh.dx as f64, sh.dy as f64)), it.radius, sh.blur / 2.0, false);
        }
        let blur = it.blur > 0.0 && lod == Lod::Full;
        if blur {
            // Single-threaded vello_cpu supports simple filter layers.
            ctx.push_filter_layer(vello_cpu::filter_effects::Filter::from_primitive(
                vello_cpu::filter_effects::FilterPrimitive::GaussianBlur { std_deviation: it.blur * cam.zoom as f32, edge_mode: vello_cpu::filter_effects::EdgeMode::None },
            ));
        }
        ctx.set_paint(color(it.fill, it.opacity));
        match it.kind {
            Kind::Rect | Kind::Ellipse => {
                let path = if it.kind == Kind::Rect { RoundedRect::from_rect(r, it.radius as f64).to_path(0.1) } else { Ellipse::from_rect(r).to_path(0.1) };
                ctx.fill_path(&path);
                if let (Some((sc, sw)), Lod::Full) = (it.stroke, lod) {
                    ctx.set_paint(color(sc, it.opacity));
                    ctx.set_stroke(Stroke::new(sw as f64));
                    ctx.stroke_path(&path);
                }
            }
            Kind::Path(p) => {
                ctx.set_transform(view * Affine::translate((r.x0, r.y0)) * Affine::scale_non_uniform(r.width(), r.height()));
                ctx.fill_path(&cache.icons[p as usize]);
            }
            Kind::Image(m) => {
                let (img, w, h) = &cache.images[m as usize];
                ctx.set_paint(img.clone());
                ctx.set_paint_transform(Affine::translate((r.x0, r.y0)) * Affine::scale_non_uniform(r.width() / *w as f64, r.height() / *h as f64));
                ctx.fill_path(&RoundedRect::from_rect(r, it.radius as f64).to_path(0.1));
                ctx.reset_paint_transform();
            }
            Kind::Text(ti) => {
                ctx.set_transform(view * Affine::translate((r.x0, r.y0)));
                for run in &scene.texts[ti as usize].runs {
                    let _ = ctx
                        .glyph_run(res, &run.font)
                        .font_size(run.size)
                        .fill_glyphs(run.glyphs.iter().map(|&(id, x, y)| vello_cpu::Glyph { id, x, y }));
                }
            }
        }
        if blur {
            ctx.pop_layer();
        }
    }
}

pub fn run(args: &Args, scene: &Scene) {
    let mut res = Resources::new();
    let mut ctx = RenderContext::new_with(EW as u16, EH as u16, RenderSettings::default());
    let images = scene
        .images
        .iter()
        .map(|img| {
            let mut px = Pixmap::new(img.width as u16, img.height as u16);
            px.data_as_u8_slice_mut().copy_from_slice(&img.rgba);
            let id = res.register_image(Arc::new(px));
            (vello_cpu::Image { image: vello_cpu::ImageSource::OpaqueId { id, may_have_transparency: false }, sampler: Default::default() }, img.width, img.height)
        })
        .collect();
    let cache = CpuCache { icons: scene.icons.iter().map(|e| to_bez(e)).collect(), images };
    let mut culler = Culler::new(true);
    let mut pixmap = Pixmap::new(EW as u16, EH as u16);
    let mut results = Vec::new();
    // Same world views as the GPU scenario at 1920x1080, rendered at 2x.
    for (name, zoom, fx) in [("detail_100pct", 1.0, 0.5), ("overview_15pct", 0.15, 0.5)] {
        let c = scenario::centered(scene, crate::W, crate::H, zoom, fx, 0.5);
        let cam = Camera { zoom: zoom * 2.0, ..c };
        let mut times = Vec::new();
        for _ in 0..5 {
            let t = Timer::start();
            culler.cull(scene, cam.view(EW, EH), cam.zoom);
            ctx.reset();
            draw(&mut ctx, &mut res, scene, &culler.visible, &cam, &cache);
            ctx.flush();
            ctx.render(&mut pixmap, &mut res);
            times.push(t.ms());
        }
        eprintln!("vello_cpu {name}: visible {} median {:.1} ms", culler.visible.len(), percentiles(times.clone()).p50);
        if let Some(dir) = args.value("--png") {
            let _ = std::fs::create_dir_all(dir);
            let rgba = pixmap.clone().take_rgba8(vello_cpu::peniko::ImageAlphaType::Alpha);
            image::save_buffer(format!("{dir}/vello_cpu_{name}.png"), &rgba, EW, EH, image::ColorType::Rgba8).unwrap();
        }
        results.push(serde_json::json!({ "view": name, "visible": culler.visible.len(), "ms": percentiles(times) }));
    }
    write_json(
        args.value("--out"),
        &serde_json::json!({ "renderer": "vello_cpu", "version": "0.3.0", "size": [EW, EH], "threads": "auto", "host": Host::detect(String::new(), "cpu".into()), "results": results }),
    );
}
