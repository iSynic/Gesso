//! Raster exports, and the rasterised fallbacks the vector writers embed.

use std::sync::Arc;

use image::ImageEncoder;
use kurbo::{Affine, BezPath, Rect, Shape};
use peniko::Color;
use render::exec::scale as xf_scale;
use render::{Cmd, DisplayList, Frame, FrameItem, Renderer};
use render_cpu::{CpuRenderer, Target};

use crate::{Content, ExportError, Format, Item};

/// Renders the whole content at `scale`, on a transparent background.
pub fn render(content: &Content, scale: f64) -> Result<Target, ExportError> {
    region(&content.items, Rect::new(0.0, 0.0, content.width, content.height), scale)
}

/// Renders `items` inside the export-space rectangle `rect` at `scale`, transparent elsewhere.
pub fn region(items: &[Item], rect: Rect, scale: f64) -> Result<Target, ExportError> {
    region_with(items, rect, scale, &mut CpuRenderer::default())
}

/// [`region`] with a given renderer (single-threaded ones, to render many regions in parallel).
pub(crate) fn region_with(items: &[Item], rect: Rect, scale: f64, renderer: &mut CpuRenderer) -> Result<Target, ExportError> {
    let (w, h) = ((rect.width() * scale).ceil().max(1.0) as u32, (rect.height() * scale).ceil().max(1.0) as u32);
    if w > crate::MAX_PX || h > crate::MAX_PX {
        return Err(ExportError::TooLarge(w, h));
    }
    let frame = Frame {
        width: w,
        height: h,
        camera: Affine::scale(scale) * Affine::translate((-rect.x0, -rect.y0)),
        background: Color::TRANSPARENT,
        items: items.iter().enumerate().map(|(i, it)| FrameItem { fragment: i as u64, version: 0, list: it.list.clone(), xf: it.xf, clips: Vec::new() }).collect(),
    };
    let mut px = Target::new(w as u16, h as u16);
    renderer.render(&frame, &mut px)?;
    Ok(px)
}

/// One list of commands as an item (for rasterising part of a list).
pub(crate) fn item(cmds: Vec<render::Cmd>, xf: Affine) -> Item {
    let mut list = DisplayList::default();
    for c in cmds {
        list.push(c);
    }
    Item { list: Arc::new(list), xf }
}

/// Straight-alpha RGBA8 pixels.
pub fn rgba(px: &Target) -> Vec<u8> {
    render_cpu::rgba(px)
}

/// Encodes rendered pixels. JPEG has no alpha: transparent areas become white.
pub fn encode(px: &Target, format: Format, quality: u8) -> Result<Vec<u8>, ExportError> {
    let (w, h) = (px.width() as u32, px.height() as u32);
    let mut out = Vec::new();
    let err = |e: image::ImageError| ExportError::Encode(e.to_string());
    match format {
        Format::Png => return Ok(render_cpu::png(px)),
        Format::Jpeg => {
            let rgb: Vec<u8> = rgba(px)
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| {
                    let a = u32::from(p[3]);
                    let on_white = |c: u8| ((u32::from(c) * a + 255 * (255 - a) + 127) / 255) as u8;
                    [on_white(p[0]), on_white(p[1]), on_white(p[2])]
                })
                .collect();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality.clamp(1, 100)).write_image(&rgb, w, h, image::ExtendedColorType::Rgb8).map_err(err)?;
        }
        Format::Webp => {
            image::codecs::webp::WebPEncoder::new_lossless(&mut out).write_image(&rgba(px), w, h, image::ExtendedColorType::Rgba8).map_err(err)?;
        }
        Format::Svg | Format::Pdf => return Err(ExportError::Encode(format!("{format:?} is not a raster format"))),
    }
    Ok(out)
}

/// `r` grown to whole pixels at `scale`, inside the content.
pub(crate) fn snap(content: &Content, r: Rect, scale: f64) -> Rect {
    let r = Rect::new((r.x0 * scale).floor() / scale, (r.y0 * scale).floor() / scale, (r.x1 * scale).ceil() / scale, (r.y1 * scale).ceil() / scale);
    r.intersect(Rect::new(0.0, 0.0, content.width, content.height))
}

/// A background blur, rasterised: what everything drawn before command `at` of item `item`
/// looks like blurred. Returns the items to render, the export-space area to render (the clip
/// with room for the blur), and the clip (path, path → export) to show the result in.
/// `open` are the layers and effects open at that command, outermost first.
pub(crate) fn backdrop(content: &Content, index: usize, at: usize, open: &[&Cmd]) -> Option<(Vec<Item>, Rect, Arc<BezPath>, Affine)> {
    let it = content.items.get(index)?;
    let Some(Cmd::BackdropBlur { clip, xf: l, sigma }) = it.list.cmds.get(at) else { return None };
    let full = it.xf * *l;
    let margin = 3.0 * sigma * xf_scale(full);
    let r = full.transform_rect_bbox(clip.bounding_box()).inflate(margin, margin);
    let mut items: Vec<Item> = content.items[..index].to_vec();
    let mut cmds: Vec<Cmd> = it.list.cmds[..at].to_vec();
    // Close what is open, as the renderers do before taking the snapshot.
    for c in open.iter().rev() {
        cmds.push(if matches!(c, Cmd::PushEffect { .. }) { Cmd::PopEffect } else { Cmd::PopLayer });
    }
    cmds.push(it.list.cmds[at].clone());
    items.push(item(cmds, it.xf));
    Some((items, r, clip.clone(), full))
}

/// The index of the `PopEffect` closing the effect pushed at `start` (or the list's end).
pub(crate) fn effect_end(cmds: &[Cmd], start: usize) -> usize {
    let mut depth = 0usize;
    for (i, c) in cmds.iter().enumerate().skip(start) {
        match c {
            Cmd::PushEffect { .. } => depth += 1,
            Cmd::PopEffect => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return i;
                }
            }
            _ => {}
        }
    }
    cmds.len().saturating_sub(1)
}
