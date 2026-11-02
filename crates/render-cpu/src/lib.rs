//! The CPU reference renderer, on vello_cpu: exports, headless rendering and golden images.
//! Effects use `render::blur`, the reference the GPU shaders follow.

use std::sync::Arc;

use doc::{Document, NodeId};
use kurbo::{Affine, BezPath, Rect};
use peniko::{BlendMode, Color, Fill, ImageAlphaType, ImageQuality, ImageSampler};
use render::blur::{self, Pixels};
use render::{Backend, Brush, Frame, IRect, Process, RenderError, RenderStats, Renderer, Scene};
use rustc_hash::FxHashMap;
use vello_cpu::{Image, ImageSource, Pixmap, RenderContext, RenderSettings, Resources};

pub use vello_cpu::Pixmap as Target;

struct Surface {
    ctx: RenderContext,
    origin: (i32, i32),
    w: u32,
    h: u32,
}

/// Renders frames into a [`Pixmap`].
pub struct CpuRenderer {
    threads: u16,
    resources: Resources,
    images: FxHashMap<u64, Arc<Pixmap>>,
}

impl Default for CpuRenderer {
    fn default() -> Self {
        Self::new(RenderSettings::default().num_threads)
    }
}

impl CpuRenderer {
    /// `threads` = worker threads for the frame (0 = render on the calling thread).
    pub fn new(threads: u16) -> Self {
        CpuRenderer { threads, resources: Resources::new(), images: FxHashMap::default() }
    }
}

fn settings(threads: u16) -> RenderSettings {
    RenderSettings { num_threads: threads, ..RenderSettings::default() }
}

fn context(w: u32, h: u32, threads: u16) -> RenderContext {
    RenderContext::new_with(w.clamp(1, u16::MAX as u32) as u16, h.clamp(1, u16::MAX as u32) as u16, settings(threads))
}

struct Cpu<'r> {
    stack: Vec<Surface>,
    res: &'r mut Resources,
    images: &'r mut FxHashMap<u64, Arc<Pixmap>>,
}

fn pixmap_of(p: &Pixels) -> Arc<Pixmap> {
    Arc::new(Pixmap::from_parts(
        p.data.clone(),
        p.w.max(1) as u16,
        p.h.max(1) as u16,
        vello_cpu::PixelMetadata { may_have_transparency: true, alpha_type: ImageAlphaType::AlphaPremultiplied },
    ))
}

fn pixels_of(px: &Pixmap, origin: (i32, i32)) -> Pixels {
    Pixels { x: origin.0, y: origin.1, w: px.width() as u32, h: px.height() as u32, data: px.data_as_u8_slice().to_vec() }
}

fn crop(p: &Pixels, r: IRect) -> Pixels {
    let mut out = Pixels::new(r.x, r.y, r.w, r.h);
    for y in 0..r.h as i32 {
        let sy = r.y + y - p.y;
        if sy < 0 || sy >= p.h as i32 {
            continue;
        }
        for x in 0..r.w as i32 {
            let sx = r.x + x - p.x;
            if sx < 0 || sx >= p.w as i32 {
                continue;
            }
            let (s, d) = (((sy as u32 * p.w + sx as u32) * 4) as usize, ((y as u32 * r.w + x as u32) * 4) as usize);
            out.data[d..d + 4].copy_from_slice(&p.data[s..s + 4]);
        }
    }
    out
}

impl Cpu<'_> {
    fn top(&mut self) -> &mut Surface {
        let n = self.stack.len() - 1;
        &mut self.stack[n]
    }

    fn at(&mut self, xf: Affine) -> &mut RenderContext {
        let s = self.top();
        let o = Affine::translate((-s.origin.0 as f64, -s.origin.1 as f64));
        s.ctx.set_transform(o * xf);
        &mut s.ctx
    }

    fn set_brush(&mut self, brush: &Brush, brush_xf: Affine) {
        let paint: vello_cpu::PaintType = match brush {
            Brush::Solid(c) => (*c).into(),
            Brush::Gradient(g) => g.clone().into(),
            Brush::Image(ip) => {
                let px = self
                    .images
                    .entry(ip.image.key)
                    .or_insert_with(|| {
                        Arc::new(Pixmap::from_parts(
                            ip.image.rgba.data().to_vec(),
                            ip.image.width as u16,
                            ip.image.height as u16,
                            vello_cpu::PixelMetadata { may_have_transparency: true, alpha_type: ImageAlphaType::Alpha },
                        ))
                    })
                    .clone();
                let sampler = ImageSampler { x_extend: ip.extend, y_extend: ip.extend, quality: ip.quality, alpha: ip.alpha };
                Image { image: ImageSource::Pixmap(px), sampler }.into()
            }
        };
        let ctx = &mut self.top().ctx;
        ctx.set_paint(paint);
        ctx.set_paint_transform(brush_xf);
    }

    fn render_surface(s: &mut Surface, res: &mut Resources) -> Pixmap {
        let mut px = Pixmap::new(s.w.max(1) as u16, s.h.max(1) as u16);
        s.ctx.flush();
        s.ctx.render(&mut px, res);
        px
    }
}

