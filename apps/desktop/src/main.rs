//! Gesso, the canvas app.
//!
//! ```text
//! gesso [PATH.canvas] [--novsync] [--no-recover]
//! gesso --script SCRIPT.json [--novsync] [--out RESULT.json] [--autosave]
//! either: [--agent-socket NAME] [--no-agent]
//! ```
//!
//! `--script` replays a scripted session (see `script.rs`) through the same controller as
//! real input and writes latency measurements.
//!
//! The app answers agent requests (from the MCP server, `canvas-mcp`) over a per-user local
//! socket. `--no-agent` turns that off; scripted sessions listen only with `--agent-socket`.

// Release builds on Windows open no console window; debug builds keep one for logs.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod a11y;
mod app;
mod canvas;
mod gfx;
mod panels;
mod script;
mod variables;

use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use editor::Session;
use text::FontDb;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowId};

use crate::app::App;
use crate::gfx::Gfx;

/// Encode budget per frame for re-building fragment display lists; anything over it draws the
/// previous zoom band and finishes on later frames.
const ENCODE_BUDGET: Duration = Duration::from_millis(4);

pub enum UserEvent {
    AccessKit(egui_winit::accesskit_winit::Event),
    /// A request from an agent over IPC, and where its answer goes.
    Agent(Box<agent::Request>, mpsc::Sender<agent::Response>),
    /// A background image decode finished: redraw.
    ImageDecoded,
}

impl From<egui_winit::accesskit_winit::Event> for UserEvent {
    fn from(e: egui_winit::accesskit_winit::Event) -> Self {
        UserEvent::AccessKit(e)
    }
}

struct Shell {
    app: App,
    gfx: Option<Gfx>,
    proxy: EventLoopProxy<UserEvent>,
    novsync: bool,
    script: Option<script::Runner>,
    title: String,
    t_main: Instant,
    first_frame: Option<f64>,
}

/// An application icon from an embedded PNG.
fn icon(png: &[u8]) -> Option<winit::window::Icon> {
    let image = image::load_from_memory(png).ok()?.into_rgba8();
    let (w, h) = image.dimensions();
    winit::window::Icon::from_rgba(image.into_raw(), w, h).ok()
}

