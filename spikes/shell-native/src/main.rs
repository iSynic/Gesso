//! Shell spike (a): winit + wgpu with a Vello canvas (10k nodes, retained per-artboard
//! fragments) and an egui properties panel (x, y, w, h, fill) kept in sync both ways.
//!
//! Interactive: click a node to select it, drag to move it, edit it in the panel; right-drag pans,
//! wheel zooms. `--auto` runs a scripted drag + panel-edit session and writes latency JSON.

use common::*;
use render_vello::{encode, Cache};
use std::sync::Arc;
use vello::kurbo::Affine;
use vello::peniko::Color;
use vello::{wgpu, AaConfig, RenderParams, Renderer, RendererOptions, Scene as VScene};
use web_time::Instant;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowId};

const W: u32 = 1600;
const H: u32 = 900;
const PANEL_W: f32 = 260.0;

struct Gfx {
    window: Arc<Window>,
    cx: vello::util::RenderContext,
    surface: vello::util::RenderSurface<'static>,
    vello: Renderer,
    egui: egui::Context,
    egui_state: egui_winit::State,
    egui_rend: egui_wgpu::Renderer,
}

#[derive(Default)]
struct Lat {
    /// Input handled -> present() returned (CPU side of the frame).
    to_present_ms: Vec<f64>,
    /// Input handled -> GPU finished the frame (measured with a blocking poll in --auto only).
    to_gpu_done_ms: Vec<f64>,
    egui_ms: Vec<f64>,
    canvas_encode_ms: Vec<f64>,
}

struct App {
    args: Args,
    scene: Scene,
    cache: Cache,
    frags: Vec<Option<VScene>>,
    vscene: VScene,
    culler: Culler,
    cam: Camera,
    selected: Option<u32>,
    drag: Option<(f64, f64)>,
    pan: Option<(f64, f64)>,
    cursor: (f64, f64),
    pending_input: Option<Instant>,
    gfx: Option<Gfx>,
    lat_drag: Lat,
    lat_panel: Lat,
    auto_frame: usize,
    t_main: Instant,
    first_frame_ms: f64,
}

impl App {
    fn invalidate(&mut self, item: u32) {
        let g = self.scene.group_of(item);
        self.frags[g] = None;
    }

    fn set_rect(&mut self, item: u32, r: Rect) {
        let old = Scene::visual_bounds(&self.scene.items[item as usize]);
        self.scene.items[item as usize].rect = r;
        self.scene.update_index(item, old);
        self.invalidate(item);
    }

    fn world(&self, p: (f64, f64)) -> (f64, f64) {
        (self.cam.x + p.0 / self.cam.zoom, self.cam.y + p.1 / self.cam.zoom)
    }

    fn hit(&self, p: (f64, f64)) -> Option<u32> {
        let (x, y) = self.world(p);
        self.scene
            .index
            .locate_all_at_point([x, y])
            .map(|g| g.data)
            .filter(|&i| {
                let it = &self.scene.items[i as usize];
                it.rect.w != ARTBOARD_W && x >= it.rect.x && x <= it.rect.x + it.rect.w && y >= it.rect.y && y <= it.rect.y + it.rect.h
            })
            .max()
    }

    fn drag_to(&mut self, p: (f64, f64)) {
        if let (Some(sel), Some(last)) = (self.selected, self.drag) {
            let (dx, dy) = ((p.0 - last.0) / self.cam.zoom, (p.1 - last.1) / self.cam.zoom);
            let mut r = self.scene.items[sel as usize].rect;
            r.x += dx;
            r.y += dy;
            self.set_rect(sel, r);
            self.drag = Some(p);
            self.pending_input.get_or_insert(Instant::now());
        }
    }

