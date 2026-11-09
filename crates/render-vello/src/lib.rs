//! The GPU renderer, on Vello: the canvas's interactive path. It draws the same [`Frame`]s as
//! `render-cpu` through the shared executor, so the two differ only in rasterisation.
//!
//! - Effect-free runs are encoded once into a `vello::Scene` per [`RunKey`] (fragment, version,
//!   position) and appended with the camera transform on later frames.
//! - Offscreen effect regions render into pooled textures; blurs and shadows run as compute
//!   passes that follow `render::blur` step by step (`effects.rs`); results are drawn back as
//!   images whose pixels Vello takes from the textures (`Renderer::override_image`).
//! - Targets are `Rgba8Unorm`, so blending and gradients stay in sRGB-encoded values, as on the
//!   CPU.

mod effects;

use std::sync::Arc;

use effects::{Batch, Effects, Pool, Tex};
use kurbo::{Affine, BezPath, Rect};
use peniko::{Blob, BlendMode, Color, Fill, ImageAlphaType, ImageBrush, ImageData, ImageFormat, ImageQuality, ImageSampler};
use render::{Backend, Brush, Frame, IRect, Process, RenderError, RenderStats, Renderer, RunKey};
use render::exec::draw_simple;
use rustc_hash::FxHashMap;
use vello::{AaConfig, AaSupport, RenderParams, RendererOptions, Scene, wgpu};

pub use vello::wgpu as gpu;

/// A render target: an `Rgba8Unorm` texture holding straight-alpha pixels.
pub struct GpuTarget {
    pub texture: wgpu::Texture,
    view: wgpu::TextureView,
    pub width: u32,
    pub height: u32,
}

/// What the GPU side did in the last frame.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct GpuStats {
    /// Runs encoded this frame / appended from the cache.
    pub runs_encoded: usize,
    pub runs_reused: usize,
    /// Vello renders: offscreen surfaces and backdrop snapshots, plus the frame itself.
    pub renders: usize,
    pub compute_passes: usize,
    /// CPU time spent submitting offscreen Vello renders and effect passes (the GPU runs them
    /// asynchronously).
    pub offscreen_ms: f64,
    pub effects_ms: f64,
    /// CPU time walking the frame's display lists into Vello scenes (including offscreen work).
    pub execute_ms: f64,
    /// CPU time submitting the frame's own Vello render.
    pub submit_ms: f64,
    /// Paths and glyphs in the frame's own Vello scene.
    pub paths: usize,
    pub glyphs: usize,
}

/// Renders frames on the GPU.
pub struct VelloRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    adapter: Option<wgpu::AdapterInfo>,
    vello: vello::Renderer,
    effects: Effects,
    pool: Pool,
    /// Decoded images by their stable key; the same `ImageData` (blob id) keeps Vello's atlas
    /// entry. With each, the last Vello render that used it.
    images: FxHashMap<u64, (ImageData, u64)>,
    /// Encoded runs, the images they use, and the frame they were last drawn in.
    runs: FxHashMap<RunKey, Run>,
    frame_no: u64,
    /// Vello renders so far (several per frame with effects).
    renders: u64,
    /// The frame's own scene, kept between frames so its buffers keep their capacity (a
    /// whole-page view is several MB of encoding).
    top: Option<Scene>,
    pub stats: GpuStats,
}

fn gpu_err(e: impl std::fmt::Display) -> RenderError {
    RenderError::Gpu(e.to_string())
}

impl VelloRenderer {
    /// Uses the caller's device (the canvas app's window device).
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Result<Self, RenderError> {
        let vello = vello::Renderer::new(&device, RendererOptions { use_cpu: false, antialiasing_support: AaSupport::area_only(), num_init_threads: None, pipeline_cache: None })
            .map_err(gpu_err)?;
        let effects = Effects::new(&device);
        Ok(VelloRenderer {
            device,
            queue,
            adapter: None,
            vello,
            effects,
            pool: Pool::default(),
            images: FxHashMap::default(),
            runs: FxHashMap::default(),
            frame_no: 0,
            renders: 0,
            top: None,
            stats: GpuStats::default(),
        })
    }

