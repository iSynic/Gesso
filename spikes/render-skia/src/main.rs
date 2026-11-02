//! Renderer spike: Skia (skia-safe, OpenGL backend via glutin) running the shared scenario.
//!
//! Usage: render-skia [--no-cull] [--retained] [--frames N] [--out file.json] [--png dir]
//!        render-skia --cpu [--out file.json] [--png dir]   (Skia raster 4K export timing)

use common::*;
use glutin::config::{ConfigTemplateBuilder, GlConfig};
use glutin::context::{ContextAttributesBuilder, PossiblyCurrentContext};
use glutin::display::{GetGlDisplay, GlDisplay};
use glutin::prelude::{GlSurface, NotCurrentGlContext};
use glutin::surface::{Surface as GlutinSurface, SwapInterval, WindowSurface};
use glutin_winit::{DisplayBuilder, GlWindow};
use raw_window_handle::HasWindowHandle;
use skia_safe::gpu::gl::FramebufferInfo;
use skia_safe::gpu::{self, backend_render_targets, DirectContext, SurfaceOrigin};
use skia_safe::{
    image_filters, images, BlurStyle, Canvas, ClipOp, Color, ColorType, Data, Font, FontMgr, Image, ImageInfo, MaskFilter, Matrix, Paint, PaintStyle,
    Path, Picture, PictureRecorder, Point, RRect, Rect as SRect, SamplingOptions, Surface, TextBlob, Typeface,
};
use std::collections::HashMap;
use std::ffi::CString;
use std::num::NonZeroU32;
use std::time::Instant;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowId};

const W: u32 = 1920;
const H: u32 = 1080;
const EW: u32 = 3840;
const EH: u32 = 2160;

/// Retained Skia objects built once: unit-square icon paths, images (GPU textures when a
/// context is given), and one TextBlob per glyph run.
struct Cache {
    icons: Vec<Path>,
    images: Vec<Image>,
    blobs: Vec<Vec<TextBlob>>,
}

impl Cache {
    fn new(scene: &Scene, ctx: Option<&mut DirectContext>) -> Self {
        let icons = scene
            .icons
            .iter()
            .map(|els| {
                let mut p = skia_safe::PathBuilder::new();
                for e in els {
                    match *e {
                        PathEl::Move(x, y) => p.move_to((x, y)),
                        PathEl::Line(x, y) => p.line_to((x, y)),
                        PathEl::Cubic(a, b, c, d, e, f) => p.cubic_to((a, b), (c, d), (e, f)),
                        PathEl::Close => p.close(),
                    };
                }
                p.detach()
            })
            .collect();
        let mut images: Vec<Image> = scene
            .images
            .iter()
            .map(|img| {
                let info = ImageInfo::new((img.width as i32, img.height as i32), ColorType::RGBA8888, skia_safe::AlphaType::Unpremul, None);
                images::raster_from_data(&info, Data::new_copy(&img.rgba), img.width as usize * 4).unwrap()
            })
            .collect();
        if let Some(ctx) = ctx {
            images = images.into_iter().map(|i| i.new_texture_image(ctx, gpu::Mipmapped::Yes).unwrap()).collect();
        }
        let mgr = FontMgr::new();
        let mut faces: HashMap<(u64, u32), Typeface> = HashMap::new();
        let blobs = scene
            .texts
            .iter()
            .map(|tl| {
                tl.runs
                    .iter()
                    .filter_map(|run| {
                        let key = (run.font.data.id(), run.font.index);
                        let face = faces
                            .entry(key)
                            .or_insert_with(|| mgr.new_from_data(Data::new_copy(run.font.data.data()), run.font.index).expect("typeface"))
                            .clone();
                        let mut font = Font::from_typeface(face, run.size);
                        font.set_subpixel(true);
                        font.set_edging(skia_safe::font::Edging::AntiAlias);
                        let ids: Vec<u16> = run.glyphs.iter().map(|g| g.0 as u16).collect();
                        let pos: Vec<Point> = run.glyphs.iter().map(|g| Point::new(g.1, g.2)).collect();
                        TextBlob::from_pos_text(ids.as_slice(), &pos, &font)
                    })
                    .collect()
            })
            .collect();
        Cache { icons, images, blobs }
    }
}