impl Backend for Cpu<'_> {
    type Surface = Pixels;

    fn fill(&mut self, path: &BezPath, rule: Fill, xf: Affine, brush: &Brush, brush_xf: Affine) {
        self.at(xf);
        self.set_brush(brush, brush_xf);
        let ctx = &mut self.top().ctx;
        ctx.set_fill_rule(rule);
        ctx.fill_path(path);
    }

    fn stroke(&mut self, path: &BezPath, style: &kurbo::Stroke, xf: Affine, brush: &Brush, brush_xf: Affine) {
        self.at(xf);
        self.set_brush(brush, brush_xf);
        let ctx = &mut self.top().ctx;
        ctx.set_stroke(style.clone());
        ctx.stroke_path(path);
    }

    fn glyphs(&mut self, run: &text::GlyphRun, xf: Affine, brush: &Brush, brush_xf: Affine) {
        self.at(xf);
        self.set_brush(brush, brush_xf);
        let skew = run.skew.map(|deg| Affine::skew((-deg as f64).to_radians().tan(), 0.0));
        let n = self.stack.len() - 1;
        let s = &mut self.stack[n];
        let mut b = s.ctx.glyph_run(self.res, &run.font).font_size(run.size).hint(false).normalized_coords(&run.coords);
        if let Some(k) = skew {
            b = b.glyph_transform(k);
        }
        let _ = b.fill_glyphs(run.glyphs.iter().map(|&(id, x, y)| vello_cpu::Glyph { id, x, y }));
    }

    fn blurred_rect(&mut self, rect: Rect, radius: f64, sigma: f64, color: Color, xf: Affine) {
        let ctx = self.at(xf);
        ctx.set_paint(color);
        ctx.set_paint_transform(Affine::IDENTITY);
        ctx.fill_blurred_rounded_rect(&rect, radius as f32, sigma as f32, false);
    }

    fn push_layer(&mut self, clip: &BezPath, rule: Fill, xf: Affine, blend: BlendMode, alpha: f32) {
        let ctx = self.at(xf);
        ctx.set_fill_rule(rule);
        ctx.push_layer(Some(clip), Some(blend), Some(alpha), None, None);
    }

    fn pop_layer(&mut self) {
        self.top().ctx.pop_layer();
    }

    fn begin_offscreen(&mut self, r: IRect) {
        self.stack.push(Surface { ctx: context(r.w, r.h, 0), origin: (r.x, r.y), w: r.w, h: r.h });
    }

    fn end_offscreen(&mut self) -> Pixels {
        let Some(mut s) = self.stack.pop() else { return Pixels::new(0, 0, 0, 0) };
        if s.w == 0 || s.h == 0 {
            return Pixels::new(s.origin.0, s.origin.1, 0, 0);
        }
        let px = Self::render_surface(&mut s, self.res);
        pixels_of(&px, s.origin)
    }

    fn snapshot(&mut self, r: IRect) -> Pixels {
        let n = self.stack.len() - 1;
        let s = &mut self.stack[n];
        let px = Self::render_surface(s, self.res);
        let all = pixels_of(&px, s.origin);
        // Continue on top of what was drawn so far.
        s.ctx.reset();
        let base = Arc::new(px);
        s.ctx.set_transform(Affine::IDENTITY);
        s.ctx.set_paint(Image { image: ImageSource::Pixmap(base), sampler: ImageSampler { quality: ImageQuality::Low, ..Default::default() } });
        s.ctx.set_paint_transform(Affine::IDENTITY);
        s.ctx.set_fill_rule(Fill::NonZero);
        s.ctx.fill_rect(&Rect::new(0.0, 0.0, s.w as f64, s.h as f64));
        crop(&all, r)
    }

    fn process(&mut self, mut s: Pixels, p: Process) -> Pixels {
        match p {
            Process::Blur { sigma } => {
                blur::blur(&mut s, sigma);
                s
            }
            Process::DropShadow { color, dx, dy, sigma, spread } => blur::drop_shadow(&s, color, dx, dy, sigma, spread),
            Process::InnerShadow { color, dx, dy, sigma, spread } => blur::inner_shadow(&s, color, dx, dy, sigma, spread),
        }
    }

    fn draw_surface(&mut self, p: &Pixels) {
        if p.w == 0 || p.h == 0 {
            return;
        }
        let px = pixmap_of(p);
        let ctx = self.at(Affine::translate((p.x as f64, p.y as f64)));
        ctx.set_paint(Image { image: ImageSource::Pixmap(px), sampler: ImageSampler { quality: ImageQuality::Low, ..Default::default() } });
        ctx.set_paint_transform(Affine::IDENTITY);
        ctx.set_fill_rule(Fill::NonZero);
        ctx.fill_rect(&Rect::new(0.0, 0.0, p.w as f64, p.h as f64));
    }
}