    /// Creates its own device on the high-performance adapter (tests, benchmarks, exports).
    pub fn headless() -> Result<Self, RenderError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            display: None,
            backends: wgpu::Backends::from_env().unwrap_or_default(),
            flags: wgpu::InstanceFlags::from_build_config().with_env(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            backend_options: wgpu::BackendOptions::from_env_or_default(),
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
            apply_limit_buckets: false,
        }))
        .map_err(gpu_err)?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("render-vello"),
            required_features: adapter.features() & (wgpu::Features::CLEAR_TEXTURE | wgpu::Features::PIPELINE_CACHE),
            // Vello grows its working buffers on complex frames; allow what the adapter does.
            required_limits: wgpu::Limits {
                max_storage_buffer_binding_size: adapter.limits().max_storage_buffer_binding_size,
                max_buffer_size: adapter.limits().max_buffer_size,
                ..wgpu::Limits::default()
            },
            ..Default::default()
        }))
        .map_err(gpu_err)?;
        let mut r = Self::new(device, queue)?;
        r.adapter = Some(adapter.get_info());
        Ok(r)
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// The adapter, when this renderer created its own device.
    pub fn adapter_info(&self) -> Option<&wgpu::AdapterInfo> {
        self.adapter.as_ref()
    }

    /// A `width × height` target for [`Renderer::render`].
    pub fn target(&self, width: u32, height: u32) -> GpuTarget {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("target"),
            size: wgpu::Extent3d { width: width.max(1), height: height.max(1), depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        GpuTarget { texture, view, width, height }
    }

    /// Blocks until the GPU has finished the work submitted so far.
    pub fn wait(&self) -> Result<(), RenderError> {
        self.device.poll(wgpu::PollType::wait_indefinitely()).map(|_| ()).map_err(gpu_err)
    }

    /// Checks, without blocking, whether a frame already submitted ran out of Vello's working
    /// memory. Vello has grown its buffers since, and that frame drew incomplete, so render again
    /// when this returns true.
    pub fn poll_memory(&mut self) -> bool {
        self.vello.poll_memory(&self.device)
    }

    /// Whether frames' memory checks are still on their way back from the GPU: call
    /// [`poll_memory`](Self::poll_memory) again soon.
    pub fn memory_pending(&self) -> bool {
        self.vello.memory_pending()
    }

    /// Straight-alpha RGBA8 pixels of a target (waits for the GPU).
    pub fn read(&self, t: &GpuTarget) -> Result<Vec<u8>, RenderError> {
        let (w, h) = (t.width, t.height);
        let bpr = (w * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: bpr as u64 * h as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("readback") });
        enc.copy_texture_to_buffer(
            t.texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bpr), rows_per_image: None } },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.queue.submit([enc.finish()]);
        buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        self.wait()?;
        let data = buf.slice(..).get_mapped_range().map_err(gpu_err)?;
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for row in data.chunks_exact(bpr as usize) {
            out.extend_from_slice(&row[..(w * 4) as usize]);
        }
        drop(data);
        buf.unmap();
        Ok(out)
    }

    /// Drops cached encodings and images (memory pressure, document switch).
    pub fn clear_caches(&mut self) {
        self.runs.clear();
        self.images.clear();
    }

    /// Has Vello copy this frame's effect results into its atlas again before the next render.
    /// Like images (see `render_scene`), their atlas pixels don't reliably survive between
    /// renders; re-copying is a GPU texture copy per result.
    fn refresh_overrides(&mut self, images: &[ImageData]) {
        for image in images {
            self.vello.mark_override_image_dirty(image);
        }
    }

    fn evict_runs(&mut self) {
        let now = self.frame_no;
        let mut latest: FxHashMap<u64, u64> = FxHashMap::default();
        for k in self.runs.keys() {
            let v = latest.entry(k.fragment).or_default();
            *v = (*v).max(k.version);
        }
        self.runs.retain(|k, run| run.last_frame + RUN_TTL >= now || latest.get(&k.fragment) == Some(&k.version));
        let total: usize = self.runs.values().map(|r| r.bytes).sum();
        if total > RUN_BUDGET {
            let mut by_age: Vec<(u64, usize, RunKey)> = self.runs.iter().map(|(k, r)| (r.last_frame, r.bytes, *k)).collect();
            by_age.sort_unstable_by_key(|x| x.0);
            let mut over = total - RUN_BUDGET * 3 / 4;
            for (last, bytes, k) in by_age {
                if over == 0 || last == now {
                    break;
                }
                self.runs.remove(&k);
                over = over.saturating_sub(bytes);
            }
        }
    }

    /// Renders `scene`, which draws the images `used`.
    fn render_scene(&mut self, scene: &Scene, used: &[u64], view: &wgpu::TextureView, w: u32, h: u32) -> Result<(), RenderError> {
        self.stats.renders += 1;
        self.renders += 1;
        // Vello 0.11 keeps an image resident in its atlas, but its pixels don't survive a render
        // that doesn't use it: re-upload images that skipped the previous render.
        for k in used {
            if let Some((image, last)) = self.images.get_mut(k) {
                if *last + 1 != self.renders {
                    self.vello.mark_override_image_dirty(image);
                }
                *last = self.renders;
            }
        }
        let params = RenderParams { base_color: Color::TRANSPARENT, width: w, height: h, antialiasing_method: AaConfig::Area };
        self.vello.render_to_texture(&self.device, &self.queue, scene, view, &params).map_err(gpu_err)
    }
}