    fn panel(&mut self, root: &mut egui::Ui) -> bool {
        let mut changed = None;
        egui::Panel::right("props").exact_size(PANEL_W).show(root, |ui| {
            ui.heading("Properties");
            let Some(sel) = self.selected else {
                ui.label("Click a node on the canvas.");
                return;
            };
            let it = &self.scene.items[sel as usize];
            let mut r = it.rect;
            let mut fill = egui::Color32::from_rgba_unmultiplied(it.fill[0], it.fill[1], it.fill[2], it.fill[3]);
            ui.label(format!("Node {sel} ({:?})", it.kind));
            egui::Grid::new("g").num_columns(2).show(ui, |ui| {
                for (name, v) in [("X", &mut r.x), ("Y", &mut r.y), ("W", &mut r.w), ("H", &mut r.h)] {
                    ui.label(name);
                    ui.add(egui::DragValue::new(v).speed(1.0));
                    ui.end_row();
                }
                ui.label("Fill");
                ui.color_edit_button_srgba(&mut fill);
                ui.end_row();
            });
            if r != it.rect || fill.to_srgba_unmultiplied() != it.fill {
                changed = Some((sel, r, fill.to_srgba_unmultiplied()));
            }
        });
        if let Some((sel, r, fill)) = changed {
            self.scene.items[sel as usize].fill = fill;
            self.set_rect(sel, r);
            self.pending_input.get_or_insert(Instant::now());
            return true;
        }
        false
    }