fn paint(c: Rgba, opacity: f32) -> Paint {
    let mut p = Paint::default();
    p.set_anti_alias(true);
    p.set_color(Color::from_argb((c[3] as f32 * opacity) as u8, c[0], c[1], c[2]));
    p
}

/// Draw items in world coordinates; caller sets the camera matrix on the canvas.
fn draw(canvas: &Canvas, scene: &Scene, items: impl Iterator<Item = (u32, Lod)>, cache: &Cache, zoom: f32) {
    for (i, lod) in items {
        let it = &scene.items[i as usize];
        let r = SRect::from_xywh(it.rect.x as f32, it.rect.y as f32, it.rect.w as f32, it.rect.h as f32);
        if lod == Lod::Greek {
            canvas.draw_rect(SRect::from_ltrb(r.left, r.top + r.height() * 0.2, r.right, r.bottom - r.height() * 0.2), &paint(it.fill, 0.35));
            continue;
        }
        if let (Some(sh), Lod::Full) = (it.shadow, lod) {
            let mut p = paint(sh.color, 1.0);
            p.set_mask_filter(MaskFilter::blur(BlurStyle::Normal, sh.blur / 2.0, false));
            canvas.draw_rrect(RRect::new_rect_xy(r.with_offset((sh.dx, sh.dy)), it.radius, it.radius), &p);
        }
        let blur = it.blur > 0.0 && lod == Lod::Full;
        if blur {
            let mut lp = Paint::default();
            lp.set_image_filter(image_filters::blur((it.blur * zoom, it.blur * zoom), None, None, None));
            canvas.save_layer(&skia_safe::canvas::SaveLayerRec::default().paint(&lp));
        }
        let fill = paint(it.fill, it.opacity);
        let stroke = |c: Rgba, w: f32| {
            let mut p = paint(c, it.opacity);
            p.set_style(PaintStyle::Stroke);
            p.set_stroke_width(w);
            p
        };
        match it.kind {
            Kind::Rect => {
                let rr = RRect::new_rect_xy(r, it.radius, it.radius);
                canvas.draw_rrect(rr, &fill);
                if let (Some((sc, sw)), Lod::Full) = (it.stroke, lod) {
                    canvas.draw_rrect(rr, &stroke(sc, sw));
                }
            }
            Kind::Ellipse => {
                canvas.draw_oval(r, &fill);
                if let (Some((sc, sw)), Lod::Full) = (it.stroke, lod) {
                    canvas.draw_oval(r, &stroke(sc, sw));
                }
            }
            Kind::Path(p) => {
                canvas.save();
                canvas.translate((r.left, r.top));
                canvas.scale((r.width(), r.height()));
                canvas.draw_path(&cache.icons[p as usize], &fill);
                canvas.restore();
            }
            Kind::Image(m) => {
                let clip = it.radius > 0.0 && lod == Lod::Full;
                if clip {
                    canvas.save();
                    canvas.clip_rrect(RRect::new_rect_xy(r, it.radius, it.radius), ClipOp::Intersect, true);
                }
                canvas.draw_image_rect_with_sampling_options(&cache.images[m as usize], None, r, SamplingOptions::new(skia_safe::FilterMode::Linear, skia_safe::MipmapMode::Linear), &fill);
                if clip {
                    canvas.restore();
                }
            }
            Kind::Text(ti) => {
                for b in &cache.blobs[ti as usize] {
                    canvas.draw_text_blob(b, (r.left, r.top), &fill);
                }
            }
        }
        if blur {
            canvas.restore();
        }
    }
}

fn view_matrix(cam: &Camera) -> Matrix {
    let a = cam.affine();
    Matrix::new_all(a[0] as f32, 0.0, a[4] as f32, 0.0, a[3] as f32, a[5] as f32, 0.0, 0.0, 1.0)
}