// ---- encoding primitives into a vello::Scene ---------------------------------------------------

/// A solid colour ignores its transform; leaving it out saves Vello a transform per path.
fn brush_transform(b: &peniko::Brush, xf: Affine) -> Option<Affine> {
    (!matches!(b, peniko::Brush::Solid(_))).then_some(xf)
}

fn brush(images: &mut FxHashMap<u64, (ImageData, u64)>, used: &mut Vec<u64>, b: &Brush) -> peniko::Brush {
    match b {
        Brush::Solid(c) => peniko::Brush::Solid(*c),
        Brush::Gradient(g) => peniko::Brush::Gradient(g.clone()),
        Brush::Image(ip) => {
            let image = images
                .entry(ip.image.key)
                .or_insert_with(|| {
                    let data = ImageData { data: ip.image.rgba.clone(), format: ImageFormat::Rgba8, alpha_type: ImageAlphaType::Alpha, width: ip.image.width, height: ip.image.height };
                    (data, 0)
                })
                .0
                .clone();
            if !used.contains(&ip.image.key) {
                used.push(ip.image.key);
            }
            peniko::Brush::Image(ImageBrush { image, sampler: ImageSampler { x_extend: ip.extend, y_extend: ip.extend, quality: ip.quality, alpha: ip.alpha } })
        }
    }
}

/// Draws primitives into a scene, noting the images it uses.
struct Painter<'a> {
    scene: &'a mut Scene,
    images: &'a mut FxHashMap<u64, (ImageData, u64)>,
    used: &'a mut Vec<u64>,
}

impl Painter<'_> {
    fn fill(&mut self, path: &BezPath, rule: Fill, xf: Affine, b: &Brush, brush_xf: Affine) {
        let b = brush(self.images, self.used, b);
        self.scene.fill(rule, xf, &b, brush_transform(&b, brush_xf), path);
    }

    fn stroke(&mut self, path: &BezPath, style: &kurbo::Stroke, xf: Affine, b: &Brush, brush_xf: Affine) {
        let b = brush(self.images, self.used, b);
        self.scene.stroke(style, xf, &b, brush_transform(&b, brush_xf), path);
    }

    fn glyphs(&mut self, run: &text::GlyphRun, xf: Affine, b: &Brush, brush_xf: Affine) {
        let b = brush(self.images, self.used, b);
        let skew = run.skew.map(|deg| Affine::skew((-deg as f64).to_radians().tan(), 0.0));
        self.scene
            .draw_glyphs(&run.font)
            .font_size(run.size)
            .hint(false)
            .normalized_coords(&run.coords)
            .transform(xf)
            .glyph_transform(skew)
            .brush(&b)
            .brush_transform(brush_transform(&b, brush_xf))
            .draw(Fill::NonZero, run.glyphs.iter().map(|&(id, x, y)| vello::Glyph { id, x, y }));
    }

    fn blurred_rect(&mut self, rect: Rect, radius: f64, sigma: f64, color: Color, xf: Affine) {
        self.scene.draw_blurred_rounded_rect(xf, rect, color, radius, sigma);
    }

    fn push_layer(&mut self, clip: &BezPath, rule: Fill, xf: Affine, blend: BlendMode, alpha: f32) {
        self.scene.push_layer(rule, blend, alpha, xf, clip);
    }
}

