//! The window's GPU side: one wgpu device shared by Vello (the canvas, rendered into an
//! offscreen texture) and egui (panels and overlays, plus the canvas texture as an image),
//! which draws everything to the window surface in one pass.

use std::sync::Arc;

use render::{Frame, Renderer};
use render_vello::{GpuTarget, VelloRenderer};
use vello::wgpu;
use winit::window::Window;

pub struct Gfx {
    pub window: Arc<Window>,
    cx: vello::util::RenderContext,
    surface: vello::util::RenderSurface<'static>,
    pub vello: VelloRenderer,
    target: GpuTarget,
    pub canvas_tex: egui::TextureId,
    pub egui: egui::Context,
    pub egui_state: egui_winit::State,
    egui_rend: egui_wgpu::Renderer,
}

fn present_mode(novsync: bool) -> wgpu::PresentMode {
    if novsync { wgpu::PresentMode::AutoNoVsync } else { wgpu::PresentMode::AutoVsync }
}

impl Gfx {
    pub fn new(window: Arc<Window>, novsync: bool) -> Result<Self, String> {
        let size = window.inner_size();
        let mut cx = vello::util::RenderContext::new();
        let surface = pollster::block_on(cx.create_surface(window.clone(), size.width.max(1), size.height.max(1), present_mode(novsync))).map_err(|e| format!("surface: {e}"))?;
        let dev = &cx.devices[surface.dev_id];
        let vello = VelloRenderer::new(dev.device.clone(), dev.queue.clone()).map_err(|e| e.to_string())?;
        let target = vello.target(size.width.max(1), size.height.max(1));
        let mut egui_rend = egui_wgpu::Renderer::new(&dev.device, surface.format, egui_wgpu::RendererOptions::default());
        let view = target.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let canvas_tex = egui_rend.register_native_texture(&dev.device, &view, wgpu::FilterMode::Nearest);
        let egui = egui::Context::default();
        egui.set_fonts(ui_fonts());
        // Ctrl+plus and Ctrl+minus zoom the canvas; the interface scale has its own menu.
        egui.options_mut(|o| o.zoom_with_keyboard = false);
        let mut egui_state = egui_winit::State::new(egui.clone(), egui::ViewportId::ROOT, &window, Some(window.scale_factor() as f32), None, None);
        egui_state.set_max_texture_side(dev.device.limits().max_texture_dimension_2d as usize);
        Ok(Gfx { window, cx, surface, vello, target, canvas_tex, egui, egui_state, egui_rend })
    }

    pub fn adapter(&self) -> String {
        let i = self.cx.devices[self.surface.dev_id].adapter().get_info();
        format!("{} ({:?}, driver {})", i.name, i.backend, i.driver_info)
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        if w > 0 && h > 0 {
            self.cx.resize_surface(&mut self.surface, w, h);
        }
    }

    /// Renders the canvas into the offscreen texture, resizing it (and re-pointing egui's image
    /// at it) when the canvas size changed.
    pub fn render_canvas(&mut self, frame: &Frame) -> Result<(), String> {
        if (self.target.width, self.target.height) != (frame.width, frame.height) {
            self.target = self.vello.target(frame.width, frame.height);
            let dev = &self.cx.devices[self.surface.dev_id];
            let view = self.target.texture.create_view(&wgpu::TextureViewDescriptor::default());
            self.egui_rend.update_egui_texture_from_wgpu_texture(&dev.device, &view, wgpu::FilterMode::Nearest, self.canvas_tex);
        }
        self.vello.render(frame, &mut self.target).map(|_| ()).map_err(|e| e.to_string())
    }