struct Gl {
    window: Window,
    surface: Surface,
    gr: DirectContext,
    gl_surface: GlutinSurface<WindowSurface>,
    gl_context: PossiblyCurrentContext,
}

struct App {
    args: Args,
    scene: Scene,
    cache: Option<Cache>,
    culler: Culler,
    steps: Vec<Step>,
    frame: usize,
    samples: Vec<FrameSample>,
    retained: bool,
    pictures: Vec<[Option<Picture>; 3]>,
    gl: Option<Gl>,
    t_main: Instant,
    startup: serde_json::Value,
    peak_rss: f64,
    gpu_name: String,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.gl.is_some() {
            return;
        }
        let t = Timer::start();
        let attrs = Window::default_attributes().with_title("render-skia").with_inner_size(winit::dpi::PhysicalSize::new(W, H)).with_resizable(false);
        let template = ConfigTemplateBuilder::new().with_alpha_size(8).with_transparency(false);
        let (window, cfg) = DisplayBuilder::new()
            .with_window_attributes(Some(attrs))
            .build(el, template, |configs| configs.reduce(|a, c| if c.num_samples() < a.num_samples() { c } else { a }).unwrap())
            .unwrap();
        let window = window.unwrap();
        let raw = window.window_handle().unwrap().as_raw();
        let not_current = unsafe { cfg.display().create_context(&cfg, &ContextAttributesBuilder::new().build(Some(raw))).unwrap() };
        let sattrs = window.build_surface_attributes(Default::default()).unwrap();
        let gl_surface = unsafe { cfg.display().create_window_surface(&cfg, &sattrs).unwrap() };
        let gl_context = not_current.make_current(&gl_surface).unwrap();
        let _ = gl_surface.set_swap_interval(&gl_context, SwapInterval::DontWait);
        gl::load_with(|s| cfg.display().get_proc_address(CString::new(s).unwrap().as_c_str()));
        let t_ctx = t.ms();
        let interface = gpu::gl::Interface::new_load_with(|name| {
            if name == "eglGetCurrentDisplay" {
                return std::ptr::null();
            }
            cfg.display().get_proc_address(CString::new(name).unwrap().as_c_str())
        })
        .unwrap();
        let mut gr = gpu::direct_contexts::make_gl(interface, None).unwrap();
        let mut fboid: gl::types::GLint = 0;
        unsafe { gl::GetIntegerv(gl::FRAMEBUFFER_BINDING, &mut fboid) };
        let fb = FramebufferInfo { fboid: fboid as u32, format: gpu::gl::Format::RGBA8.into(), ..Default::default() };
        let brt = backend_render_targets::make_gl((W as i32, H as i32), cfg.num_samples() as usize, cfg.stencil_size() as usize, fb);
        let surface = gpu::surfaces::wrap_backend_render_target(&mut gr, &brt, SurfaceOrigin::BottomLeft, ColorType::RGBA8888, None, None).unwrap();
        self.gpu_name = unsafe { std::ffi::CStr::from_ptr(gl::GetString(gl::RENDERER) as *const _).to_string_lossy().into_owned() };
        let t_skia = t.ms();
        let tc = Timer::start();
        self.cache = Some(Cache::new(&self.scene, Some(&mut gr)));
        self.startup["renderer_cache_ms"] = tc.ms().into();
        self.startup["window_gl_ms"] = t_ctx.into();
        self.startup["skia_context_ms"] = (t_skia - t_ctx).into();
        let _ = NonZeroU32::new(1);
        self.gl = Some(Gl { window, surface, gr, gl_surface, gl_context });
        self.gl.as_ref().unwrap().window.request_redraw();
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::RedrawRequested => {
                if self.frame >= self.steps.len() {
                    self.finish();
                    el.exit();
                    return;
                }
                self.render_frame();
                self.gl.as_ref().unwrap().window.request_redraw();
            }
            _ => {}
        }
    }
}