/// Records an effect-free run at its own (fragment) coordinates for the run cache.
struct Recorder<'a> {
    p: Painter<'a>,
}

impl Backend for Recorder<'_> {
    type Surface = ();

    fn fill(&mut self, path: &BezPath, rule: Fill, xf: Affine, brush: &Brush, brush_xf: Affine) {
        self.p.fill(path, rule, xf, brush, brush_xf);
    }

    fn stroke(&mut self, path: &BezPath, style: &kurbo::Stroke, xf: Affine, brush: &Brush, brush_xf: Affine) {
        self.p.stroke(path, style, xf, brush, brush_xf);
    }

    fn glyphs(&mut self, run: &text::GlyphRun, xf: Affine, brush: &Brush, brush_xf: Affine) {
        self.p.glyphs(run, xf, brush, brush_xf);
    }

    fn blurred_rect(&mut self, rect: Rect, radius: f64, sigma: f64, color: Color, xf: Affine) {
        self.p.blurred_rect(rect, radius, sigma, color, xf);
    }

    fn push_layer(&mut self, clip: &BezPath, rule: Fill, xf: Affine, blend: BlendMode, alpha: f32) {
        self.p.push_layer(clip, rule, xf, blend, alpha);
    }

    fn pop_layer(&mut self) {
        self.p.scene.pop_layer();
    }

    // Runs contain no effects.
    fn begin_offscreen(&mut self, _r: IRect) {}
    fn end_offscreen(&mut self) {}
    fn snapshot(&mut self, _r: IRect) {}
    fn process(&mut self, _s: (), _p: Process) {}
    fn draw_surface(&mut self, _s: &()) {}
}

// ---- the frame backend ----------------------------------------------------------------------
//
// Offscreen effect regions are deferred: `end_offscreen` keeps the region's scene, `process`
// notes the effect, `draw_surface` draws a placeholder image. Before a scene that draws them is
// rendered, all pending regions are packed into an atlas and rendered with one Vello call, their
// effects run as one batch of compute passes, and each placeholder's pixels are pointed at its
// result (`override_image`, which can take a sub-rectangle of a texture).

/// Largest atlas side for batched offscreen regions (wgpu's default texture limit).
const ATLAS_MAX: u32 = 8192;

enum Src {
    Empty,
    /// Rendered (a backdrop snapshot): the texture and the region's origin in it.
    Ready(Tex, (u32, u32)),
    Pending(usize),
}

/// An offscreen result at a device rectangle.
pub struct Surface {
    src: Src,
    rect: IRect,
    premul: bool,
}

struct Level {
    scene: Scene,
    area: IRect,
    /// Image keys the scene draws.
    used: Vec<u64>,
    /// Pending regions the scene draws.
    deps: Vec<usize>,
}

impl Level {
    fn new(area: IRect) -> Self {
        Level { scene: Scene::new(), area, used: Vec::new(), deps: Vec::new() }
    }
}

struct Pending {
    /// The region's content, until it is rendered.
    level: Option<Level>,
    deps: Vec<usize>,
    process: Option<Process>,
    /// The placeholder drawn for it.
    image: Option<ImageData>,
    done: bool,
}

struct Run {
    scene: Scene,
    images: Vec<u64>,
    last_frame: u64,
    /// Encoded size, for the cache budget.
    bytes: usize,
}

fn encoded_bytes(scene: &Scene) -> usize {
    use std::mem::size_of_val;
    let e = scene.encoding();
    size_of_val(e.path_tags.as_slice())
        + size_of_val(e.path_data.as_slice())
        + size_of_val(e.draw_tags.as_slice())
        + size_of_val(e.draw_data.as_slice())
        + size_of_val(e.transforms.as_slice())
        + size_of_val(e.styles.as_slice())
        + size_of_val(e.resources.glyphs.as_slice())
}

