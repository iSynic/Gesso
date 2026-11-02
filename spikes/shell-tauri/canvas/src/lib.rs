//! Shell spike (b) canvas: the same Vello renderer and scene as the native shell, compiled to
//! WASM and drawing through WebGPU into an HTML canvas inside the Tauri webview.

use common::*;
use render_vello::{encode, Cache};
use std::sync::atomic::{AtomicU32, Ordering};
use vello::kurbo::Affine;
use vello::peniko::Color;
use vello::{wgpu, AaConfig, RenderParams, Renderer, RendererOptions, Scene as VScene};
use wasm_bindgen::prelude::*;

static GPU_DONE: AtomicU32 = AtomicU32::new(0);

#[wasm_bindgen]
pub struct Canvas {
    scene: Scene,
    cache: Cache,
    frags: Vec<Option<VScene>>,
    vscene: VScene,
    culler: Culler,
    cam: Camera,
    selected: Option<u32>,
    drag: Option<(f64, f64)>,
    cx: vello::util::RenderContext,
    surface: vello::util::RenderSurface<'static>,
    vello: Renderer,
    w: u32,
    h: u32,
    submitted: u32,
}

#[wasm_bindgen]
impl Canvas {
    /// `fonts`: array of Uint8Array font files (the webview has no system font access from WASM).
    pub async fn create(canvas: web_sys::HtmlCanvasElement, fonts: js_sys::Array, items: usize) -> Result<Canvas, JsValue> {
        console_error_panic_hook::set_once();
        let (w, h) = (canvas.width(), canvas.height());
        let mut shaper = text::Shaper::default();
        let mut families = Vec::new();
        for f in fonts.iter() {
            families.extend(shaper.register_font(js_sys::Uint8Array::new(&f).to_vec()));
        }
        families.dedup();
        shaper.family = families.join(", ");
        let scene = Scene::generate_with(&SceneConfig { target_items: items, ..Default::default() }, &mut shaper);
        let cache = Cache::new(&scene);
        let mut cx = vello::util::RenderContext::new();
        let surface = cx
            .create_surface(wgpu::SurfaceTarget::Canvas(canvas), w, h, wgpu::PresentMode::AutoVsync)
            .await
            .map_err(|e| JsValue::from_str(&format!("surface: {e}")))?;
        let dev = &cx.devices[surface.dev_id];
        let vello = Renderer::new(&dev.device, RendererOptions { antialiasing_support: vello::AaSupport::area_only(), num_init_threads: None, ..Default::default() })
            .map_err(|e| JsValue::from_str(&format!("vello: {e}")))?;
        let cam = scenario::centered(&scene, w, h, 0.75, 0.2, 0.2);
        Ok(Canvas {
            frags: (0..scene.groups.len()).map(|_| None).collect(),
            scene,
            cache,
            vscene: VScene::new(),
            culler: Culler::new(true),
            cam,
            selected: None,
            drag: None,
            cx,
            surface,
            vello,
            w,
            h,
            submitted: 0,
        })
    }

    pub fn adapter_info(&self) -> String {
        let info = self.cx.devices[self.surface.dev_id].adapter().get_info();
        format!("{} ({:?})", info.name, info.backend)
    }

    pub fn node_count(&self) -> usize {
        self.scene.items.len()
    }

    fn world(&self, p: (f64, f64)) -> (f64, f64) {
        (self.cam.x + p.0 / self.cam.zoom, self.cam.y + p.1 / self.cam.zoom)
    }

    fn set_rect(&mut self, item: u32, r: Rect) {
        let old = Scene::visual_bounds(&self.scene.items[item as usize]);
        self.scene.items[item as usize].rect = r;
        self.scene.update_index(item, old);
        let g = self.scene.group_of(item);
        self.frags[g] = None;
    }

    /// Select the topmost node under the point; returns its id or -1.
    pub fn pointer_down(&mut self, x: f64, y: f64) -> i32 {
        let (wx, wy) = self.world((x, y));
        self.selected = self
            .scene
            .index
            .locate_all_at_point([wx, wy])
            .map(|g| g.data)
            .filter(|&i| {
                let it = &self.scene.items[i as usize];
                it.rect.w != ARTBOARD_W && wx >= it.rect.x && wx <= it.rect.x + it.rect.w && wy >= it.rect.y && wy <= it.rect.y + it.rect.h
            })
            .max();
        self.drag = self.selected.map(|_| (x, y));
        self.selected.map_or(-1, |s| s as i32)
    }

    pub fn pointer_up(&mut self) {
        self.drag = None;
    }

    pub fn drag_to(&mut self, x: f64, y: f64) {
        if let (Some(sel), Some(last)) = (self.selected, self.drag) {
            let mut r = self.scene.items[sel as usize].rect;
            r.x += (x - last.0) / self.cam.zoom;
            r.y += (y - last.1) / self.cam.zoom;
            self.set_rect(sel, r);
            self.drag = Some((x, y));
        }
    }

    /// Selected node as JSON `{id,x,y,w,h,fill}`, or "null".
    pub fn props(&self) -> String {
        self.selected.map_or("null".into(), |s| {
            let it = &self.scene.items[s as usize];
            serde_json::json!({"id": s, "x": it.rect.x, "y": it.rect.y, "w": it.rect.w, "h": it.rect.h, "fill": it.fill}).to_string()
        })
    }

    pub fn set_props(&mut self, x: f64, y: f64, w: f64, h: f64, r: u8, g: u8, b: u8) {
        if let Some(s) = self.selected {
            self.scene.items[s as usize].fill = [r, g, b, 255];
            self.set_rect(s, Rect::new(x, y, w, h));
        }
    }

    pub fn gpu_done_count() -> u32 {
        GPU_DONE.load(Ordering::Relaxed)
    }

    /// Encode + submit + present one frame. Returns the submission number; it is complete once
    /// `gpu_done_count()` reaches it.
    pub fn render(&mut self) -> u32 {
        self.vscene.reset();
        let view = Affine::new(self.cam.affine());
        self.culler.cull_groups(&self.scene, self.cam.view(self.w, self.h));
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
            self.vscene.stroke(&vello::kurbo::Stroke::new(2.0 / self.cam.zoom), view, Color::from_rgba8(13, 153, 255, 255), None, &vello::kurbo::Rect::new(r.x, r.y, r.x + r.w, r.y + r.h));
        }
        let dev = &self.cx.devices[self.surface.dev_id];
        self.vello
            .render_to_texture(&dev.device, &dev.queue, &self.vscene, &self.surface.target_view, &RenderParams {
                base_color: Color::from_rgba8(229, 231, 235, 255),
                width: self.w,
                height: self.h,
                antialiasing_method: AaConfig::Area,
            })
            .unwrap();
        let frame = match self.surface.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            other => panic!("surface: {other:?}"),
        };
        let tv = frame.texture.create_view(&Default::default());
        let mut enc = dev.device.create_command_encoder(&Default::default());
        self.surface.blitter.copy(&dev.device, &mut enc, &self.surface.target_view, &tv);
        dev.queue.submit([enc.finish()]);
        dev.queue.on_submitted_work_done(|| {
            GPU_DONE.fetch_add(1, Ordering::Relaxed);
        });
        dev.queue.present(frame);
        self.submitted += 1;
        self.submitted
    }
}
