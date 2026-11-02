//! Renderer spike: Vello (GPU compute on wgpu) running the shared pan/zoom/edit scenario.
//!
//! Usage: render-vello [--no-cull] [--frames N] [--items N] [--out file.json] [--png dir]
//!        render-vello --cpu [--out file.json] [--png dir]   (vello_cpu 4K export timing)

mod cpu;

use common::*;
use std::sync::Arc;
use render_vello::{encode, Cache};
pub use render_vello::{color, to_bez};
use std::time::Instant;
use vello::kurbo::Affine;
use vello::peniko::Color;
use vello::{wgpu, AaConfig, RenderParams, Renderer, RendererOptions, Scene as VScene};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowId};

pub const W: u32 = 1920;
pub const H: u32 = 1080;

struct Gpu {
    window: Arc<Window>,
    cx: vello::util::RenderContext,
    surface: vello::util::RenderSurface<'static>,
    renderer: Renderer,
}

struct App {
    args: Args,
    scene: Scene,
    cache: Cache,
    culler: Culler,
    steps: Vec<Step>,
    frame: usize,
    samples: Vec<FrameSample>,
    vscene: VScene,
    retained: bool,
    /// Per-artboard encoded fragments, one per LOD level (Full, Simple, Greek).
    frags: Vec<[Option<VScene>; 3]>,
    gpu: Option<Gpu>,
    t_main: Instant,
    startup: serde_json::Value,
    peak_rss: f64,
    adapter: (String, String),
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.gpu.is_some() {
            return;
        }
        let t = Timer::start();
        let window = Arc::new(
            el.create_window(Window::default_attributes().with_title("render-vello").with_inner_size(winit::dpi::PhysicalSize::new(W, H)).with_resizable(false))
                .unwrap(),
        );
        let t_window = t.ms();
        let mut cx = vello::util::RenderContext::new();
        let mut surface = pollster::block_on(cx.create_surface(window.clone(), W, H, wgpu::PresentMode::AutoNoVsync)).unwrap();
        // Same as vello's target but readable for screenshots.
        let tex = cx.devices[surface.dev_id].device.create_texture(&wgpu::TextureDescriptor {
            label: Some("target"),
            size: wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        surface.target_view = tex.create_view(&Default::default());
        surface.target_texture = tex;
        let t_surface = t.ms();
        let dev = &cx.devices[surface.dev_id];
        let info = dev.adapter().get_info();
        self.adapter = (info.name.clone(), format!("{:?} {}", info.backend, info.driver_info));
        let renderer = Renderer::new(&dev.device, RendererOptions { antialiasing_support: vello::AaSupport::area_only(), ..Default::default() }).unwrap();
        let t_renderer = t.ms();
        let s = serde_json::json!({
            "window_ms": t_window, "device_surface_ms": t_surface - t_window, "renderer_init_ms": t_renderer - t_surface,
        });
        for (k, v) in s.as_object().unwrap() {
            self.startup[k] = v.clone();
        }
        self.gpu = Some(Gpu { window, cx, surface, renderer });
        self.gpu.as_ref().unwrap().window.request_redraw();
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
                self.gpu.as_ref().unwrap().window.request_redraw();
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
        }
        let t = Timer::start();
        if !self.retained {
            self.culler.cull(&self.scene, cam.view(W, H), cam.zoom);
        }
        let cull_ms = t.ms();
        let t = Timer::start();
        self.vscene.reset();
        let view = Affine::new(cam.affine());
        if self.retained {
            if let Some(e) = edit {
                self.frags[self.scene.group_of(e)] = Default::default();
            }
            self.culler.cull_groups(&self.scene, cam.view(W, H));
            let (li, rep_zoom) = zoom_band(cam.zoom);
            for &g in &self.culler.groups {
                let slot = &mut self.frags[g as usize][li];
                if slot.is_none() {
                    let mut f = VScene::new();
                    let range = self.scene.groups[g as usize].1.clone();
                    let items = &self.scene.items;
                    encode(&mut f, &self.scene, range.filter_map(|i| lod_at(&items[i as usize], rep_zoom).map(|l| (i, l))), Affine::IDENTITY, &self.cache);
                    *slot = Some(f);
                }
                self.vscene.append(slot.as_ref().unwrap(), Some(view));
            }
        } else {
            encode(&mut self.vscene, &self.scene, self.culler.visible.iter().copied(), view, &self.cache);
        }
        let encode_ms = t.ms();

        let t = Timer::start();
        let gpu = self.gpu.as_mut().unwrap();
        let dev = &gpu.cx.devices[gpu.surface.dev_id];
        gpu.renderer
            .render_to_texture(
                &dev.device,
                &dev.queue,
                &self.vscene,
                &gpu.surface.target_view,
                &RenderParams { base_color: Color::from_rgba8(229, 231, 235, 255), width: W, height: H, antialiasing_method: AaConfig::Area },
            )
            .unwrap();
        let frame = gpu.surface.surface.get_current_texture();
        let frame = match frame {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            other => panic!("surface: {other:?}"),
        };
        let mut enc = dev.device.create_command_encoder(&Default::default());
        let view = frame.texture.create_view(&Default::default());
        gpu.surface.blitter.copy(&dev.device, &mut enc, &gpu.surface.target_view, &view);
        dev.queue.submit([enc.finish()]);
        dev.queue.present(frame);
        dev.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let gpu_ms = t.ms();

        if self.frame == 0 {
            self.startup["first_frame_since_main_ms"] = (self.t_main.elapsed().as_secs_f64() * 1e3).into();
        }
        if let Some(dir) = self.args.value("--png") {
            if matches!(self.frame, 150 | 450 | 750 | 950) {
                let path = format!("{dir}/vello_{:04}.png", self.frame);
                screenshot(&dev.device, &dev.queue, &gpu.surface.target_texture, &path);
            }
        }
        if self.frame % 30 == 0 {
            self.peak_rss = self.peak_rss.max(rss_mb());
        }
        self.samples.push(FrameSample { phase, visible: if self.retained { self.culler.groups.iter().map(|&g| self.scene.groups[g as usize].1.len() as u32).sum() } else { self.culler.visible.len() as u32 }, cull_ms, encode_ms, gpu_ms, total_ms: total.ms() });
        self.frame += 1;
    }

