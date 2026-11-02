//! Replays a [`Frame`] on a [`Backend`]. Effect handling lives here, once: effect regions are
//! rendered offscreen, processed (blur, shadow) and drawn back; a backdrop blur snapshots what is
//! drawn so far. Backends only provide drawing primitives, offscreen surfaces and the
//! processing steps.

use std::sync::Arc;

use kurbo::{Affine, BezPath, Rect};
use peniko::{BlendMode, Color, Fill};

use crate::list::{Brush, Cmd, DisplayList, Effect};
use crate::{Frame, RenderStats};

/// An integer device-pixel rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IRect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl IRect {
    /// The pixels touched by `r`, clipped to `within`; `None` if empty.
    pub fn cover(r: Rect, within: IRect) -> Option<IRect> {
        let x0 = (r.x0.floor() as i64).max(within.x as i64);
        let y0 = (r.y0.floor() as i64).max(within.y as i64);
        let x1 = (r.x1.ceil() as i64).min(within.x as i64 + within.w as i64);
        let y1 = (r.y1.ceil() as i64).min(within.y as i64 + within.h as i64);
        (x1 > x0 && y1 > y0).then(|| IRect { x: x0 as i32, y: y0 as i32, w: (x1 - x0) as u32, h: (y1 - y0) as u32 })
    }

    pub fn rect(&self) -> Rect {
        Rect::new(self.x as f64, self.y as f64, self.x as f64 + self.w as f64, self.y as f64 + self.h as f64)
    }

    fn inflate(&self, m: i32) -> IRect {
        IRect { x: self.x - m, y: self.y - m, w: (self.w as i64 + 2 * m as i64).max(0) as u32, h: (self.h as i64 + 2 * m as i64).max(0) as u32 }
    }
}

/// A run of effect-free commands, identified so a backend can cache its encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RunKey {
    pub fragment: u64,
    pub version: u64,
    pub start: u32,
}

/// What an offscreen result is turned into before it is drawn back.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Process {
    Blur { sigma: f64 },
    DropShadow { color: [f32; 4], dx: i32, dy: i32, sigma: f64, spread: i32 },
    InnerShadow { color: [f32; 4], dx: i32, dy: i32, sigma: f64, spread: i32 },
}

/// Drawing primitives. All transforms are to device pixels of the current surface's origin
/// handled by the backend: draws are in frame device space; offscreen surfaces cover a device
/// rectangle and the backend offsets draws into them.
pub trait Backend {
    /// An offscreen result placed at a device rectangle.
    type Surface;

    fn fill(&mut self, path: &BezPath, rule: Fill, xf: Affine, brush: &Brush, brush_xf: Affine);
    fn stroke(&mut self, path: &BezPath, style: &kurbo::Stroke, xf: Affine, brush: &Brush, brush_xf: Affine);
    fn glyphs(&mut self, run: &text::GlyphRun, xf: Affine, brush: &Brush, brush_xf: Affine);
    /// `rect`, `radius` and `sigma` are in the local units `xf` maps from.
    fn blurred_rect(&mut self, rect: Rect, radius: f64, sigma: f64, color: Color, xf: Affine);
    fn push_layer(&mut self, clip: &BezPath, rule: Fill, xf: Affine, blend: BlendMode, alpha: f32);
    fn pop_layer(&mut self);

    /// Starts drawing into a new transparent surface covering `r`.
    fn begin_offscreen(&mut self, r: IRect);
    /// Finishes the innermost offscreen surface.
    fn end_offscreen(&mut self) -> Self::Surface;
    /// Renders what the current surface holds so far and returns the part under `r`. No layers
    /// are open when this is called; the surface keeps its content and drawing continues on top.
    fn snapshot(&mut self, r: IRect) -> Self::Surface;
    fn process(&mut self, s: Self::Surface, p: Process) -> Self::Surface;
    /// Draws a surface at its device rectangle (1:1 pixels).
    fn draw_surface(&mut self, s: &Self::Surface);

    /// Draws an effect-free run of commands. Backends may cache the encoding by `key`; the
    /// default replays the commands.
    fn run(&mut self, _key: RunKey, cmds: &[Cmd], xf: Affine) {
        for c in cmds {
            draw_simple(self, c, xf);
        }
    }
}