struct Gpu<'r> {
    r: &'r mut VelloRenderer,
    levels: Vec<Level>,
    pending: Vec<Pending>,
    /// Images whose pixels come from textures this frame; released after it.
    overrides: Vec<ImageData>,
    error: Option<RenderError>,
}

fn ms_since(t: std::time::Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

/// An atlas's size and its items as (index, x, y).
type Atlas = ((u32, u32), Vec<(usize, u32, u32)>);

/// Shelf-packs `sizes` into atlases no larger than `max`, at multiples of `align`; returns
/// (atlas size, [(index, x, y)]).
fn pack(sizes: &[(usize, u32, u32)], max: u32, align: u32) -> Vec<Atlas> {
    let mut order: Vec<&(usize, u32, u32)> = sizes.iter().collect();
    order.sort_by_key(|s| std::cmp::Reverse(s.2));
    let mut atlases = Vec::new();
    let (mut items, mut x, mut y, mut shelf, mut width) = (Vec::new(), 0u32, 0u32, 0u32, 0u32);
    for &&(i, w, h) in &order {
        let (w, h) = (w.next_multiple_of(align), h.next_multiple_of(align));
        if x + w > max && x > 0 {
            (x, y) = (0, y + shelf);
            shelf = 0;
        }
        if y + h > max && !items.is_empty() {
            atlases.push(((width, y + shelf), std::mem::take(&mut items)));
            (x, y, shelf, width) = (0, 0, 0, 0);
        }
        items.push((i, x, y));
        x += w;
        shelf = shelf.max(h);
        width = width.max(x);
    }
    if !items.is_empty() {
        atlases.push(((width, y + shelf), items));
    }
    atlases
}

impl Gpu<'_> {
    /// The current scene, the transform from frame device space to it, and the image table.
    fn top(&mut self) -> (Painter<'_>, Affine) {
        let n = self.levels.len() - 1;
        let l = &mut self.levels[n];
        let o = Affine::translate((-l.area.x as f64, -l.area.y as f64));
        (Painter { scene: &mut l.scene, images: &mut self.r.images, used: &mut l.used }, o)
    }

    fn fail(&mut self, e: RenderError) {
        self.error.get_or_insert(e);
    }

    fn render(&mut self, scene: &Scene, used: &[u64], w: u32, h: u32) -> Tex {
        self.r.refresh_overrides(&self.overrides);
        let t = self.r.pool.take(&self.r.device, w, h, false);
        let t0 = std::time::Instant::now();
        if let Err(e) = self.r.render_scene(scene, used, &t.view, w, h) {
            self.fail(e);
        }
        self.r.stats.offscreen_ms += ms_since(t0);
        t
    }

    /// Points `image` at the `image.width × image.height` region of `t` at `origin`.
    fn attach(&mut self, image: &ImageData, t: &Tex, origin: (u32, u32)) {
        let copy = wgpu::TexelCopyTextureInfoBase { texture: t.texture.clone(), mip_level: 0, origin: wgpu::Origin3d { x: origin.0, y: origin.1, z: 0 }, aspect: wgpu::TextureAspect::All };
        self.r.vello.override_image(image, Some(copy));
    }

    /// Draws a placeholder for a `rect`-sized surface in the current level; returns it.
    fn placeholder(&mut self, rect: IRect, premul: bool) -> ImageData {
        let image = ImageData {
            data: Blob::new(Arc::new(Vec::<u8>::new())),
            format: ImageFormat::Rgba8,
            alpha_type: if premul { ImageAlphaType::AlphaPremultiplied } else { ImageAlphaType::Alpha },
            width: rect.w,
            height: rect.h,
        };
        let n = self.levels.len() - 1;
        let l = &mut self.levels[n];
        let brush = ImageBrush { image: image.clone(), sampler: ImageSampler { quality: ImageQuality::Low, ..Default::default() } };
        l.scene.draw_image(&brush, Affine::translate(((rect.x - l.area.x) as f64, (rect.y - l.area.y) as f64)));
        self.overrides.push(image.clone());
        image
    }

    /// Renders every pending region `roots` depend on, innermost first, a batch per depth.
    fn resolve(&mut self, roots: &[usize]) {
        loop {
            let mut todo = Vec::new();
            let mut stack: Vec<usize> = roots.to_vec();
            while let Some(i) = stack.pop() {
                if !self.pending[i].done && !todo.contains(&i) {
                    todo.push(i);
                    stack.extend(self.pending[i].deps.iter().copied());
                }
            }
            if todo.is_empty() {
                return;
            }
            let ready: Vec<usize> = todo.into_iter().filter(|i| self.pending[*i].deps.iter().all(|d| self.pending[*d].done)).collect();
            self.batch(&ready);
        }
    }

    fn batch(&mut self, ids: &[usize]) {
        let sizes: Vec<(usize, u32, u32)> = ids.iter().filter_map(|&i| self.pending[i].level.as_ref().map(|l| (i, l.area.w, l.area.h))).collect();
        // Slots aligned to the deepest octave, so the effects' octave images never overlap.
        let align = 1 << ids.iter().filter_map(|&i| self.pending[i].process.as_ref()).map(effects::levels).max().unwrap_or(0);
        for &i in ids {
            self.pending[i].done = true;
        }
        for ((aw, ah), items) in pack(&sizes, ATLAS_MAX, align) {
            let mut scene = Scene::new();
            let mut used: Vec<u64> = Vec::new();
            let mut placed = Vec::new();
            for (i, x, y) in items {
                let Some(l) = self.pending[i].level.take() else { continue };
                let slot = Rect::new(x as f64, y as f64, (x + l.area.w) as f64, (y + l.area.h) as f64);
                scene.push_clip_layer(Fill::NonZero, Affine::IDENTITY, &slot);
                scene.append(&l.scene, Some(Affine::translate((x as f64, y as f64))));
                scene.pop_layer();
                for k in l.used {
                    if !used.contains(&k) {
                        used.push(k);
                    }
                }
                placed.push((i, x, y, l.area));
            }
            // Pooled by rounded size, so atlases of similar frames reuse textures.
            let (tw, th) = (aw.div_ceil(256) * 256, ah.div_ceil(256) * 256);
            let atlas = self.render(&scene, &used, tw.min(ATLAS_MAX), th.min(ATLAS_MAX));
            let t0 = std::time::Instant::now();
            let mut b = Batch::new(atlas.clone());
            for &(i, x, y, area) in &placed {
                if let Some(p) = self.pending[i].process {
                    b.add((x, y), (area.w, area.h), false, p);
                }
            }
            let r = &mut *self.r;
            let out = r.effects.submit(&r.device, &r.queue, &mut r.pool, b).map(|(t, n)| {
                r.stats.compute_passes += n;
                t
            });
            r.stats.effects_ms += ms_since(t0);
            for (i, x, y, _) in placed {
                let t = match (&self.pending[i].process, &out) {
                    (Some(_), Some(out)) => out.clone(),
                    _ => atlas.clone(),
                };
                if let Some(image) = self.pending[i].image.clone() {
                    self.attach(&image, &t, (x, y));
                }
            }
        }
    }
}