impl App {
    fn render_frame(&mut self) {
        let total = Timer::start();
        let step = &self.steps[self.frame];
        let (cam, phase, edit) = (step.camera, step.phase, step.edit);
        if let Some(e) = edit {
            apply_edit(&mut self.scene, e, self.frame);
            if self.retained {
                self.pictures[self.scene.group_of(e)] = Default::default();
            }
        }
        let t = Timer::start();
        if !self.retained {
            self.culler.cull(&self.scene, cam.view(W, H), cam.zoom);
        }
        let cull_ms = t.ms();

        let t = Timer::start();
        let gl = self.gl.as_mut().unwrap();
        let cache = self.cache.as_ref().unwrap();
        let canvas = gl.surface.canvas();
        canvas.clear(Color::from_rgb(229, 231, 235));
        canvas.save();
        canvas.set_matrix(&view_matrix(&cam).into());
        if self.retained {
            self.culler.cull_groups(&self.scene, cam.view(W, H));
            let (band, rep_zoom) = zoom_band(cam.zoom);
            for &g in &self.culler.groups {
                let slot = &mut self.pictures[g as usize][band];
                if slot.is_none() {
                    let (bounds, range) = self.scene.groups[g as usize].clone();
                    let mut rec = PictureRecorder::new();
                    let c = rec.begin_recording(SRect::from_xywh(bounds.x as f32, bounds.y as f32, bounds.w as f32, bounds.h as f32), false);
                    let items = &self.scene.items;
                    draw(c, &self.scene, range.filter_map(|i| lod_at(&items[i as usize], rep_zoom).map(|l| (i, l))), cache, rep_zoom.min(8.0) as f32);
                    *slot = rec.finish_recording_as_picture(None);
                }
                canvas.draw_picture(slot.as_ref().unwrap(), None, None);
            }
        } else {
            draw(canvas, &self.scene, self.culler.visible.iter().copied(), cache, cam.zoom as f32);
        }
        canvas.restore();
        let encode_ms = t.ms();

        let t = Timer::start();
        gl.gr.flush_and_submit();
        gl.gl_surface.swap_buffers(&gl.gl_context).unwrap();
        unsafe { gl::Finish() };
        let gpu_ms = t.ms();

        if self.frame == 0 {
            self.startup["first_frame_since_main_ms"] = (self.t_main.elapsed().as_secs_f64() * 1e3).into();
        }
        if let Some(dir) = self.args.value("--png") {
            if matches!(self.frame, 150 | 450 | 750 | 950) {
                // Re-render into an offscreen raster for a clean screenshot.
                let mut s = skia_safe::surfaces::raster_n32_premul((W as i32, H as i32)).unwrap();
                s.canvas().clear(Color::from_rgb(229, 231, 235));
                s.canvas().set_matrix(&view_matrix(&cam).into());
                let mut c2 = Culler::new(true);
                c2.cull(&self.scene, cam.view(W, H), cam.zoom);
                draw(s.canvas(), &self.scene, c2.visible.iter().copied(), &Cache::new(&self.scene, None), cam.zoom as f32);
                save_png(&mut s, &format!("{dir}/skia_{:04}.png", self.frame));
            }
        }
        if self.frame % 30 == 0 {
            self.peak_rss = self.peak_rss.max(rss_mb());
        }
        let visible = if self.retained { self.culler.groups.iter().map(|&g| self.scene.groups[g as usize].1.len() as u32).sum() } else { self.culler.visible.len() as u32 };
        self.samples.push(FrameSample { phase, visible, cull_ms, encode_ms, gpu_ms, total_ms: total.ms() });
        self.frame += 1;
    }

    fn finish(&mut self) {
        let warm = &self.samples[10.min(self.samples.len())..];
        let phases = summarize(warm);
        let name = match (self.retained, self.culler.use_rtree) {
            (true, _) => "skia (retained pictures)",
            (false, true) => "skia (cull+LOD)",
            (false, false) => "skia (no cull)",
        };
        print_summary(name, &phases);
        let out = serde_json::json!({
            "renderer": "skia", "version": "skia-safe 0.156 / GL", "cull": self.culler.use_rtree, "retained": self.retained,
            "host": Host::detect(self.gpu_name.clone(), "OpenGL".into()),
            "viewport": [W, H], "scene": self.scene.stats(), "unsupported": {},
            "startup": self.startup, "peak_rss_mb": self.peak_rss, "phases": phases,
            "first_frames_ms": self.samples.iter().take(10).map(|s| s.total_ms).collect::<Vec<_>>(),
            "frames": self.samples,
        });
        write_json(self.args.value("--out"), &out);
    }
}