impl Renderer for CpuRenderer {
    type Target = Pixmap;

    fn render(&mut self, frame: &Frame, target: &mut Pixmap) -> Result<RenderStats, RenderError> {
        if frame.width == 0 || frame.height == 0 || frame.width > u16::MAX as u32 || frame.height > u16::MAX as u32 {
            return Err(RenderError::Size(frame.width, frame.height));
        }
        let top = Surface { ctx: context(frame.width, frame.height, self.threads), origin: (0, 0), w: frame.width, h: frame.height };
        let mut cpu = Cpu { stack: vec![top], res: &mut self.resources, images: &mut self.images };
        let stats = render::execute(&mut cpu, frame);
        if let Some(mut s) = cpu.stack.pop() {
            s.ctx.flush();
            s.ctx.render(&mut *target, cpu.res);
        }
        Ok(stats)
    }
}

/// Straight-alpha RGBA8 of a rendered pixmap.
pub fn rgba(px: &Pixmap) -> Vec<u8> {
    px.clone().take_rgba8(ImageAlphaType::Alpha)
}

/// Encodes a pixmap as PNG.
pub fn png(px: &Pixmap) -> Vec<u8> {
    let mut out = Vec::new();
    let enc = image::codecs::png::PngEncoder::new(&mut out);
    let _ = image::ImageEncoder::write_image(enc, &rgba(px), px.width() as u32, px.height() as u32, image::ExtendedColorType::Rgba8);
    out
}

/// Renders the world rectangle `rect` of `page` at `scale` pixels per unit.
pub fn render_rect(scene: &mut Scene, doc: &Document, rect: Rect, scale: f64, renderer: &mut CpuRenderer) -> Result<Pixmap, RenderError> {
    let (w, h) = ((rect.width() * scale).ceil().max(1.0) as u32, (rect.height() * scale).ceil().max(1.0) as u32);
    if w > u16::MAX as u32 || h > u16::MAX as u32 {
        return Err(RenderError::Size(w, h));
    }
    let camera = Affine::scale(scale) * Affine::translate((-rect.x0, -rect.y0));
    let frame = scene.frame_complete(doc, camera, w, h);
    let mut px = Pixmap::new(w as u16, h as u16);
    renderer.render(&frame, &mut px)?;
    Ok(px)
}

/// Renders a whole page (or one node's bounds) to PNG bytes.
pub fn render_png(doc: &Document, page: NodeId, node: Option<NodeId>, scale: f64, fonts: text::FontDb, assets: Arc<dyn render::AssetSource>) -> Result<Vec<u8>, RenderError> {
    let mut scene = Scene::new(doc, page, fonts, assets);
    let rect = match node {
        Some(n) => scene.node_bounds(doc, n),
        None => scene.page_bounds(doc),
    }
    .unwrap_or(Rect::new(0.0, 0.0, 1.0, 1.0));
    let rect = Rect::new(rect.x0.floor(), rect.y0.floor(), rect.x1.ceil(), rect.y1.ceil());
    let px = render_rect(&mut scene, doc, rect, scale, &mut CpuRenderer::default())?;
    Ok(png(&px))
}