impl Backend for Gpu<'_> {
    type Surface = Surface;

    fn fill(&mut self, path: &BezPath, rule: Fill, xf: Affine, brush: &Brush, brush_xf: Affine) {
        let (mut p, o) = self.top();
        p.fill(path, rule, o * xf, brush, brush_xf);
    }

    fn stroke(&mut self, path: &BezPath, style: &kurbo::Stroke, xf: Affine, brush: &Brush, brush_xf: Affine) {
        let (mut p, o) = self.top();
        p.stroke(path, style, o * xf, brush, brush_xf);
    }

    fn glyphs(&mut self, run: &text::GlyphRun, xf: Affine, brush: &Brush, brush_xf: Affine) {
        let (mut p, o) = self.top();
        p.glyphs(run, o * xf, brush, brush_xf);
    }

    fn blurred_rect(&mut self, rect: Rect, radius: f64, sigma: f64, color: Color, xf: Affine) {
        let (mut p, o) = self.top();
        p.blurred_rect(rect, radius, sigma, color, o * xf);
    }

    fn push_layer(&mut self, clip: &BezPath, rule: Fill, xf: Affine, blend: BlendMode, alpha: f32) {
        let (mut p, o) = self.top();
        p.push_layer(clip, rule, o * xf, blend, alpha);
    }

    fn pop_layer(&mut self) {
        let (p, _) = self.top();
        p.scene.pop_layer();
    }

    fn begin_offscreen(&mut self, r: IRect) {
        self.levels.push(Level::new(r));
    }

    fn end_offscreen(&mut self) -> Surface {
        let empty = IRect { x: 0, y: 0, w: 0, h: 0 };
        let Some(mut l) = self.levels.pop() else { return Surface { src: Src::Empty, rect: empty, premul: false } };
        if l.area.w == 0 || l.area.h == 0 {
            return Surface { src: Src::Empty, rect: l.area, premul: false };
        }
        let rect = l.area;
        let deps = std::mem::take(&mut l.deps);
        self.pending.push(Pending { level: Some(l), deps, process: None, image: None, done: false });
        Surface { src: Src::Pending(self.pending.len() - 1), rect, premul: false }
    }

    fn snapshot(&mut self, r: IRect) -> Surface {
        let n = self.levels.len() - 1;
        let deps = self.levels[n].deps.clone();
        self.resolve(&deps);
        let area = self.levels[n].area;
        let (x0, y0) = ((r.x - area.x).max(0), (r.y - area.y).max(0));
        let (x1, y1) = ((r.x - area.x + r.w as i32).min(area.w as i32), (r.y - area.y + r.h as i32).min(area.h as i32));
        if x1 <= x0 || y1 <= y0 {
            return Surface { src: Src::Empty, rect: r, premul: false };
        }
        let rect = IRect { x: area.x + x0, y: area.y + y0, w: (x1 - x0) as u32, h: (y1 - y0) as u32 };
        // Render just the region from a moved copy of what is drawn so far; the level carries on.
        let mut part = Scene::new();
        part.append(&self.levels[n].scene, Some(Affine::translate((-x0 as f64, -y0 as f64))));
        let used = self.levels[n].used.clone();
        let t = self.render(&part, &used, rect.w, rect.h);
        Surface { src: Src::Ready(t, (0, 0)), rect, premul: false }
    }

    fn process(&mut self, s: Surface, p: Process) -> Surface {
        match s.src {
            Src::Empty => s,
            Src::Pending(i) if self.pending[i].process.is_none() => {
                self.pending[i].process = Some(p);
                Surface { src: Src::Pending(i), rect: s.rect, premul: true }
            }
            Src::Pending(i) => {
                // A second effect on one region (the executor doesn't do this): a new region
                // that draws the first.
                self.levels.push(Level::new(s.rect));
                self.draw_surface(&Surface { src: Src::Pending(i), rect: s.rect, premul: s.premul });
                let outer = self.end_offscreen();
                self.process(outer, p)
            }
            Src::Ready(t, origin) => {
                // A snapshot: rendered already, so run now.
                let t0 = std::time::Instant::now();
                let r = &mut *self.r;
                let mut b = Batch::new(t);
                b.add(origin, (s.rect.w, s.rect.h), s.premul, p);
                let Some((out, n)) = r.effects.submit(&r.device, &r.queue, &mut r.pool, b) else { return Surface { src: Src::Empty, rect: s.rect, premul: true } };
                r.stats.compute_passes += n;
                r.stats.effects_ms += ms_since(t0);
                Surface { src: Src::Ready(out, origin), rect: s.rect, premul: true }
            }
        }
    }

    fn draw_surface(&mut self, s: &Surface) {
        match &s.src {
            Src::Empty => {}
            Src::Ready(t, origin) => {
                let image = self.placeholder(s.rect, s.premul);
                self.attach(&image, t, *origin);
            }
            Src::Pending(i) => {
                let i = *i;
                let image = self.placeholder(s.rect, s.premul);
                self.pending[i].image = Some(image);
                let n = self.levels.len() - 1;
                self.levels[n].deps.push(i);
            }
        }
    }

    fn run(&mut self, key: RunKey, cmds: &[render::Cmd], xf: Affine) {
        // A run can close layers an earlier run opened (an effect split the list between them).
        // A cached run's own scene has nothing open to close, and Vello drops such a pop, so those
        // go to the level's scene directly, between cached parts.
        let (mut depth, mut start) = (0usize, 0usize);
        for (i, c) in cmds.iter().enumerate() {
            match c {
                render::Cmd::PushLayer { .. } => depth += 1,
                render::Cmd::PopLayer if depth > 0 => depth -= 1,
                render::Cmd::PopLayer => {
                    if i > start {
                        self.cached(RunKey { start: key.start + start as u32, ..key }, &cmds[start..i], xf);
                    }
                    let n = self.levels.len() - 1;
                    self.levels[n].scene.pop_layer();
                    start = i + 1;
                }
                _ => {}
            }
        }
        if start < cmds.len() {
            self.cached(RunKey { start: key.start + start as u32, ..key }, &cmds[start..], xf);
        }
    }
}

