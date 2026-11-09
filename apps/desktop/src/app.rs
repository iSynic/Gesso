//! App state and the egui layout: menu bar and toolbar, layers panel (left), properties panel
//! (right), status bar, the canvas in the middle, and the modal dialogs. Files, recent files,
//! the clipboard and autosave live here too.

use std::path::{Path, PathBuf};
use std::time::Instant;

use editor::autosave::{self, Autosave, Recovery};
use editor::commands::{self, Command};
use editor::{Controller, Session, Tool, clipboard, place};

/// File types File ▸ Place and dropping accept.
const PLACEABLE: &[&str] = &["svg", "png", "jpg", "jpeg", "webp", "gif"];
use egui::{Align, Layout, RichText};
use text::FontDb;

use crate::canvas::{self, CanvasState, Request};
use crate::panels::{LayersUi, PropsUi};
use crate::variables::VariablesUi;

/// What to do once unsaved changes are dealt with.
#[derive(Debug, Clone)]
pub enum After {
    Quit,
    New,
    Open(Option<PathBuf>),
}

pub enum Modal {
    Unsaved(After),
    Recover(Vec<Recovery>),
    Message(String),
}

/// Tabs of the left panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LeftTab {
    #[default]
    Layers,
    Assets,
    Variables,
}

pub struct App {
    pub session: Session,
    /// Wakes the event loop when a background image decode finishes (every session's scene
    /// decodes in the background with it).
    image_waker: Option<render::Waker>,
    pub ctl: Controller,
    pub canvas: CanvasState,
    pub autosave: Option<Autosave>,
    pub fonts: FontDb,
    pub modal: Option<Modal>,
    pub recent: Vec<PathBuf>,
    /// The interface scale chosen in the View menu, on top of the display's own scale.
    pub ui_scale: f32,
    /// A scale for the shell to apply before the next frame.
    pub pending_scale: Option<f32>,
    /// Whether the user's settings file is read and written (scripted sessions leave it alone).
    settings: bool,
    /// The zoom popup's percentage field, and a script's request to open the popup.
    zoom_text: String,
    pub zoom_menu_request: bool,
    /// A menu asked to paste: the shell reads the clipboard into the next frame's input.
    pub paste_request: bool,
    pub status: String,
    pub layers: LayersUi,
    pub props: PropsUi,
    pub variables: VariablesUi,
    pub left_tab: LeftTab,
    /// When the input handled this frame arrived (latency measurement).
    pub on_input: Option<Instant>,
    /// Fit the page into the view once the canvas has a size.
    pub fit_pending: bool,
    /// The side panels' widths as the user left them (layers, properties). A narrow window
    /// shows them narrower for now; they come back when there's room again.
    panel_widths: [f32; 2],
    /// The side panels' rects last frame, to see a drag on their edges.
    panel_rects: [egui::Rect; 2],
    /// The panel whose edge is being dragged (0 layers, 1 properties).
    panel_drag: Option<usize>,
    pub quit: bool,
    /// Last egui pass, ms.
    pub egui_ms: f64,
    pub frame_ms: f64,
    /// Runs agent scripts (requests arriving over IPC).
    pub engine: ::script::Engine,
    /// The folder the app was started in; agents may open and save there and in the open
    /// document's folder.
    pub launch_dir: Option<PathBuf>,
    /// Agent requests answered so far, and when the last one that changed the document
    /// arrived (scripted sessions measure the frame that shows it).
    pub agent_count: u64,
    pub agent_edit: Option<Instant>,
}

/// The canvas keeps at least this width (points) when the window narrows; the side panels give
/// way first, down to their minimum widths.
const MIN_CANVAS: f32 = 280.0;
const PANEL_MIN: [f32; 2] = [160.0, 220.0];
const PANEL_MAX: [f32; 2] = [480.0, 520.0];

/// The side panels' widths for a window `total` points wide: as `want`, or both narrowed in
/// proportion to their slack so the canvas keeps [`MIN_CANVAS`].
fn fit_panels(want: [f32; 2], total: f32) -> [f32; 2] {
    let room = (total - MIN_CANVAS).max(PANEL_MIN[0] + PANEL_MIN[1]);
    let excess = want[0] + want[1] - room;
    if excess <= 0.0 {
        return want;
    }
    let slack = [want[0] - PANEL_MIN[0], want[1] - PANEL_MIN[1]];
    let k = (excess / (slack[0] + slack[1]).max(1e-3)).min(1.0);
    [want[0] - slack[0] * k, want[1] - slack[1] * k]
}

/// The interface scales on offer, multiplying the display's own scale.
const UI_SCALES: [(f32, &str); 7] = [(0.75, "75%"), (0.9, "90%"), (1.0, "100% (automatic)"), (1.1, "110%"), (1.25, "125%"), (1.5, "150%"), (2.0, "200%")];

fn recent_file() -> Option<PathBuf> {
    editor::dirs::config_dir().map(|d| d.join("ai-design-engine").join("recent.json"))
}

fn settings_file() -> Option<PathBuf> {
    editor::dirs::config_dir().map(|d| d.join("ai-design-engine").join("settings.json"))
}