fn arg_value(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

impl Shell {
    fn redraw(&mut self) {
        let Some(gfx) = self.gfx.as_mut() else { return };
        let t0 = Instant::now();
        if let Some(s) = &mut self.script {
            s.before_frame(&mut self.app);
        }
        if let Some(scale) = self.app.pending_scale.take() {
            gfx.egui.set_zoom_factor(scale);
            self.app.session.set_view_scale(f64::from(scale));
        }
        let raw = gfx.egui_state.take_egui_input(&gfx.window);
        let ctx = gfx.egui.clone();
        let tex = gfx.canvas_tex;
        let app = &mut self.app;
        let out = ctx.run_ui(raw, |ui| app.ui(ui, tex));
        let ppp = f64::from(out.pixels_per_point);
        let rect = app.canvas.rect;
        let (w, h) = (((f64::from(rect.width()) * ppp).round() as u32).max(1), ((f64::from(rect.height()) * ppp).round() as u32).max(1));
        let cam = kurbo::Affine::scale(ppp) * app.session.camera.affine();
        let tc = Instant::now();
        let frame = app.session.frame(cam, w, h, Some(ENCODE_BUDGET));
        let stale = app.session.scene.stats.stale > 0 || app.session.scene.images_ready();
        let tr = Instant::now();
        if let Err(e) = gfx.render_canvas(&frame) {
            eprintln!("render: {e}");
        }
        let canvas_ms = [tr.duration_since(tc).as_secs_f64() * 1e3, tr.elapsed().as_secs_f64() * 1e3];
        let repaint = out.viewport_output.values().any(|v| v.repaint_delay.is_zero());
        let capture = self.script.as_ref().and_then(|s| s.capture.clone());
        if let Err(e) = gfx.present(out, [0.11, 0.11, 0.12, 1.0], capture.as_deref()) {
            eprintln!("{e}");
            gfx.window.request_redraw();
        }
        app.frame_ms = t0.elapsed().as_secs_f64() * 1e3;
        if self.first_frame.is_none() {
            self.first_frame = Some(self.t_main.elapsed().as_secs_f64() * 1e3);
        }
        if let Some(s) = &mut self.script {
            s.after_frame(app, gfx, t0, canvas_ms);
        }
        app.on_input = None;
        let title = app.title();
        if title != self.title {
            gfx.window.set_title(&title);
            self.title = title;
        }
        if stale || repaint || app.ctl.busy() || self.script.is_some() {
            gfx.window.request_redraw();
        }
    }
}

impl ApplicationHandler<UserEvent> for Shell {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.gfx.is_some() {
            return;
        }
        let size = self.script.as_ref().and_then(|s| s.window).unwrap_or_else(|| {
            // Small or heavily scaled displays can have less room than the default size.
            let room = el.primary_monitor().map(|m| m.size().to_logical::<f64>(m.scale_factor()));
            room.map_or([1440.0, 900.0], |r| [(r.width * 0.9).min(1440.0), (r.height * 0.9).min(900.0)])
        });
        let attrs = Window::default_attributes()
            .with_title(self.app.title())
            .with_inner_size(winit::dpi::LogicalSize::new(size[0], size[1]))
            .with_min_inner_size(winit::dpi::LogicalSize::new(640.0, 400.0))
            .with_window_icon(icon(include_bytes!("../assets/icon/gesso-32.png")))
            .with_visible(false);
        #[cfg(windows)]
        let attrs = winit::platform::windows::WindowAttributesExtWindows::with_taskbar_icon(attrs, icon(include_bytes!("../assets/icon/gesso-256.png")));
        let window = match el.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("cannot create a window: {e}");
                el.exit();
                return;
            }
        };
        match Gfx::new(window.clone(), self.novsync) {
            Ok(mut g) => {
                g.egui_state.init_accesskit(el, &window, self.proxy.clone());
                if let Some(s) = &mut self.script {
                    s.adapter = g.adapter();
                }
                window.set_visible(true);
                window.request_redraw();
                self.gfx = Some(g);
            }
            Err(e) => {
                eprintln!("cannot start the GPU renderer: {e}");
                el.exit();
            }
        }
    }

    fn user_event(&mut self, _el: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Agent(req, reply) => {
                // Reads change nothing on screen; a redraw would only delay the next request.
                let read = matches!(*req, agent::Request::GetState(_) | agent::Request::Screenshot(_) | agent::Request::Validate(_) | agent::Request::ReadGuide(_));
                let _ = reply.send(self.app.agent(*req));
                if read {
                    return;
                }
            }
            UserEvent::ImageDecoded => {}
            UserEvent::AccessKit(e) => {
                let Some(gfx) = self.gfx.as_mut() else { return };
                use egui_winit::accesskit_winit::WindowEvent as A;
                match e.window_event {
                    A::InitialTreeRequested => gfx.egui.enable_accesskit(),
                    A::ActionRequested(req) => gfx.egui_state.on_accesskit_action_request(req),
                    A::AccessibilityDeactivated => gfx.egui.disable_accesskit(),
                }
            }
        }
        if let Some(gfx) = &self.gfx {
            gfx.window.request_redraw();
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(gfx) = self.gfx.as_mut() else { return };
        let resp = gfx.egui_state.on_window_event(&gfx.window, &event);
        match event {
            WindowEvent::CloseRequested => {
                self.app.request_quit();
                gfx.window.request_redraw();
            }
            WindowEvent::Resized(size) => {
                gfx.resize(size.width, size.height);
                gfx.window.request_redraw();
            }
            WindowEvent::RedrawRequested => self.redraw(),
            _ => {
                if resp.repaint {
                    self.app.on_input.get_or_insert_with(Instant::now);
                    gfx.window.request_redraw();
                }
            }
        }
        if self.app.quit {
            if let Some(a) = &mut self.app.autosave {
                a.flush();
            }
            el.exit();
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        let now = Instant::now();
        let mut wake = None;
        if let Some(a) = &mut self.app.autosave {
            a.tick(&mut self.app.session, now);
            if self.app.session.is_dirty() {
                wake = Some(now + Duration::from_millis(500));
            }
        }
        if self.app.quit {
            el.exit();
        }
        el.set_control_flow(wake.map_or(ControlFlow::Wait, ControlFlow::WaitUntil));
    }
}