impl Gpu<'_> {
    /// Draws `cmds` from the run cache, encoding them first if needed.
    fn cached(&mut self, key: RunKey, cmds: &[render::Cmd], xf: Affine) {
        let frame_no = self.r.frame_no;
        if !self.r.runs.contains_key(&key) {
            let (mut scene, mut images) = (Scene::new(), Vec::new());
            let mut rec = Recorder { p: Painter { scene: &mut scene, images: &mut self.r.images, used: &mut images } };
            for c in cmds {
                draw_simple(&mut rec, c, Affine::IDENTITY);
            }
            let bytes = encoded_bytes(&scene);
            self.r.runs.insert(key, Run { scene, images, last_frame: frame_no, bytes });
            self.r.stats.runs_encoded += 1;
        } else {
            self.r.stats.runs_reused += 1;
        }
        let n = self.levels.len() - 1;
        let l = &mut self.levels[n];
        let o = Affine::translate((-l.area.x as f64, -l.area.y as f64));
        if let Some(run) = self.r.runs.get_mut(&key) {
            run.last_frame = frame_no;
            l.scene.append(&run.scene, Some(o * xf));
            for k in &run.images {
                if !l.used.contains(k) {
                    l.used.push(*k);
                }
            }
        }
    }
}

/// Frames a superseded run (an older version of its fragment) survives without being drawn.
const RUN_TTL: u64 = 2;
/// Encoded runs kept for reuse, least recently drawn dropped first. Runs of every zoom band are
/// kept, so zooming back to a band doesn't re-encode the view.
const RUN_BUDGET: usize = 128 << 20;