fn load_ui_scale() -> Option<f32> {
    let v: serde_json::Value = settings_file().and_then(|f| std::fs::read(f).ok()).and_then(|b| serde_json::from_slice(&b).ok())?;
    v.get("uiScale").and_then(serde_json::Value::as_f64).map(|s| s as f32).filter(|s| UI_SCALES.iter().any(|(u, _)| u == s))
}

/// What a percentage typed into the zoom popup asks for ("150", "150%", "12.5 %").
fn parse_zoom(text: &str) -> Option<f64> {
    let t = text.trim();
    let n: f64 = t.strip_suffix('%').unwrap_or(t).trim().parse().ok()?;
    (n.is_finite() && n > 0.0).then_some(n / 100.0)
}

fn zoom_label(zoom: f64) -> String {
    let p = zoom * 100.0;
    if p < 10.0 { format!("{p:.1}%") } else { format!("{p:.0}%") }
}

fn load_recent() -> Vec<PathBuf> {
    recent_file().and_then(|f| std::fs::read(f).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn display_name(p: &Path) -> String {
    p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned())
}

/// What a modal asks the app to do once it closes.
type AfterModal = Box<dyn FnOnce(&mut App)>;

impl App {
    pub fn new(mut session: Session, fonts: FontDb, recover: bool, autosave: bool, settings: bool) -> Self {
        let modal = if recover {
            let found = autosave::default_dir().map(|d| autosave::find(&d)).unwrap_or_default();
            (!found.is_empty()).then_some(Modal::Recover(found))
        } else {
            None
        };
        let autosave = autosave.then(autosave::default_dir).flatten().map(|d| Autosave::new(d, &mut session));
        let saved_scale = settings.then(load_ui_scale).flatten();
        App {
            session,
            image_waker: None,
            ctl: Controller::default(),
            canvas: CanvasState::default(),
            autosave,
            fonts,
            modal,
            recent: load_recent(),
            ui_scale: saved_scale.unwrap_or(1.0),
            pending_scale: saved_scale,
            settings,
            zoom_text: String::new(),
            zoom_menu_request: false,
            paste_request: false,
            status: String::new(),
            layers: LayersUi::default(),
            props: PropsUi::default(),
            variables: VariablesUi::default(),
            left_tab: LeftTab::default(),
            on_input: None,
            fit_pending: true,
            panel_widths: [240.0, 280.0],
            panel_rects: [egui::Rect::NOTHING; 2],
            panel_drag: None,
            quit: false,
            egui_ms: 0.0,
            frame_ms: 0.0,
            engine: ::script::Engine::new(),
            launch_dir: std::env::current_dir().ok(),
            agent_count: 0,
            agent_edit: None,
        }
    }

    /// Sets the interface scale and remembers it.
    pub fn set_ui_scale(&mut self, scale: f32) {
        self.ui_scale = scale;
        self.pending_scale = Some(scale);
        if let Some(f) = settings_file().filter(|_| self.settings) {
            if let Some(d) = f.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let _ = std::fs::write(f, serde_json::to_vec_pretty(&serde_json::json!({ "uiScale": scale })).unwrap_or_default());
        }
    }

    pub fn title(&self) -> String {
        let name = self.session.path().map_or_else(|| "Untitled".to_string(), display_name);
        format!("{}{name} \u{2014} Gesso", if self.session.is_dirty() { "\u{2022} " } else { "" })
    }

    fn remember(&mut self, p: &Path) {
        let p = p.to_path_buf();
        self.recent.retain(|q| q != &p);
        self.recent.insert(0, p);
        self.recent.truncate(10);
        if let Some(f) = recent_file() {
            if let Some(d) = f.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let _ = std::fs::write(f, serde_json::to_vec_pretty(&self.recent).unwrap_or_default());
        }
    }

    /// Decodes images in the background, calling `wake` as each finishes.
    pub fn set_image_waker(&mut self, wake: render::Waker) {
        self.session.set_image_waker(Some(wake.clone()));
        self.image_waker = Some(wake);
    }

    fn replace_session(&mut self, s: Session) {
        let autosave = self.autosave.is_some();
        if let Some(a) = &mut self.autosave {
            a.discard();
        }
        self.session = s;
        self.session.set_image_waker(self.image_waker.clone());
        self.ctl = Controller::default();
        self.layers = LayersUi::default();
        self.props = PropsUi::default();
        self.variables = VariablesUi::default();
        self.autosave = autosave.then(autosave::default_dir).flatten().map(|d| Autosave::new(d, &mut self.session));
        self.fit_pending = true;
    }

    pub fn open_path(&mut self, path: &Path) {
        match Session::open(path, self.fonts.clone()) {
            Ok(s) => {
                self.replace_session(s);
                self.remember(path);
                self.status = format!("Opened {}", display_name(path));
            }
            Err(e) => self.modal = Some(Modal::Message(format!("Couldn't open {}:\n{e}", path.display()))),
        }
    }

    /// Runs `after` now if there are no unsaved changes, otherwise asks first.
    fn guarded(&mut self, after: After) {
        if self.session.is_dirty() {
            self.modal = Some(Modal::Unsaved(after));
        } else {
            self.proceed(after);
        }
    }

    fn proceed(&mut self, after: After) {
        match after {
            After::Quit => {
                if let Some(a) = &mut self.autosave {
                    a.discard();
                }
                self.quit = true;
            }
            After::New => match Session::untitled(self.fonts.clone()) {
                Ok(s) => self.replace_session(s),
                Err(e) => self.modal = Some(Modal::Message(e.to_string())),
            },
            After::Open(Some(p)) => self.open_path(&p),
            After::Open(None) => {
                if let Some(p) = rfd::FileDialog::new().set_title("Open a .canvas document").pick_folder() {
                    self.open_path(&p);
                }
            }
        }
    }

    /// The folders agent file paths must stay in.
    fn agent_roots(&self) -> Vec<PathBuf> {
        let doc_dir = self.session.path().and_then(Path::parent).map(Path::to_path_buf);
        self.launch_dir.iter().cloned().chain(doc_dir).collect()
    }

    /// Answers an agent request against the open document. Edits are ordinary transactions
    /// (origin `Agent`), so undo, autosave and the panels see them like any other edit.
    pub fn agent(&mut self, req: agent::Request) -> agent::Response {
        use agent::{Request as R, Response};
        let t = Instant::now();
        self.agent_count += 1;
        let opts = ::script::Options { roots: self.agent_roots(), ..Default::default() };
        match req {
            R::Open(_) | R::New(_) if self.session.is_dirty() => {
                Response::err("the open document has unsaved changes; save it first (save), then open or start a new one")
            }
            R::Open(p) => {
                let path = match ::script::resolve_path(&opts.roots, &p.path).and_then(|p| p.canonicalize().map_err(|e| format!("{}: {e}", p.display()))) {
                    Ok(p) => p,
                    Err(e) => return Response::err(e),
                };
                match Session::open(&path, self.fonts.clone()) {
                    Ok(s) => {
                        self.replace_session(s);
                        self.remember(&path);
                        self.status = format!("Agent opened {}", display_name(&path));
                        Response::ok(agent::state(&self.session, "app"))
                    }
                    Err(e) => Response::err(e.to_string()),
                }
            }
            R::New(_) => match Session::untitled(self.fonts.clone()) {
                Ok(s) => {
                    self.replace_session(s);
                    self.status = "Agent started a new document".into();
                    Response::ok(agent::state(&self.session, "app"))
                }
                Err(e) => Response::err(e.to_string()),
            },
            req => {
                let name = req.name();
                let r = agent::handle_with(&mut self.session, &mut self.engine, &opts, "app", req);
                let Some(res) = r.result.as_ref().filter(|_| r.is_ok()) else { return r };
                match name {
                    "execute" => {
                        let c = &res["changes"];
                        let n: u64 = ["inserted", "updated", "moved", "deleted"].iter().filter_map(|k| c[*k].as_u64()).sum();
                        if n > 0 {
                            self.agent_edit = Some(t);
                            let label = res["undoLabel"].as_str().unwrap_or("Script");
                            self.status = format!("Agent: {label} ({n} change{})", if n == 1 { "" } else { "s" });
                        }
                    }
                    "save" => {
                        if let Some(a) = &mut self.autosave {
                            a.saved(&mut self.session);
                        }
                        if let Some(p) = self.session.path().map(Path::to_path_buf) {
                            self.remember(&p);
                        }
                        self.status = "Agent saved".into();
                    }
                    "undo" | "redo" => {
                        let label = res.as_object().and_then(|o| o.values().next()).and_then(|v| v.as_str()).unwrap_or("");
                        self.status = format!("Agent {} {label}", if name == "undo" { "undid" } else { "redid" });
                    }
                    _ => {}
                }
                r
            }
        }
    }

    pub fn request_quit(&mut self) {
        self.guarded(After::Quit);
    }

    /// The selection, or everything on the page when nothing is selected, for exports.
    fn export_ids(&self) -> Vec<doc::NodeId> {
        let ids = self.session.selection().to_vec();
        if !ids.is_empty() {
            return ids;
        }
        let doc = self.session.doc();
        let page = self.session.page();
        doc.key(page).and_then(|k| doc.node(k)).map(|p| p.children.iter().filter_map(|c| doc.node(*c)).map(|n| n.id).collect()).unwrap_or_default()
    }

    fn export_content(&mut self, ids: &[doc::NodeId]) -> Result<::export::Content, String> {
        let (scene, doc) = self.session.scene_and_doc();
        let assets = scene.assets();
        ::export::collect_with(doc, ids, &mut scene.text, &mut scene.images, &*assets).map_err(|e| e.to_string())
    }

    /// Exports the selection (or everything on the page) with the properties panel's export
    /// settings, asking where. Returns whether it wrote a file.
    fn export(&mut self) -> bool {
        let settings = self.props.export.clone();
        let format = settings.format;
        let named = self.session.selection().first().and_then(|id| self.session.doc().get(*id)).map(|n| n.name.clone()).filter(|n| !n.is_empty());
        let ids = self.export_ids();
        if ids.is_empty() {
            self.status = "Nothing to export".into();
            return false;
        }
        let ext = format.extension();
        let file = format!("{}.{ext}", named.unwrap_or_else(|| "Export".into()).replace(['/', '\\', ':'], "-"));
        let Some(mut path) = rfd::FileDialog::new().set_title("Export").add_filter(ext.to_uppercase(), &[ext]).set_file_name(file).save_file() else { return false };
        if path.extension().is_none() {
            path.set_extension(ext);
        }
        let t = Instant::now();
        let r = self
            .export_content(&ids)
            .and_then(|c| ::export::write(&c, format, ::export::Options { scale: settings.scale, quality: settings.quality }).map_err(|e| e.to_string()))
            .and_then(|bytes| std::fs::write(&path, &bytes).map(|()| bytes.len()).map_err(|e| e.to_string()));
        match r {
            Ok(n) => {
                self.status = format!("Exported {} ({} KB, {:.0} ms)", display_name(&path), n.div_ceil(1024), t.elapsed().as_secs_f64() * 1e3);
                true
            }
            Err(e) => {
                self.modal = Some(Modal::Message(format!("Couldn't export:\n{e}")));
                false
            }
        }
    }

    /// Copies the selection as a PNG bitmap (at 2×) or as SVG text.
    fn copy_as(&mut self, ctx: &egui::Context, format: ::export::Format) {
        let ids = self.session.selection().to_vec();
        if ids.is_empty() {
            self.status = "Select something to copy".into();
            return;
        }
        let r = self.export_content(&ids).and_then(|c| match format {
            ::export::Format::Svg => ::export::svg::write(&c, 1.0).map(|svg| ctx.copy_text(svg)).map_err(|e| e.to_string()),
            _ => ::export::raster::render(&c, 2.0)
                .map(|px| {
                    let size = [usize::from(px.width()), usize::from(px.height())];
                    ctx.copy_image(egui::ColorImage::from_rgba_unmultiplied(size, &::export::raster::rgba(&px)));
                })
                .map_err(|e| e.to_string()),
        });
        self.status = match r {
            Ok(()) => format!("Copied as {}", if format == ::export::Format::Svg { "SVG" } else { "PNG" }),
            Err(e) => format!("Couldn't copy: {e}"),
        };
    }

    /// Asks for an image or SVG file and places it in the view.
    fn place_dialog(&mut self) -> bool {
        let Some(path) = rfd::FileDialog::new().set_title("Place image or SVG").add_filter("Images and SVG", PLACEABLE).pick_file() else { return false };
        self.place_file(&path, None)
    }

    /// Places an image or SVG file, centred on `at` (world) or in the view.
    fn place_file(&mut self, path: &std::path::Path, at: Option<kurbo::Point>) -> bool {
        let name = path.file_stem().map_or_else(|| "Image".into(), |s| s.to_string_lossy().into_owned());
        let svg = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("svg"));
        let r = match std::fs::read(path) {
            Err(e) => Err(e.to_string()),
            Ok(bytes) if svg => match String::from_utf8(bytes) {
                Ok(text) => place::svg(&mut self.session, &text, &name, at).map(|(_, w)| w).map_err(|e| e.to_string()),
                Err(_) => Err("the SVG file isn't UTF-8 text".into()),
            },
            Ok(bytes) => place::image(&mut self.session, bytes, &name, at).map(|_| Vec::new()).map_err(|e| e.to_string()),
        };
        self.placed(&name, r)
    }

    /// Reports a placement in the status bar; returns whether it worked.
    fn placed(&mut self, name: &str, r: Result<Vec<String>, String>) -> bool {
        match r {
            Ok(w) if w.is_empty() => {
                self.status = format!("Placed {name}");
                true
            }
            Ok(w) => {
                self.status = format!("Placed {name}; not exact: {}", w.join("; "));
                true
            }
            Err(e) => {
                self.status = format!("Couldn't place {name}: {e}");
                false
            }
        }
    }

    /// Places a clipboard bitmap.
    fn paste_image(&mut self, img: &egui::ColorImage) {
        let [w, h] = img.size;
        let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_srgba_unmultiplied()).collect();
        let mut png = Vec::new();
        let r = image::RgbaImage::from_raw(w as u32, h as u32, rgba)
            .ok_or_else(|| "bad bitmap".to_string())
            .and_then(|im| im.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).map_err(|e| e.to_string()))
            .and_then(|()| place::image(&mut self.session, png, "Pasted image", None).map(|_| Vec::new()).map_err(|e| e.to_string()));
        self.placed("pasted image", r);
    }

    /// Saves; asks for a location when the document has none. Returns whether it saved.
    fn save(&mut self, ask: bool) -> bool {
        let r = if ask || self.session.path().is_none() {
            let Some(mut p) = rfd::FileDialog::new().set_title("Save as").set_file_name("Untitled.canvas").save_file() else { return false };
            if p.extension().is_none_or(|e| e != "canvas") {
                p.set_extension("canvas");
            }
            let r = self.session.save_as(&p);
            if r.is_ok() {
                self.remember(&p);
            }
            r
        } else {
            self.session.save()
        };
        match r {
            Ok(()) => {
                if let Some(a) = &mut self.autosave {
                    a.saved(&mut self.session);
                }
                self.status = "Saved".into();
                true
            }
            Err(e) => {
                self.modal = Some(Modal::Message(format!("Couldn't save:\n{e}")));
                false
            }
        }
    }

    pub fn run(&mut self, cmd: Command) {
        // Commands act on committed text: an input-method composition is dropped first.
        if let Some(t) = &mut self.ctl.text {
            t.end_composition(&mut self.session);
        }
        let r = match cmd {
            Command::Tool(t) => {
                self.ctl.set_tool(t);
                Ok(true)
            }
            Command::New => {
                self.guarded(After::New);
                Ok(true)
            }
            Command::Open => {
                self.guarded(After::Open(None));
                Ok(true)
            }
            Command::Save => Ok(self.save(false)),
            Command::SaveAs => Ok(self.save(true)),
            Command::Export => Ok(self.export()),
            Command::Place => Ok(self.place_dialog()),
            Command::Copy | Command::Cut | Command::Paste | Command::CopyAsPng | Command::CopyAsSvg => Ok(false),
            Command::Undo | Command::Redo => {
                let label = if cmd == Command::Undo { self.session.store().undo_label() } else { self.session.store().redo_label() }.map(str::to_string);
                let r = commands::execute(&mut self.session, cmd);
                if let Some(l) = label {
                    self.status = format!("{} {l}", if cmd == Command::Undo { "Undid" } else { "Redid" });
                }
                r
            }
            c => commands::execute(&mut self.session, c),
        };
        if let Err(e) = r {
            self.status = format!("Couldn't do that: {e}");
        }
    }

    fn copy(&mut self, ctx: &egui::Context, cut: bool) {
        if let Some(text) = clipboard::copy(&self.session) {
            ctx.copy_text(text);
            if cut {
                self.run(Command::Delete);
            }
        }
    }

    fn paste(&mut self, text: &str) {
        match clipboard::paste(&mut self.session, text) {
            Ok(Some(ids)) => self.status = format!("Pasted {} layer{}", ids.len(), if ids.len() == 1 { "" } else { "s" }),
            Ok(None) if ::export::import::is_svg(text) => {
                let r = place::svg(&mut self.session, text, "Pasted SVG", None).map(|(_, w)| w).map_err(|e| e.to_string());
                self.placed("SVG", r);
            }
            Ok(None) => self.status = "Nothing to paste: the clipboard has text, not layers, SVG or an image".into(),
            Err(e) => self.status = format!("Couldn't paste: {e}"),
        }
    }

    fn restore(&mut self, r: &Recovery) {
        let bundle = r.meta.source.as_ref().and_then(|p| format::Bundle::open(p).ok()).map(|o| o.bundle);
        match autosave::load(r).and_then(|doc| Session::new(doc, bundle, self.fonts.clone())) {
            Ok(mut s) => {
                s.mark_dirty();
                self.replace_session(s);
                if let Some(a) = &mut self.autosave {
                    a.snapshot(&mut self.session);
                    a.flush();
                }
                autosave::remove(r);
                self.status = "Recovered unsaved work".into();
            }
            Err(e) => self.modal = Some(Modal::Message(format!("Couldn't recover:\n{e}"))),
        }
    }

    // ---- UI -----------------------------------------------------------------------------------

    /// Builds this frame's UI and handles input.
    pub fn ui(&mut self, root: &mut egui::Ui, tex: egui::TextureId) {
        let t = Instant::now();
        let mut cmds: Vec<Command> = Vec::new();
        self.menu_bar(root, &mut cmds);
        egui::Panel::bottom("status").show(root, |ui| self.status_bar(ui));
        // The side panels hold the widths the user gave them. While an edge is dragged egui
        // sizes the panel; otherwise it gets its width, narrowed if the window is tight.
        let total = root.available_rect_before_wrap().width();
        let (down, origin) = root.ctx().input(|i| (i.pointer.primary_down(), i.pointer.press_origin()));
        if !down {
            self.panel_drag = None;
        } else if self.panel_drag.is_none()
            && let Some(o) = origin
            && root.ctx().input(|i| i.pointer.primary_pressed())
        {
            let [l, r] = self.panel_rects;
            self.panel_drag = if (o.x - l.right()).abs() <= 6.0 && l.y_range().contains(o.y) {
                Some(0)
            } else if (o.x - r.left()).abs() <= 6.0 && r.y_range().contains(o.y) {
                Some(1)
            } else {
                None
            };
        }
        let fitted = fit_panels(self.panel_widths, total);
        let panel = |p: egui::Panel, i: usize, drag: Option<usize>| {
            if drag == Some(i) {
                let other = fitted[1 - i];
                p.size_range(PANEL_MIN[i]..=PANEL_MAX[i].min(total - other - MIN_CANVAS).max(PANEL_MIN[i]))
            } else {
                p.exact_size(fitted[i])
            }
        };
        let drag = self.panel_drag;
        let left = panel(egui::Panel::left("layers").resizable(true), 0, drag).show(root, |ui| {
            ui.horizontal(|ui| {
                for (t, label) in [(LeftTab::Layers, "Layers"), (LeftTab::Assets, "Assets"), (LeftTab::Variables, "Variables")] {
                    ui.selectable_value(&mut self.left_tab, t, label);
                }
            });
            ui.separator();
            match self.left_tab {
                LeftTab::Layers => self.layers.show(ui, &mut self.session, &mut cmds),
                LeftTab::Assets => crate::variables::assets(ui, &mut self.session),
                LeftTab::Variables => self.variables.show(ui, &mut self.session),
            }
        });
        let right = panel(egui::Panel::right("properties").resizable(true), 1, drag).show(root, |ui| {
            self.props.show(ui, &mut self.session, &mut self.ctl, &mut self.on_input);
            if std::mem::take(&mut self.props.export.requested) {
                cmds.push(Command::Export);
            }
        });
        self.panel_rects = [left.response.rect, right.response.rect];
        if let Some(i) = drag {
            self.panel_widths[i] = self.panel_rects[i].width();
        }
        egui::CentralPanel::no_frame().show(root, |ui| {
            let rect = ui.available_rect_before_wrap();
            self.canvas.rect = rect;
            self.session.viewport = kurbo::Vec2::new(f64::from(rect.width()), f64::from(rect.height()));
            if self.fit_pending && rect.width() > 1.0 {
                self.session.zoom_to_fit();
                self.fit_pending = false;
            }
            let reqs = canvas::input(ui, &mut self.canvas, &mut self.session, &mut self.ctl, &mut self.on_input);
            for r in reqs {
                match r {
                    Request::Command(c) => cmds.push(c),
                    Request::Copy => self.copy(ui.ctx(), false),
                    Request::Cut => self.copy(ui.ctx(), true),
                    Request::Paste(t) => self.paste(&t),
                    Request::PasteImage(img) => self.paste_image(&img),
                    Request::PasteFromClipboard => self.paste_request = true,
                }
            }
            canvas::paint(ui, &mut self.canvas, &mut self.session, &self.ctl, tex);
            canvas::scrollbars(ui, &mut self.canvas, &mut self.session);
        });
        self.zoom_control(root.ctx(), &mut cmds);
        let (dropped, pointer) = root.ctx().input(|i| (i.raw.dropped_files.clone(), i.pointer.hover_pos()));
        for f in dropped {
            let min = self.canvas.rect.min;
            let at = pointer.map(|p| self.session.camera.to_world(kurbo::Point::new(f64::from(p.x - min.x), f64::from(p.y - min.y))));
            self.place_file(f.path(), at);
        }
        for c in cmds {
            match c {
                Command::CopyAsPng => self.copy_as(root.ctx(), ::export::Format::Png),
                Command::CopyAsSvg => self.copy_as(root.ctx(), ::export::Format::Svg),
                Command::Copy => self.copy(root.ctx(), false),
                Command::Cut => self.copy(root.ctx(), true),
                c => self.run(c),
            }
        }
        self.modals(root.ctx());
        self.egui_ms = t.elapsed().as_secs_f64() * 1e3;
    }

    fn menu_bar(&mut self, root: &mut egui::Ui, cmds: &mut Vec<Command>) {
        egui::Panel::top("toolbar").show(root, |ui| {
            ui.horizontal(|ui| {
                egui::MenuBar::new().ui(ui, |ui| {
                    ui.menu_button("File", |ui| {
                        let mut item = |ui: &mut egui::Ui, label: &str, key: &str, c: Command| {
                            if ui.add(egui::Button::new(label).shortcut_text(key)).clicked() {
                                cmds.push(c);
                            }
                        };
                        item(ui, "New", "Ctrl+N", Command::New);
                        item(ui, "Open\u{2026}", "Ctrl+O", Command::Open);
                        ui.menu_button("Open recent", |ui| {
                            if self.recent.is_empty() {
                                ui.label("No recent documents");
                            }
                            for p in self.recent.clone() {
                                if ui.button(display_name(&p)).on_hover_text(p.display().to_string()).clicked() {
                                    self.guarded(After::Open(Some(p)));
                                }
                            }
                        });
                        ui.separator();
                        item(ui, "Save", "Ctrl+S", Command::Save);
                        item(ui, "Save as\u{2026}", "Ctrl+Shift+S", Command::SaveAs);
                        ui.separator();
                        item(ui, "Place image or SVG\u{2026}", "Ctrl+Shift+K", Command::Place);
                        item(ui, "Export selection\u{2026}", "Ctrl+Shift+E", Command::Export);
                        ui.separator();
                        if ui.button("Quit").clicked() {
                            self.request_quit();
                        }
                    });
                    ui.menu_button("Edit", |ui| {
                        let undo = self.session.store().undo_label().map_or_else(|| "Undo".to_string(), |l| format!("Undo {l}"));
                        let redo = self.session.store().redo_label().map_or_else(|| "Redo".to_string(), |l| format!("Redo {l}"));
                        let can_undo = self.session.store().can_undo();
                        let can_redo = self.session.store().can_redo();
                        if ui.add_enabled(can_undo, egui::Button::new(undo).shortcut_text("Ctrl+Z")).clicked() {
                            cmds.push(Command::Undo);
                        }
                        if ui.add_enabled(can_redo, egui::Button::new(redo).shortcut_text("Ctrl+Shift+Z")).clicked() {
                            cmds.push(Command::Redo);
                        }
                        ui.separator();
                        let has = !self.session.selection().is_empty();
                        for (label, key, c, on) in [
                            ("Cut", "Ctrl+X", Command::Cut, has),
                            ("Copy", "Ctrl+C", Command::Copy, has),
                            ("Duplicate", "Ctrl+D", Command::Duplicate, has),
                            ("Delete", "Del", Command::Delete, has),
                            ("Select all", "Ctrl+A", Command::SelectAll, true),
                        ] {
                            if ui.add_enabled(on, egui::Button::new(label).shortcut_text(key)).clicked() {
                                cmds.push(c);
                            }
                        }
                        for (label, key, c) in [("Copy as PNG", "Ctrl+Shift+C", Command::CopyAsPng), ("Copy as SVG", "", Command::CopyAsSvg)] {
                            if ui.add_enabled(has, egui::Button::new(label).shortcut_text(key)).clicked() {
                                cmds.push(c);
                            }
                        }
                        ui.label(RichText::new("Paste (layers, SVG or an image): Ctrl+V").weak());
                        ui.separator();
                        for (label, key, c) in [
                            ("Group", "Ctrl+G", Command::Group),
                            ("Ungroup", "Ctrl+Shift+G", Command::Ungroup),
                            ("Bring forward", "Ctrl+]", Command::BringForward),
                            ("Send backward", "Ctrl+[", Command::SendBackward),
                            ("Bring to front", "Ctrl+Shift+]", Command::BringToFront),
                            ("Send to back", "Ctrl+Shift+[", Command::SendToBack),
                        ] {
                            if ui.add_enabled(has, egui::Button::new(label).shortcut_text(key)).clicked() {
                                cmds.push(c);
                            }
                        }
                    });
                    ui.menu_button("Object", |ui| {
                        let doc = self.session.doc();
                        let sel = self.session.selection();
                        let has = !sel.is_empty();
                        let instance = sel.iter().any(|id| doc.get(*id).is_some_and(|n| matches!(n.kind, doc::NodeKind::Instance { .. })));
                        let inside = sel.iter().any(|id| doc.get(*id).is_some_and(|n| n.virt.is_some()));
                        for (label, key, c, on) in [
                            ("Create component", "Ctrl+Alt+K", Command::CreateComponent, has && !inside),
                            ("Detach instance", "Ctrl+Alt+B", Command::DetachInstance, instance && !inside),
                            ("Reset overrides", "", Command::ResetOverrides, instance || inside),
                            ("Go to main component", "", Command::GoToMainComponent, instance),
                        ] {
                            if ui.add_enabled(on, egui::Button::new(label).shortcut_text(key)).clicked() {
                                cmds.push(c);
                            }
                        }
                    });
                    ui.menu_button("View", |ui| {
                        for (label, key, c) in [
                            ("Zoom in", "Ctrl++", Command::ZoomIn),
                            ("Zoom out", "Ctrl+-", Command::ZoomOut),
                            ("Zoom to 100%", "Ctrl+0", Command::ZoomTo100),
                            ("Zoom to fit", "Shift+1", Command::ZoomToFit),
                            ("Zoom to selection", "Shift+2", Command::ZoomToSelection),
                        ] {
                            if ui.add(egui::Button::new(label).shortcut_text(key)).clicked() {
                                cmds.push(c);
                            }
                        }
                        ui.separator();
                        ui.menu_button("Interface scale", |ui| {
                            let display = ui.ctx().native_pixels_per_point().unwrap_or(1.0) * 100.0;
                            ui.label(RichText::new(format!("Follows your display ({display:.0}%)")).weak());
                            for (scale, label) in UI_SCALES {
                                if ui.radio(self.ui_scale == scale, label).clicked() {
                                    self.set_ui_scale(scale);
                                    ui.close();
                                }
                            }
                        });
                        ui.separator();
                        ui.checkbox(&mut self.session.snap_to_objects, "Snap to objects");
                        ui.checkbox(&mut self.session.snap_to_pixels, "Snap to pixel grid");
                    });
                });
                ui.separator();
                for t in Tool::ALL {
                    let on = self.ctl.tool == t;
                    let r = ui.selectable_label(on, t.label()).on_hover_text(format!("{} ({})", t.label(), t.key()));
                    if r.clicked() {
                        cmds.push(Command::Tool(t));
                    }
                }
            });
        });
    }

    /// The zoom buttons floating in the canvas's bottom-right corner.
    fn zoom_control(&mut self, ctx: &egui::Context, cmds: &mut Vec<Command>) {
        // Above the horizontal scrollbar and left of the vertical one.
        let corner = self.canvas.rect.right_bottom() - egui::vec2(18.0, 18.0);
        let zoom = self.session.zoom();
        egui::Area::new(egui::Id::NULL.with("zoom control")).order(egui::Order::Middle).pivot(egui::Align2::RIGHT_BOTTOM).fixed_pos(corner).show(ctx, |ui| {
            egui::Frame::popup(ui.style()).inner_margin(4.0).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    if ui.add(egui::Button::new("\u{2212}").frame(false).min_size(egui::vec2(24.0, 22.0))).on_hover_text("Zoom out (Ctrl+-)").clicked() {
                        cmds.push(Command::ZoomOut);
                    }
                    let level = ui.add(egui::Button::new(zoom_label(zoom)).frame(false).min_size(egui::vec2(52.0, 22.0))).on_hover_text("Zoom level");
                    if ui.add(egui::Button::new("+").frame(false).min_size(egui::vec2(24.0, 22.0))).on_hover_text("Zoom in (Ctrl++)").clicked() {
                        cmds.push(Command::ZoomIn);
                    }
                    let id = egui::Popup::default_response_id(&level);
                    if std::mem::take(&mut self.zoom_menu_request) {
                        egui::Popup::open_id(ctx, id);
                    }
                    if level.clicked() {
                        self.zoom_text = zoom_label(zoom);
                    }
                    egui::Popup::menu(&level).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
                        ui.set_width(180.0);
                        let field = ui.add(egui::TextEdit::singleline(&mut self.zoom_text).desired_width(180.0).hint_text("Zoom %"));
                        let mut close = false;
                        if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            if let Some(z) = parse_zoom(&self.zoom_text) {
                                cmds.push(Command::ZoomTo(z));
                            }
                            close = true;
                        }
                        ui.separator();
                        let has_selection = !self.session.selection().is_empty();
                        for (label, key, on, c) in [
                            ("Zoom to fit", "Shift+1", true, Command::ZoomToFit),
                            ("Zoom to selection", "Shift+2", has_selection, Command::ZoomToSelection),
                        ] {
                            if ui.add_enabled(on, egui::Button::new(label).shortcut_text(key)).clicked() {
                                cmds.push(c);
                                close = true;
                            }
                        }
                        ui.separator();
                        for (label, key, z) in [("50%", "", 0.5), ("100%", "Ctrl+0", 1.0), ("200%", "", 2.0)] {
                            if ui.add(egui::Button::new(label).shortcut_text(key)).clicked() {
                                cmds.push(Command::ZoomTo(z));
                                close = true;
                            }
                        }
                        if close {
                            egui::Popup::close_id(ctx, id);
                        }
                    });
                });
            });
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let desc = canvas::describe(&self.session);
            let r = ui.label(&desc);
            // Screen readers announce selection changes.
            ui.ctx().accesskit_node_builder(r.id, |n| n.set_live(egui::accesskit::Live::Polite));
            ui.separator();
            ui.label(&self.status);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(format!("{:.1} ms", self.frame_ms)).weak()).on_hover_text("Last frame: input to submitted (CPU)");
                if self.session.is_dirty() {
                    ui.label(RichText::new("Unsaved").weak());
                }
            });
        });
    }

    fn modals(&mut self, ctx: &egui::Context) {
        let Some(m) = self.modal.take() else { return };
        let mut keep = true;
        let mut next: Option<AfterModal> = None;
        match &m {
            Modal::Unsaved(after) => {
                egui::Modal::new(egui::Id::NULL.with("unsaved")).show(ctx, |ui| {
                    ui.heading("Save changes?");
                    ui.label("This document has changes that haven't been saved.");
                    ui.horizontal(|ui| {
                        if ui.button("Save").clicked() {
                            let after = after.clone();
                            next = Some(Box::new(move |app| {
                                if app.save(false) {
                                    app.proceed(after);
                                }
                            }));
                            keep = false;
                        }
                        if ui.button("Don't save").clicked() {
                            let after = after.clone();
                            next = Some(Box::new(move |app| app.proceed(after)));
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                });
            }
            Modal::Recover(list) => {
                egui::Modal::new(egui::Id::NULL.with("recover")).show(ctx, |ui| {
                    ui.heading("Recover unsaved work?");
                    ui.label("The app didn't close cleanly last time. These snapshots have work that wasn't saved:");
                    for r in list {
                        ui.horizontal(|ui| {
                            let name = r.meta.source.as_deref().map_or_else(|| "Untitled".to_string(), display_name);
                            ui.label(name);
                            if ui.button("Restore").clicked() {
                                let r = r.clone();
                                next = Some(Box::new(move |app| app.restore(&r)));
                                keep = false;
                            }
                            if ui.button("Discard").clicked() {
                                autosave::remove(r);
                                keep = false;
                            }
                        });
                    }
                    if ui.button("Later").clicked() {
                        keep = false;
                    }
                });
            }
            Modal::Message(text) => {
                egui::Modal::new(egui::Id::NULL.with("message")).show(ctx, |ui| {
                    ui.label(text);
                    if ui.button("OK").clicked() {
                        keep = false;
                    }
                });
            }
        }
        if keep && self.modal.is_none() {
            self.modal = Some(m);
        }
        if let Some(f) = next {
            f(self);
        }
    }
}