fn main() {
    let t_main = Instant::now();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let fonts = FontDb::new(true);
    let script = arg_value(&args, "--script").map(|p| match script::Runner::load(&p, arg_value(&args, "--out")) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("script {p}: {e}");
            std::process::exit(2);
        }
    });
    let values: Vec<String> = ["--script", "--out", "--agent-socket"].iter().filter_map(|f| arg_value(&args, f)).collect();
    let path = script.as_ref().and_then(|s| s.document.clone()).or_else(|| args.iter().find(|a| !a.starts_with("--") && !values.contains(a)).cloned());
    let t_open = Instant::now();
    let session = match &path {
        Some(p) => Session::open(p, fonts.clone()),
        None => Session::untitled(fonts.clone()),
    };
    let session = match session {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot open {}: {e}", path.unwrap_or_default());
            std::process::exit(1);
        }
    };
    let open_ms = t_open.elapsed().as_secs_f64() * 1e3;
    let recover = script.is_none() && !args.iter().any(|a| a == "--no-recover");
    // Scripted sessions neither recover nor autosave, unless `--autosave` (the crash test).
    let app = App::new(session, fonts, recover, script.is_none() || args.iter().any(|a| a == "--autosave"), script.is_none());
    let event_loop = match EventLoop::<UserEvent>::with_user_event().build() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("cannot start the event loop: {e}");
            std::process::exit(1);
        }
    };
    let agent_socket = arg_value(&args, "--agent-socket");
    if !args.iter().any(|a| a == "--no-agent") && (script.is_none() || agent_socket.is_some()) {
        listen(agent_socket.as_deref(), event_loop.create_proxy());
    }
    let mut script = script;
    if let Some(s) = &mut script {
        s.open_ms = open_ms;
    }
    let mut app = app;
    let wake = Mutex::new(event_loop.create_proxy());
    app.set_image_waker(Arc::new(move || {
        if let Ok(p) = wake.lock() {
            let _ = p.send_event(UserEvent::ImageDecoded);
        }
    }));
    let mut shell = Shell {
        app,
        gfx: None,
        proxy: event_loop.create_proxy(),
        novsync: args.iter().any(|a| a == "--novsync") || script.is_some(),
        script,
        title: String::new(),
        t_main,
        first_frame: None,
    };
    if let Err(e) = event_loop.run_app(&mut shell) {
        eprintln!("event loop: {e}");
    }
    if let (Some(s), Some(ff)) = (&shell.script, shell.first_frame) {
        s.report_first_frame(ff);
    }
}

/// Serves agent requests on the local socket: each one is handed to the event loop, which
/// answers it between frames on the UI thread.
fn listen(name: Option<&str>, proxy: EventLoopProxy<UserEvent>) {
    let proxy = Mutex::new(proxy);
    let handler: agent::ipc::Handler = Arc::new(move |req| {
        let (tx, rx) = mpsc::channel();
        let sent = proxy.lock().is_ok_and(|p| p.send_event(UserEvent::Agent(Box::new(req), tx)).is_ok());
        if !sent {
            return agent::Response::err("the app is closing");
        }
        rx.recv_timeout(Duration::from_secs(120)).unwrap_or_else(|_| agent::Response::err("the app didn't answer in time"))
    });
    let info = serde_json::json!({ "app": "desktop", "version": env!("CARGO_PKG_VERSION"), "pid": std::process::id() });
    if let Err(e) = agent::ipc::serve(name, info, handler) {
        eprintln!("agent connection off (is another window serving agents?): {e}");
    }
}