/// Draws one effect-free command under `xf` (fragment → device).
pub fn draw_simple<B: Backend + ?Sized>(b: &mut B, c: &Cmd, xf: Affine) {
    match c {
        Cmd::PushLayer { clip, rule, xf: l, blend, alpha } => b.push_layer(clip, *rule, xf * *l, *blend, *alpha),
        Cmd::PopLayer => b.pop_layer(),
        Cmd::Fill { path, rule, xf: l, brush, brush_xf } => b.fill(path, *rule, xf * *l, brush, *brush_xf),
        Cmd::Stroke { path, style, xf: l, brush, brush_xf } => b.stroke(path, style, xf * *l, brush, *brush_xf),
        Cmd::Glyphs { run, xf: l, brush, brush_xf } => b.glyphs(run, xf * *l, brush, *brush_xf),
        Cmd::BlurredRect { rect, radius, sigma, color, xf: l } => b.blurred_rect(*rect, *radius, *sigma, *color, xf * *l),
        Cmd::BackdropBlur { .. } | Cmd::PushEffect { .. } | Cmd::PopEffect => {}
    }
}

/// A direction vector under the linear part of `t`.
pub fn transform_vector(t: Affine, v: kurbo::Vec2) -> kurbo::Vec2 {
    let c = t.as_coeffs();
    kurbo::Vec2::new(c[0] * v.x + c[2] * v.y, c[1] * v.x + c[3] * v.y)
}

/// Uniform scale of a transform (√|det|).
pub fn scale(t: Affine) -> f64 {
    t.determinant().abs().sqrt()
}

fn straight(c: Color) -> [f32; 4] {
    c.components
}

#[derive(Clone)]
struct Layer {
    clip: Arc<BezPath>,
    rule: Fill,
    xf: Affine,
    blend: BlendMode,
    alpha: f32,
}

struct Level {
    /// Device area of the surface.
    area: IRect,
    /// Layers open on this surface, outermost first.
    layers: Vec<Layer>,
    /// The effect this offscreen level applies when it ends (`None` for the frame itself).
    process: Option<Process>,
}

struct Exec<'b, B: Backend> {
    b: &'b mut B,
    levels: Vec<Level>,
    stats: RenderStats,
}