    fn redraw(&mut self) {
        let auto = self.args.flag("--auto");
        // Scripted session: 300 frames of dragging, then 200 frames of panel edits.
        let mut panel_phase = false;
        if auto {
            let f = self.auto_frame;
            if f == 5 {
                let c = (W as f64 * 0.3, H as f64 * 0.4);
                self.selected = self.hit(c);
                self.drag = Some(c);
            } else if (6..306).contains(&f) {
                let last = self.drag.unwrap();
                self.drag_to((last.0 + 1.0, last.1 + if f % 2 == 0 { 0.5 } else { -0.5 }));
            } else if (306..506).contains(&f) {
                self.drag = None;
                panel_phase = true;
                if let Some(sel) = self.selected {
                    // The same path a DragValue edit takes.
                    let mut r = self.scene.items[sel as usize].rect;
                    r.w = 200.0 + (f % 40) as f64;
                    self.set_rect(sel, r);
                    self.pending_input.get_or_insert(Instant::now());
                }
            } else if f >= 506 {
                self.finish();
                std::process::exit(0);
            }
            self.auto_frame += 1;
        }

        let gfx = self.gfx.as_mut().unwrap();
        // egui.
        let t = Timer::start();
        let raw = gfx.egui_state.take_egui_input(&gfx.window);
        let ctx = gfx.egui.clone();
        let mut panel_changed = false;
        let out = ctx.run_ui(raw, |ui| panel_changed = self.panel(ui));
        let gfx = self.gfx.as_mut().unwrap();
        gfx.egui_state.handle_platform_output(&gfx.window, out.platform_output);
        let prims = gfx.egui.tessellate(out.shapes, out.pixels_per_point);
        let egui_ms = t.ms();

        // Canvas (retained per-artboard fragments, as in the renderer spike).
        let t = Timer::start();
        self.vscene.reset();
        let view = Affine::new(self.cam.affine());
        self.culler.cull_groups(&self.scene, self.cam.view(W, H));
        let (_, rep) = zoom_band(self.cam.zoom);
        for &g in &self.culler.groups {
            let slot = &mut self.frags[g as usize];
            if slot.is_none() {
                let mut f = VScene::new();
                let items = &self.scene.items;
                encode(&mut f, &self.scene, self.scene.groups[g as usize].1.clone().filter_map(|i| lod_at(&items[i as usize], rep).map(|l| (i, l))), Affine::IDENTITY, &self.cache);
                *slot = Some(f);
            }
            self.vscene.append(slot.as_ref().unwrap(), Some(view));
        }
        if let Some(sel) = self.selected {
            let r = self.scene.items[sel as usize].rect;
            self.vscene.stroke(
                &vello::kurbo::Stroke::new(2.0 / self.cam.zoom),
                view,
                Color::from_rgba8(13, 153, 255, 255),
                None,
                &vello::kurbo::Rect::new(r.x, r.y, r.x + r.w, r.y + r.h),
            );
        }
        let canvas_ms = t.ms();

        let gfx = self.gfx.as_mut().unwrap();
        let dev = &gfx.cx.devices[gfx.surface.dev_id];
        gfx.vello
            .render_to_texture(&dev.device, &dev.queue, &self.vscene, &gfx.surface.target_view, &RenderParams {
                base_color: Color::from_rgba8(229, 231, 235, 255),
                width: W,
                height: H,
                antialiasing_method: AaConfig::Area,
            })
            .unwrap();
        let frame = match gfx.surface.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            other => panic!("surface: {other:?}"),
        };
        let view = frame.texture.create_view(&Default::default());
        let mut enc = dev.device.create_command_encoder(&Default::default());
        gfx.surface.blitter.copy(&dev.device, &mut enc, &gfx.surface.target_view, &view);
        let sd = egui_wgpu::ScreenDescriptor { size_in_pixels: [W, H], pixels_per_point: out.pixels_per_point };
        for (id, deltas) in &out.textures_delta.set {
            for delta in deltas {
                gfx.egui_rend.update_texture(&dev.device, &dev.queue, *id, delta);
            }
        }
        let extra = gfx.egui_rend.update_buffers(&dev.device, &dev.queue, &mut enc, &prims, &sd);
        {
            let mut rp = enc
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("egui"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                    })],
                    ..Default::default()
                })
                .forget_lifetime();
            gfx.egui_rend.render(&mut rp, &prims, &sd);
        }
        for id in &out.textures_delta.free {
            gfx.egui_rend.free_texture(id);
        }
        dev.queue.submit(extra.into_iter().chain([enc.finish()]));
        gfx.window.pre_present_notify();
        dev.queue.present(frame);
        if self.first_frame_ms == 0.0 {
            self.first_frame_ms = self.t_main.elapsed().as_secs_f64() * 1e3;
        }
        if let Some(t0) = self.pending_input.take() {
            let lat = if panel_phase || panel_changed { &mut self.lat_panel } else { &mut self.lat_drag };
            lat.to_present_ms.push(t0.elapsed().as_secs_f64() * 1e3);
            if auto {
                dev.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                lat.to_gpu_done_ms.push(t0.elapsed().as_secs_f64() * 1e3);
            }
            lat.egui_ms.push(egui_ms);
            lat.canvas_encode_ms.push(canvas_ms);
        }
        if auto || out.viewport_output.values().any(|v| v.repaint_delay.is_zero()) {
            gfx.window.request_redraw();
        }
    }

    fn finish(&self) {
        let s = |l: &Lat| {
            serde_json::json!({
                "to_present": percentiles(l.to_present_ms.clone()), "to_gpu_done": percentiles(l.to_gpu_done_ms.clone()),
                "egui": percentiles(l.egui_ms.clone()), "canvas_encode": percentiles(l.canvas_encode_ms.clone()),
            })
        };
        let out = serde_json::json!({
            "shell": "native winit 0.30 + wgpu 30 + vello 0.11 + egui 0.37", "viewport": [W, H], "nodes": self.scene.items.len(),
            "present_mode": if self.args.flag("--novsync") { "AutoNoVsync" } else { "AutoVsync" },
            "first_frame_since_main_ms": self.first_frame_ms, "rss_mb": rss_mb(),
            "drag": s(&self.lat_drag), "panel_edit": s(&self.lat_panel),
        });
        eprintln!("{}", serde_json::to_string_pretty(&out["drag"]["to_gpu_done"]).unwrap());
        write_json(self.args.value("--out"), &out);
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.gfx.is_some() {
            return;
        }
        let window = Arc::new(el.create_window(Window::default_attributes().with_title("shell-native (winit + wgpu + vello + egui)").with_inner_size(winit::dpi::PhysicalSize::new(W, H)).with_resizable(false)).unwrap());
        let mut cx = vello::util::RenderContext::new();
        let mode = if self.args.flag("--novsync") { wgpu::PresentMode::AutoNoVsync } else { wgpu::PresentMode::AutoVsync };
        let surface = pollster::block_on(cx.create_surface(window.clone(), W, H, mode)).unwrap();
        let dev = &cx.devices[surface.dev_id];
        let vello = Renderer::new(&dev.device, RendererOptions { antialiasing_support: vello::AaSupport::area_only(), ..Default::default() }).unwrap();
        let egui_rend = egui_wgpu::Renderer::new(&dev.device, surface.format, egui_wgpu::RendererOptions::default());
        let egui = egui::Context::default();
        let egui_state = egui_winit::State::new(egui.clone(), egui::ViewportId::ROOT, &window, Some(window.scale_factor() as f32), None, None);
        self.gfx = Some(Gfx { window, cx, surface, vello, egui, egui_state, egui_rend });
        self.gfx.as_ref().unwrap().window.request_redraw();
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let gfx = self.gfx.as_mut().unwrap();
        let resp = gfx.egui_state.on_window_event(&gfx.window, &event);
        let over_panel = self.cursor.0 > (W as f64 - PANEL_W as f64 * gfx.window.scale_factor());
        let mut redraw = resp.repaint;
        match event {
            WindowEvent::CloseRequested => {
                self.finish();
                el.exit();
                return;
            }
            WindowEvent::RedrawRequested => {
                self.redraw();
                return;
            }
            WindowEvent::CursorMoved { position, .. } => {
                let p = (position.x, position.y);
                self.cursor = p;
                if self.drag.is_some() {
                    self.drag_to(p);
                    redraw = true;
                } else if let Some(last) = self.pan {
                    self.cam.x -= (p.0 - last.0) / self.cam.zoom;
                    self.cam.y -= (p.1 - last.1) / self.cam.zoom;
                    self.pan = Some(p);
                    self.pending_input.get_or_insert(Instant::now());
                    redraw = true;
                }
            }
            WindowEvent::MouseInput { state, button, .. } if !resp.consumed && !over_panel => match (button, state) {
                (MouseButton::Left, ElementState::Pressed) => {
                    self.selected = self.hit(self.cursor);
                    self.drag = self.selected.map(|_| self.cursor);
                    redraw = true;
                }
                (MouseButton::Right, ElementState::Pressed) => self.pan = Some(self.cursor),
                _ => {}
            },
            WindowEvent::MouseInput { state: ElementState::Released, .. } => {
                self.drag = None;
                self.pan = None;
            }
            WindowEvent::MouseWheel { delta, .. } if !over_panel => {
                let d = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y as f64,
                    MouseScrollDelta::PixelDelta(p) => p.y / 60.0,
                };
                let before = self.world(self.cursor);
                self.cam.zoom = (self.cam.zoom * 1.15f64.powf(d)).clamp(0.02, 16.0);
                self.cam.x = before.0 - self.cursor.0 / self.cam.zoom;
                self.cam.y = before.1 - self.cursor.1 / self.cam.zoom;
                self.pending_input.get_or_insert(Instant::now());
                redraw = true;
            }
            _ => {}
        }
        if redraw {
            self.gfx.as_ref().unwrap().window.request_redraw();
        }
    }
}

fn main() {
    let t_main = Instant::now();
    let args = Args::from_env();
    let scene = Scene::generate(&SceneConfig { target_items: args.usize_or("--items", 10_000), ..Default::default() });
    let cache = Cache::new(&scene);
    let cam = scenario::centered(&scene, W, H, 0.75, 0.2, 0.2);
    let mut app = App {
        frags: (0..scene.groups.len()).map(|_| None).collect(),
        args,
        scene,
        cache,
        vscene: VScene::new(),
        culler: Culler::new(true),
        cam,
        selected: None,
        drag: None,
        pan: None,
        cursor: (0.0, 0.0),
        pending_input: None,
        gfx: None,
        lat_drag: Lat::default(),
        lat_panel: Lat::default(),
        auto_frame: 0,
        t_main,
        first_frame_ms: 0.0,
    };
    EventLoop::new().unwrap().run_app(&mut app).unwrap();
}