fn save_png(s: &mut Surface, path: &str) {
    let (w, h) = (s.width(), s.height());
    let info = ImageInfo::new((w, h), ColorType::RGBA8888, skia_safe::AlphaType::Unpremul, None);
    let mut px = vec![0u8; (w * h * 4) as usize];
    s.read_pixels(&info, &mut px, w as usize * 4, (0, 0));
    let _ = std::fs::create_dir_all(std::path::Path::new(path).parent().unwrap());
    image::save_buffer(path, &px, w as u32, h as u32, image::ColorType::Rgba8).unwrap();
}

fn run_cpu(args: &Args, scene: &Scene) {
    let cache = Cache::new(scene, None);
    let mut surface = skia_safe::surfaces::raster_n32_premul((EW as i32, EH as i32)).unwrap();
    let mut culler = Culler::new(true);
    let mut results = Vec::new();
    for (name, zoom) in [("detail_100pct", 1.0), ("overview_15pct", 0.15)] {
        let c = scenario::centered(scene, W, H, zoom, 0.5, 0.5);
        let cam = Camera { zoom: zoom * 2.0, ..c };
        let mut times = Vec::new();
        for _ in 0..5 {
            let t = Timer::start();
            culler.cull(scene, cam.view(EW, EH), cam.zoom);
            let canvas = surface.canvas();
            canvas.clear(Color::from_rgb(229, 231, 235));
            canvas.save();
            canvas.set_matrix(&view_matrix(&cam).into());
            draw(canvas, scene, culler.visible.iter().copied(), &cache, cam.zoom as f32);
            canvas.restore();
            // Force rasterization to complete by reading one pixel.
            let mut px = [0u8; 4];
            surface.read_pixels(&ImageInfo::new_n32_premul((1, 1), None), &mut px, 4, (0, 0));
            times.push(t.ms());
        }
        eprintln!("skia raster {name}: visible {} median {:.1} ms", culler.visible.len(), percentiles(times.clone()).p50);
        if let Some(dir) = args.value("--png") {
            save_png(&mut surface, &format!("{dir}/skia_cpu_{name}.png"));
        }
        results.push(serde_json::json!({ "view": name, "visible": culler.visible.len(), "ms": percentiles(times) }));
    }
    write_json(
        args.value("--out"),
        &serde_json::json!({ "renderer": "skia raster", "size": [EW, EH], "threads": 1, "host": Host::detect(String::new(), "cpu".into()), "results": results }),
    );
}

fn main() {
    let t_main = Instant::now();
    let args = Args::from_env();
    let cfg = SceneConfig { target_items: args.usize_or("--items", 100_000), ..Default::default() };
    let t = Timer::start();
    let scene = Scene::generate(&cfg);
    let gen_ms = t.ms();
    if args.flag("--cpu") {
        run_cpu(&args, &scene);
        return;
    }
    let steps = scenario::build(&scene, W, H, args.usize_or("--frames", 300));
    eprintln!("scene: {:?} gen {gen_ms:.0} ms", scene.stats());
    let retained = args.flag("--retained");
    let pictures = (0..scene.groups.len()).map(|_| Default::default()).collect();
    let startup = serde_json::json!({ "scene_gen_ms": gen_ms, "shaping_ms": scene.shape_time_ms });
    let mut app = App {
        culler: Culler::new(!args.flag("--no-cull")),
        args,
        scene,
        cache: None,
        steps,
        frame: 0,
        samples: Vec::new(),
        retained,
        pictures,
        gl: None,
        t_main,
        startup,
        peak_rss: 0.0,
        gpu_name: String::new(),
    };
    EventLoop::new().unwrap().run_app(&mut app).unwrap();
}