    fn finish(&mut self) {
        // Skip the first 10 frames (shader/pipeline warm-up) from the percentiles.
        let warm = &self.samples[10.min(self.samples.len())..];
        let phases = summarize(warm);
        let name = match (self.retained, self.culler.use_rtree) {
            (true, _) => "vello (retained fragments)",
            (false, true) => "vello (cull+LOD)",
            (false, false) => "vello (no cull)",
        };
        print_summary(name, &phases);
        let stats = self.scene.stats();
        let out = serde_json::json!({
            "renderer": "vello", "version": "0.11.0", "cull": self.culler.use_rtree, "retained": self.retained,
            "host": Host::detect(self.adapter.0.clone(), self.adapter.1.clone()),
            "viewport": [W, H], "scene": stats, "unsupported": { "layer_blur_items": stats.blurs },
            "startup": self.startup, "peak_rss_mb": self.peak_rss, "phases": phases,
            "first_frames_ms": self.samples.iter().take(10).map(|s| s.total_ms).collect::<Vec<_>>(),
            "frames": self.samples,
        });
        write_json(self.args.value("--out"), &out);
    }
}

pub fn screenshot(device: &wgpu::Device, queue: &wgpu::Queue, tex: &wgpu::Texture, path: &str) {
    let (w, h) = (tex.width(), tex.height());
    let bpr = (w * 4).div_ceil(256) * 256;
    let buf = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: (bpr * h) as u64, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
    let mut enc = device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        tex.as_image_copy(),
        wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bpr), rows_per_image: None } },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    queue.submit([enc.finish()]);
    buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let data = buf.slice(..).get_mapped_range().unwrap();
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for row in 0..h {
        rgba.extend_from_slice(&data[(row * bpr) as usize..(row * bpr + w * 4) as usize]);
    }
    let _ = std::fs::create_dir_all(std::path::Path::new(path).parent().unwrap());
    image::save_buffer(path, &rgba, w, h, image::ColorType::Rgba8).unwrap();
}

fn main() {
    let t_main = Instant::now();
    let args = Args::from_env();
    let cfg = SceneConfig { target_items: args.usize_or("--items", 100_000), ..Default::default() };
    let t = Timer::start();
    let scene = Scene::generate(&cfg);
    let gen_ms = t.ms();
    if args.flag("--cpu") {
        cpu::run(&args, &scene);
        return;
    }
    let t = Timer::start();
    let cache = Cache::new(&scene);
    let cache_ms = t.ms();
    let steps = scenario::build(&scene, W, H, args.usize_or("--frames", 300));
    let shape_ms = scene.shape_time_ms;
    eprintln!("scene: {:?} gen {gen_ms:.0} ms (shaping {:.0} ms)", scene.stats(), scene.shape_time_ms);
    let retained = args.flag("--retained");
    let frags = (0..scene.groups.len()).map(|_| Default::default()).collect();
    let mut app = App {
        culler: Culler::new(!args.flag("--no-cull")),
        args,
        scene,
        cache,
        steps,
        frame: 0,
        samples: Vec::new(),
        vscene: VScene::new(),
        retained,
        frags,
        gpu: None,
        t_main,
        startup: serde_json::json!({ "scene_gen_ms": gen_ms, "shaping_ms": shape_ms, "renderer_cache_ms": cache_ms }),
        peak_rss: 0.0,
        adapter: Default::default(),
    };
    let el = EventLoop::new().unwrap();
    el.run_app(&mut app).unwrap();

}