impl<B: Backend> Exec<'_, B> {
    fn level(&mut self) -> &mut Level {
        let n = self.levels.len() - 1;
        &mut self.levels[n]
    }

    fn push_layer(&mut self, l: Layer) {
        self.b.push_layer(&l.clip, l.rule, l.xf, l.blend, l.alpha);
        self.level().layers.push(l);
    }

    fn pop_layer(&mut self) {
        if self.level().layers.pop().is_some() {
            self.b.pop_layer();
        }
    }

    fn cmd(&mut self, c: &Cmd, xf: Affine) {
        match c {
            Cmd::PushLayer { clip, rule, xf: l, blend, alpha } => {
                self.push_layer(Layer { clip: clip.clone(), rule: *rule, xf: xf * *l, blend: *blend, alpha: *alpha })
            }
            Cmd::PopLayer => self.pop_layer(),
            Cmd::BackdropBlur { clip, xf: l, sigma } => self.backdrop(clip, xf * *l, *sigma),
            Cmd::PushEffect { effect, bounds, xf: l } => self.push_effect(effect, *bounds, xf, xf * *l),
            Cmd::PopEffect => self.pop_effect(),
            other => draw_simple(self.b, other, xf),
        }
    }

    fn backdrop(&mut self, clip: &Arc<BezPath>, xf: Affine, sigma: f64) {
        let s = sigma * scale(xf);
        let area = self.level().area;
        let shape = xf.transform_rect_bbox(kurbo::Shape::bounding_box(&**clip));
        let margin = (3.0 * s).ceil() as i32;
        let Some(r) = IRect::cover(shape, area).map(|r| r.inflate(margin)).and_then(|r| IRect::cover(r.rect(), area)) else { return };
        // Close this surface's layers, snapshot, reopen.
        let open = std::mem::take(&mut self.level().layers);
        for _ in &open {
            self.b.pop_layer();
        }
        let snap = self.b.snapshot(r);
        self.stats.backdrop_passes += 1;
        for l in open {
            self.push_layer(l);
        }
        let blurred = self.b.process(snap, Process::Blur { sigma: s });
        self.b.push_layer(clip, Fill::NonZero, xf, BlendMode::default(), 1.0);
        self.b.draw_surface(&blurred);
        self.b.pop_layer();
    }

    fn push_effect(&mut self, e: &Effect, bounds: Rect, frag_xf: Affine, xf: Affine) {
        let s = scale(xf);
        let parent = self.level().area;
        let dev = frag_xf.transform_rect_bbox(bounds);
        let px = |v: (f64, f64)| {
            let d = transform_vector(xf, kurbo::Vec2::new(v.0, v.1));
            (d.x.round() as i32, d.y.round() as i32)
        };
        let (process, reach) = match *e {
            Effect::LayerBlur { sigma } => (Process::Blur { sigma: sigma * s }, 3.0 * sigma * s),
            Effect::DropShadow { color, offset, sigma, spread } => {
                let (dx, dy) = px(offset);
                (Process::DropShadow { color: straight(color), dx, dy, sigma: sigma * s, spread: (spread * s).round() as i32 }, 3.0 * sigma * s + (dx.abs().max(dy.abs())) as f64 + (spread * s).max(0.0))
            }
            Effect::InnerShadow { color, offset, sigma, spread } => {
                let (dx, dy) = px(offset);
                (Process::InnerShadow { color: straight(color), dx, dy, sigma: sigma * s, spread: (spread * s).round() as i32 }, 0.0)
            }
        };
        // Content outside the parent surface still blurs or casts into it: keep a margin.
        let within = parent.inflate(reach.ceil() as i32);
        let area = IRect::cover(dev, within).unwrap_or(IRect { x: parent.x, y: parent.y, w: 0, h: 0 });
        self.b.begin_offscreen(area);
        self.stats.offscreen_passes += 1;
        self.levels.push(Level { area, layers: Vec::new(), process: Some(process) });
    }

    fn pop_effect(&mut self) {
        if self.levels.len() < 2 {
            return;
        }
        let open = std::mem::take(&mut self.level().layers);
        for _ in &open {
            self.b.pop_layer();
        }
        let Some(level) = self.levels.pop() else { return };
        let surface = self.b.end_offscreen();
        if let Some(p) = level.process {
            let out = self.b.process(surface, p);
            self.b.draw_surface(&out);
        }
    }

    fn list(&mut self, frag: u64, version: u64, list: &DisplayList, xf: Affine) {
        let cmds = &list.cmds;
        let mut start = 0usize;
        let mut i = 0usize;
        // Effect-free runs go to the backend whole (cacheable); track layers they open.
        while i <= cmds.len() {
            let at_end = i == cmds.len();
            if at_end || cmds[i].needs_offscreen() {
                if i > start {
                    let run = &cmds[start..i];
                    self.b.run(RunKey { fragment: frag, version, start: start as u32 }, run, xf);
                    for c in run {
                        match c {
                            Cmd::PushLayer { clip, rule, xf: l, blend, alpha } => {
                                self.level().layers.push(Layer { clip: clip.clone(), rule: *rule, xf: xf * *l, blend: *blend, alpha: *alpha })
                            }
                            Cmd::PopLayer => {
                                self.level().layers.pop();
                            }
                            _ => {}
                        }
                    }
                }
                if at_end {
                    break;
                }
                self.cmd(&cmds[i], xf);
                start = i + 1;
            }
            i += 1;
        }
    }
}

/// Renders `frame` with `b`: background, then each fragment under its ancestor clips.
pub fn execute<B: Backend>(b: &mut B, frame: &Frame) -> RenderStats {
    let area = IRect { x: 0, y: 0, w: frame.width, h: frame.height };
    let mut ex = Exec { b, levels: vec![Level { area, layers: Vec::new(), process: None }], stats: RenderStats::default() };
    if frame.background.components[3] > 0.0 {
        let bg = area.rect();
        ex.b.fill(&kurbo::Shape::to_path(&bg, 0.1), Fill::NonZero, Affine::IDENTITY, &Brush::Solid(frame.background), Affine::IDENTITY);
    }
    for item in &frame.items {
        let xf = frame.camera * item.xf;
        let depth = ex.level().layers.len();
        for (clip, cxf) in &item.clips {
            ex.push_layer(Layer { clip: clip.clone(), rule: Fill::NonZero, xf: frame.camera * *cxf, blend: BlendMode::default(), alpha: 1.0 });
        }
        ex.list(item.fragment, item.version, &item.list, xf);
        // Close anything left open by this fragment and its clips.
        while ex.levels.len() > 1 {
            ex.pop_effect();
        }
        while ex.level().layers.len() > depth {
            ex.pop_layer();
        }
        ex.stats.fragments += 1;
        ex.stats.commands += item.list.cmds.len();
    }
    ex.stats
}