impl Renderer for VelloRenderer {
    type Target = GpuTarget;

    fn render(&mut self, frame: &Frame, target: &mut GpuTarget) -> Result<RenderStats, RenderError> {
        if frame.width == 0 || frame.height == 0 || (frame.width, frame.height) != (target.width, target.height) {
            return Err(RenderError::Size(frame.width, frame.height));
        }
        self.frame_no += 1;
        self.stats = GpuStats::default();
        let area = IRect { x: 0, y: 0, w: frame.width, h: frame.height };
        let mut first = Level::new(area);
        if let Some(mut scene) = self.top.take() {
            scene.reset();
            first.scene = scene;
        }
        let mut gpu = Gpu { r: self, levels: vec![first], pending: Vec::new(), overrides: Vec::new(), error: None };
        let t0 = std::time::Instant::now();
        let stats = render::execute(&mut gpu, frame);
        let top = gpu.levels.pop().unwrap_or_else(|| Level::new(area));
        gpu.resolve(&top.deps);
        gpu.r.stats.execute_ms = ms_since(t0);
        let (overrides, error) = (std::mem::take(&mut gpu.overrides), gpu.error.take());
        self.refresh_overrides(&overrides);
        let e = top.scene.encoding();
        (self.stats.paths, self.stats.glyphs) = (e.n_paths as usize, e.resources.glyphs.len());
        let t0 = std::time::Instant::now();
        let result = self.render_scene(&top.scene, &top.used, &target.view, frame.width, frame.height);
        self.stats.submit_ms = ms_since(t0);
        for image in overrides {
            self.vello.override_image(&image, None);
        }
        self.top = Some(top.scene);
        self.pool.recycle();
        self.evict_runs();
        if let Some(e) = error {
            return Err(e);
        }
        result.map(|()| stats)
    }
}