    /// Draws egui's output (which includes the canvas image) to the window and presents.
    pub fn present(&mut self, mut out: egui::FullOutput, clear: [f64; 4], capture: Option<&std::path::Path>) -> Result<(), String> {
        let ppp = out.pixels_per_point;
        let prims = self.egui.tessellate(std::mem::take(&mut out.shapes), ppp);
        let dev = &self.cx.devices[self.surface.dev_id];
        // Texture uploads first: they must land even if this frame can't be presented.
        let mut textures = std::mem::take(&mut out.textures_delta);
        for (id, deltas) in &textures.set {
            for delta in deltas {
                self.egui_rend.update_texture(&dev.device, &dev.queue, *id, delta);
            }
        }
        let frees = std::mem::take(&mut textures.free);
        textures.clear();
        let frame = match self.surface.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            other => {
                // Lost or outdated (resize, display change): reconfigure and draw next time.
                let size = self.window.inner_size();
                self.cx.resize_surface(&mut self.surface, size.width.max(1), size.height.max(1));
                for id in &frees {
                    self.egui_rend.free_texture(id);
                }
                return Err(format!("surface: {other:?}"));
            }
        };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut enc = dev.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("egui") });
        let sd = egui_wgpu::ScreenDescriptor { size_in_pixels: [self.surface.config.width, self.surface.config.height], pixels_per_point: ppp };
        let extra = self.egui_rend.update_buffers(&dev.device, &dev.queue, &mut enc, &prims, &sd);
        // A copy of the whole window for screenshots (scripted sessions).
        let (cw, ch) = (self.surface.config.width, self.surface.config.height);
        let shot = capture.map(|_| {
            dev.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("screenshot"),
                size: wgpu::Extent3d { width: cw, height: ch, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.surface.format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        });
        let shot_view = shot.as_ref().map(|t| t.create_view(&wgpu::TextureViewDescriptor::default()));
        for target in std::iter::once(&view).chain(shot_view.as_ref()) {
            let mut rp = enc
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("egui"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: target,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color { r: clear[0], g: clear[1], b: clear[2], a: clear[3] }), store: wgpu::StoreOp::Store },
                    })],
                    ..Default::default()
                })
                .forget_lifetime();
            self.egui_rend.render(&mut rp, &prims, &sd);
        }
        for id in &frees {
            self.egui_rend.free_texture(id);
        }
        let readback = shot.as_ref().map(|t| {
            let bpr = (cw * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
            let buf = dev.device.create_buffer(&wgpu::BufferDescriptor { label: Some("screenshot"), size: u64::from(bpr) * u64::from(ch), usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
            enc.copy_texture_to_buffer(
                t.as_image_copy(),
                wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bpr), rows_per_image: None } },
                wgpu::Extent3d { width: cw, height: ch, depth_or_array_layers: 1 },
            );
            (buf, bpr)
        });
        dev.queue.submit(extra.into_iter().chain([enc.finish()]));
        if let (Some((buf, bpr)), Some(path)) = (readback, capture) {
            buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
            let _ = dev.device.poll(wgpu::PollType::wait_indefinitely());
            if let Ok(data) = buf.slice(..).get_mapped_range() {
                let bgra = matches!(self.surface.format, wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb);
                let mut px = Vec::with_capacity((cw * ch * 4) as usize);
                for row in data.chunks_exact(bpr as usize) {
                    for p in row[..(cw * 4) as usize].as_chunks::<4>().0 {
                        if bgra { px.extend_from_slice(&[p[2], p[1], p[0], 255]) } else { px.extend_from_slice(&[p[0], p[1], p[2], 255]) }
                    }
                }
                if let Some(d) = path.parent() {
                    let _ = std::fs::create_dir_all(d);
                }
                if let Some(img) = image::RgbaImage::from_raw(cw, ch, px)
                    && let Err(e) = img.save(path)
                {
                    eprintln!("screenshot {}: {e}", path.display());
                }
            }
        }
        self.window.pre_present_notify();
        dev.queue.present(frame);
        self.egui_state.handle_platform_output(&self.window, out.platform_output);
        Ok(())
    }

    /// Blocks until the GPU has finished everything submitted (latency measurement).
    pub fn wait(&self) {
        let _ = self.vello.wait();
    }
}

/// System faces that cover what Inter lacks (Arabic, CJK, symbols) in layer names and fields.
/// Missing files are skipped.
#[cfg(windows)]
const UI_FALLBACKS: &[&str] = &["segoeui.ttf", "seguisym.ttf", "YuGothR.ttc"];
#[cfg(not(windows))]
const UI_FALLBACKS: &[&str] = &[];

/// egui's fonts: the bundled Inter (the canvas's own face; egui's default font data is a separate
/// crate we don't ship), then system fallbacks.
fn ui_fonts() -> egui::FontDefinitions {
    let mut defs = egui::FontDefinitions::empty();
    defs.font_data.insert("Inter".into(), std::sync::Arc::new(egui::FontData::from_static(text::INTER_TTF)));
    let mut names = vec!["Inter".to_string()];
    let dir = std::env::var_os("SystemRoot").map(|r| std::path::PathBuf::from(r).join("Fonts"));
    for f in UI_FALLBACKS {
        if let Some(bytes) = dir.as_ref().and_then(|d| std::fs::read(d.join(f)).ok()) {
            defs.font_data.insert((*f).into(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
            names.push((*f).into());
        }
    }
    defs.families.insert(egui::FontFamily::Proportional, names.clone());
    defs.families.insert(egui::FontFamily::Monospace, names);
    defs
}
