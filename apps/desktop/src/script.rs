//! Scripted sessions: a JSON list of steps replayed one frame at a time through the editor's
//! controller (the same code real pointer input reaches), with input → GPU-done latency
//! recorded per frame for steps that name a `measure`. Used for the Stage 5 latency numbers,
//! smoke tests and screenshots.
//!
//! ```json
//! { "document": "docs.canvas", "window": [1600, 1000], "out": "result.json",
//!   "steps": [ {"fit": true}, {"zoom": 1.0, "center": [400, 300]}, {"select": "Card 3"},
//!              {"drag": {"node": "Card 3", "by": [200, 40], "frames": 120}, "measure": "drag"},
//!              {"drag": {"handle": "se", "by": [80, 40], "frames": 60, "shot": [30, "mid.png"]}, "measure": "resize"},
//!              {"panel": {"path": "width", "values": [200, 201, 202]}, "measure": "panel_number"},
//!              {"fill": ["#ff0000", "#00ff00"], "measure": "panel_colour"},
//!              {"zoomsweep": {"from": 0.03, "to": 2.0, "frames": 120}, "measure": "zoom_sweep"},
//!              {"layers": "expand"}, {"wait": 30, "measure": "layers_panel"},
//!              {"command": "undo"}, {"undoall": true}, {"screenshot": "a.png"}, {"save": "b.canvas"},
//!              {"instance": "Card 3", "at": [900, 200]}, {"enter": true},
//!              {"agent": 12, "measure": "agent_edit"},
//!              {"edit": "Title"}, {"type": "Hello", "measure": "keystroke"}, {"key": "backspace", "mods": "ctrl"},
//!              {"ime": {"preedit": "にほ"}}, {"ime": {"commit": "日本"}},
//!              {"pen": [[0, 0], [100, 0, 20, 30], [100, 100]], "close": true},
//!              {"uiscale": 1.5}, {"zoommenu": true} ] }
//! ```
//!
//! `uiscale` sets the interface scale (not saved to the user's settings); `zoommenu` opens the
//! zoom control's popup.
//!
//! `edit` starts text editing (all selected) or path edit mode on the named node; `type` types
//! one character per frame; `key` presses an editing key or shortcut (`left`, `home`,
//! `backspace`, `enter`, `escape`, `tab`, or a letter, with `mods`); `ime` sends a composition
//! update or commit; `pen` draws with the pen tool through page points, `[x, y, dx, dy]` being
//! a press dragged by `(dx, dy)`, then closes the path or finishes it with Enter.
//!
//! `instance` places an instance of the named component (centred at `at`, page coordinates);
//! `enter` double-clicks the middle of the selection (entering a group or instance).
//!
//! `agent` idles until that many agent requests (over IPC; needs `--agent-socket`) have been
//! answered.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use doc::NodeId;
use editor::commands::{self, Command, Key, KeyMods};
use editor::inspect::{self, Applies};
use editor::transform::{self, Handle, Mods};
use editor::{Button, Camera, Pointer};
use kurbo::{Point, Vec2};
use serde_json::{Value, json};

use crate::app::App;
use crate::gfx::Gfx;

#[derive(Default)]
struct Series {
    gpu_done: Vec<f64>,
    egui: Vec<f64>,
    scene: Vec<f64>,
    render: Vec<f64>,
    built: Vec<f64>,
    prefetch: Vec<f64>,
    /// Vello: display lists into scenes, the render call, and the frame's glyph count.
    execute: Vec<f64>,
    submit: Vec<f64>,
    glyphs: Vec<f64>,
    paths: Vec<f64>,
    stale: usize,
}

pub struct Runner {
    pub document: Option<String>,
    pub window: Option<[f64; 2]>,
    pub adapter: String,
    pub open_ms: f64,
    out: Option<PathBuf>,
    /// The script's folder; relative paths in steps resolve against it.
    base: PathBuf,
    steps: Vec<Value>,
    at: usize,
    /// Frame within the current step.
    frame: usize,
    measure: Option<String>,
    t_input: Option<Instant>,
    results: BTreeMap<String, Series>,
    drag_from: Option<Point>,
    pub capture: Option<PathBuf>,
    log: Vec<String>,
    pub done: bool,
}

fn stats(mut v: Vec<f64>) -> Value {
    if v.is_empty() {
        return json!({"n": 0});
    }
    v.sort_by(f64::total_cmp);
    let q = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
    json!({"n": v.len(), "p50": q(0.5), "p95": q(0.95), "max": v[v.len() - 1], "mean": v.iter().sum::<f64>() / v.len() as f64})
}

fn mods_of(v: Option<&Value>) -> Mods {
    let s = v.and_then(Value::as_str).unwrap_or("");
    Mods { shift: s.contains("shift"), alt: s.contains("alt"), ctrl: s.contains("ctrl") }
}

fn key_of(s: &str) -> Option<Key> {
    Some(match s {
        "left" => Key::Left,
        "right" => Key::Right,
        "up" => Key::Up,
        "down" => Key::Down,
        "home" => Key::Home,
        "end" => Key::End,
        "backspace" => Key::Backspace,
        "delete" => Key::Delete,
        "enter" => Key::Enter,
        "escape" => Key::Escape,
        "tab" => Key::Tab,
        _ => {
            let mut c = s.chars();
            match (c.next(), c.next()) {
                (Some(ch), None) => Key::Char(ch.to_ascii_lowercase()),
                _ => return None,
            }
        }
    })
}

fn handle_of(s: &str) -> Option<Handle> {
    Some(match s {
        "n" => Handle::N,
        "ne" => Handle::NE,
        "e" => Handle::E,
        "se" => Handle::SE,
        "s" => Handle::S,
        "sw" => Handle::SW,
        "w" => Handle::W,
        "nw" => Handle::NW,
        _ => return None,
    })
}

fn find(app: &App, name: &str) -> Option<NodeId> {
    let doc = app.session.doc();
    let page = app.session.page();
    let pk = doc.key(page)?;
    doc.descendants(pk).into_iter().filter_map(|k| doc.node(k)).find(|n| n.name == name).map(|n| n.id)
}

fn command_of(s: &str) -> Option<Command> {
    Some(match s {
        "undo" => Command::Undo,
        "redo" => Command::Redo,
        "delete" => Command::Delete,
        "duplicate" => Command::Duplicate,
        "group" => Command::Group,
        "ungroup" => Command::Ungroup,
        "selectall" => Command::SelectAll,
        "fit" => Command::ZoomToFit,
        "zoomselection" => Command::ZoomToSelection,
        "escape" => Command::Escape,
        "createcomponent" => Command::CreateComponent,
        "detach" => Command::DetachInstance,
        "resetoverrides" => Command::ResetOverrides,
        "gotomain" => Command::GoToMainComponent,
        _ => return None,
    })
}

impl Runner {
    pub fn load(path: &str, out: Option<String>) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        let v: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        let base = PathBuf::from(path).parent().map(PathBuf::from).unwrap_or_default();
        let rel = |s: &str| {
            let p = PathBuf::from(s);
            if p.is_absolute() { p } else { base.join(p) }
        };
        Ok(Runner {
            document: v.get("document").and_then(Value::as_str).map(|s| rel(s).display().to_string()),
            window: v.get("window").and_then(|w| Some([w.get(0)?.as_f64()?, w.get(1)?.as_f64()?])),
            adapter: String::new(),
            open_ms: 0.0,
            out: out.map(PathBuf::from).or_else(|| v.get("out").and_then(Value::as_str).map(rel)),
            steps: v.get("steps").and_then(Value::as_array).cloned().unwrap_or_default(),
            base: base.clone(),
            at: 0,
            frame: 0,
            measure: None,
            t_input: None,
            results: BTreeMap::new(),
            drag_from: None,
            capture: None,
            log: Vec::new(),
            done: false,
        })
    }

    fn next(&mut self) {
        self.at += 1;
        self.frame = 0;
        self.drag_from = None;
    }

    /// Applies this frame's scripted input. Instant steps run back to back until one needs a
    /// frame of its own.
    pub fn before_frame(&mut self, app: &mut App) {
        self.measure = None;
        self.t_input = None;
        self.capture = None;
        // Let the first frames lay the window out.
        if app.canvas.rect.width() <= 1.0 {
            return;
        }
        for _ in 0..1000 {
            let Some(step) = self.steps.get(self.at).cloned() else {
                self.done = true;
                return;
            };
            let measure = step.get("measure").and_then(Value::as_str).map(str::to_string);
            let t = Instant::now();
            let s = &mut app.session;
            let center = |s: &editor::Session| s.camera.to_screen(s.view().center());
            if step.get("fit").is_some() {
                s.zoom_to_fit();
            } else if let Some(z) = step.get("zoom").and_then(Value::as_f64) {
                let c = step.get("center").and_then(|c| Some(Point::new(c.get(0)?.as_f64()?, c.get(1)?.as_f64()?))).unwrap_or_else(|| s.view().center());
                s.camera = Camera { zoom: z, origin: c - s.viewport / (2.0 * z) };
            } else if let Some(name) = step.get("select").and_then(Value::as_str) {
                match find(app, name) {
                    Some(id) => app.session.select(vec![id]),
                    None => self.log.push(format!("select: no node named {name:?}")),
                }
            } else if let Some(name) = step.get("page").and_then(Value::as_str) {
                let doc = app.session.doc();
                let p = doc.pages().iter().filter_map(|&k| doc.node(k)).find(|n| n.name == name).map(|n| n.id);
                if let Some(p) = p {
                    app.session.set_page(p);
                }
            } else if let Some(name) = step.get("reveal").and_then(Value::as_str) {
                // Zoom to a node at a given zoom (default 100 %).
                if let Some(id) = find(app, name)
                    && let Some(b) = app.session.scene.index.bounds(id)
                {
                    let z = step.get("at").and_then(Value::as_f64).unwrap_or(1.0);
                    let s = &mut app.session;
                    s.camera = Camera { zoom: z, origin: b.center() - s.viewport / (2.0 * z) };
                }
            } else if let Some(name) = step.get("instance").and_then(Value::as_str) {
                let at = step.get("at").and_then(|c| Some(Point::new(c.get(0)?.as_f64()?, c.get(1)?.as_f64()?)));
                match find(app, name) {
                    Some(c) => {
                        if let Err(e) = editor::components::insert_instance(&mut app.session, c, at) {
                            self.log.push(format!("instance {name:?}: {e}"));
                        }
                    }
                    None => self.log.push(format!("instance: no component named {name:?}")),
                }
            } else if step.get("enter").is_some() {
                let s = &mut app.session;
                let b = s.selection().first().and_then(|id| s.scene.index.bounds(*id));
                match b {
                    Some(b) => {
                        let _ = s.double_click(b.center());
                    }
                    None => self.log.push("enter: nothing selected".into()),
                }
            } else if let Some(name) = step.get("edit").and_then(Value::as_str) {
                match find(app, name) {
                    Some(id) => {
                        if !app.ctl.edit(&mut app.session, id, None) {
                            self.log.push(format!("edit: {name:?} is neither text nor a path"));
                        }
                    }
                    None => self.log.push(format!("edit: no node named {name:?}")),
                }
            } else if let Some(text) = step.get("type").and_then(Value::as_str) {
                let Some(ch) = text.chars().nth(self.frame) else {
                    self.next();
                    continue;
                };
                match app.ctl.text_input(&mut app.session, &ch.to_string()) {
                    Ok(true) => {}
                    Ok(false) => self.log.push("type: no text is being edited".into()),
                    Err(e) => self.log.push(format!("type: {e}")),
                }
                self.frame += 1;
                self.measure = measure;
                self.t_input = Some(t);
                return;
            } else if let Some(k) = step.get("key").and_then(Value::as_str) {
                let m = mods_of(step.get("mods"));
                let km = KeyMods { ctrl: m.ctrl, shift: m.shift, alt: m.alt };
                match key_of(k) {
                    Some(key) => match app.ctl.key(&mut app.session, key, km) {
                        Ok(true) => {}
                        Ok(false) => {
                            if let Some(c) = commands::shortcut(key, km) {
                                let _ = commands::execute(&mut app.session, c);
                            }
                        }
                        Err(e) => self.log.push(format!("key {k}: {e}")),
                    },
                    None => self.log.push(format!("key: unknown key {k:?}")),
                }
            } else if let Some(ime) = step.get("ime") {
                let r = if let Some(p) = ime.get("preedit").and_then(Value::as_str) {
                    app.ctl.ime_preedit(&mut app.session, p, None)
                } else if let Some(c) = ime.get("commit").and_then(Value::as_str) {
                    app.ctl.ime_commit(&mut app.session, c)
                } else {
                    Ok(())
                };
                if let Err(e) = r {
                    self.log.push(format!("ime: {e}"));
                }
            } else if let Some(points) = step.get("pen").and_then(Value::as_array) {
                app.ctl.set_tool(editor::Tool::Pen);
                let at = |v: &Value, i: usize| v.get(i).and_then(Value::as_f64).unwrap_or(0.0);
                let mut first = None;
                for v in points {
                    let world = Point::new(at(v, 0), at(v, 1));
                    first.get_or_insert(world);
                    let p = Pointer { screen: app.session.camera.to_screen(world), button: Button::Primary, mods: Mods::default(), space: false };
                    let _ = app.ctl.pointer_down(&mut app.session, p, 1);
                    let mut up = p;
                    if v.as_array().is_some_and(|a| a.len() >= 4) {
                        up.screen = app.session.camera.to_screen(world + Vec2::new(at(v, 2), at(v, 3)));
                        let _ = app.ctl.pointer_move(&mut app.session, up);
                    }
                    let _ = app.ctl.pointer_up(&mut app.session, up);
                }
                if step.get("close").is_some_and(|c| c.as_bool() == Some(true))
                    && let Some(f) = first
                {
                    let p = Pointer { screen: app.session.camera.to_screen(f), button: Button::Primary, mods: Mods::default(), space: false };
                    let _ = app.ctl.pointer_down(&mut app.session, p, 1);
                    let _ = app.ctl.pointer_up(&mut app.session, p);
                } else {
                    let _ = app.ctl.key(&mut app.session, Key::Enter, KeyMods::default());
                }
                if app.ctl.pen.is_some() {
                    self.log.push("pen: the path was not finished".into());
                }
            } else if let Some(c) = step.get("command").and_then(Value::as_str) {
                if let Some(c) = command_of(c) {
                    let _ = commands::execute(&mut app.session, c);
                }
                if measure.is_some() {
                    self.measure = measure;
                    self.t_input = Some(t);
                    self.next();
                    return;
                }
            } else if step.get("undoall").is_some() {
                while app.session.store().can_undo() {
                    if app.session.undo().is_err() {
                        break;
                    }
                }
            } else if let Some(p) = step.get("screenshot").and_then(Value::as_str) {
                if self.frame < 2 {
                    // Two quiet frames first, so stale fragments finish encoding.
                    self.frame += 1;
                    return;
                }
                self.capture = Some(self.base.join(p));
                self.next();
                return;
            } else if let Some(p) = step.get("save").and_then(Value::as_str) {
                if let Err(e) = app.session.save_as(self.base.join(p)) {
                    self.log.push(format!("save {p}: {e}"));
                }
            } else if let Some(l) = step.get("layers").and_then(Value::as_str) {
                if l == "expand" {
                    let doc = app.session.doc();
                    let page = app.session.page();
                    let ids: Vec<NodeId> = doc.key(page).map(|pk| doc.descendants(pk).into_iter().filter_map(|k| doc.node(k)).filter(|n| !n.children.is_empty()).map(|n| n.id).collect()).unwrap_or_default();
                    for id in ids {
                        app.session.layers.set_expanded(id, true);
                    }
                }
            } else if let Some(n) = step.get("agent").and_then(Value::as_u64) {
                // Idle until `n` agent requests have been answered. With `measure`, each frame
                // that shows an agent edit is timed from when the request reached the app.
                if let Some(t0) = app.agent_edit.take()
                    && measure.is_some()
                {
                    self.measure = measure;
                    self.t_input = Some(t0);
                }
                if app.agent_count >= n && self.measure.is_none() {
                    self.next();
                    continue;
                }
                return;
            } else if let Some(n) = step.get("wait").and_then(Value::as_u64) {
                if self.frame as u64 >= n {
                    self.next();
                    continue;
                }
                self.frame += 1;
                self.measure = measure;
                self.t_input = Some(t);
                return;
            } else if let Some(d) = step.get("drag") {
                let frames = d.get("frames").and_then(Value::as_u64).unwrap_or(60) as usize;
                let by = d.get("by").and_then(|b| Some(Vec2::new(b.get(0)?.as_f64()?, b.get(1)?.as_f64()?))).unwrap_or(Vec2::new(100.0, 0.0));
                let mods = mods_of(d.get("mods"));
                if self.frame == 0 {
                    let from = if let Some(h) = d.get("handle").and_then(Value::as_str).and_then(handle_of) {
                        transform::selection_box(&app.session).map(|b| app.session.camera.to_screen(b.handle(h)))
                    } else if let Some(name) = d.get("node").and_then(Value::as_str) {
                        find(app, name).and_then(|id| app.session.scene.index.bounds(id)).map(|b| app.session.camera.to_screen(b.center()))
                    } else {
                        d.get("from").and_then(|f| Some(Point::new(f.get(0)?.as_f64()?, f.get(1)?.as_f64()?)))
                    };
                    let Some(from) = from else {
                        self.log.push(format!("drag: no start point for {d}"));
                        self.next();
                        continue;
                    };
                    self.drag_from = Some(from);
                    let p = Pointer { screen: from, button: Button::Primary, mods, space: false };
                    let _ = app.ctl.pointer_down(&mut app.session, p, 1);
                    self.frame = 1;
                    self.t_input = Some(t);
                    return;
                }
                let from = self.drag_from.unwrap_or_default();
                // `"shot": [frame, "file.png"]` captures the window mid-gesture (guides, labels).
                if let Some(shot) = d.get("shot")
                    && shot.get(0).and_then(Value::as_u64) == Some(self.frame as u64)
                    && let Some(f) = shot.get(1).and_then(Value::as_str)
                {
                    self.capture = Some(self.base.join(f));
                }
                let k = self.frame as f64 / frames as f64;
                let p = Pointer { screen: from + by * k.min(1.0), button: Button::Primary, mods, space: false };
                let _ = app.ctl.pointer_move(&mut app.session, p);
                if self.frame >= frames {
                    let _ = app.ctl.pointer_up(&mut app.session, p);
                    self.measure = measure.map(|m| format!("{m}_drop"));
                    self.t_input = Some(t);
                    self.next();
                    return;
                }
                self.frame += 1;
                self.measure = measure;
                self.t_input = Some(t);
                return;
            } else if let Some(pnl) = step.get("panel") {
                let path = pnl.get("path").and_then(Value::as_str).unwrap_or("width").to_string();
                let values = pnl.get("values").and_then(Value::as_array).cloned().unwrap_or_default();
                let Some(v) = values.get(self.frame).cloned() else {
                    self.next();
                    continue;
                };
                let applies = inspect::SECTIONS.iter().find(|sec| sec.fields.iter().any(|f| f.path == path)).map_or(Applies::All, |sec| sec.applies);
                let ids = app.session.selection().to_vec();
                let _ = inspect::write(&mut app.session, &ids, applies, &path, v, Some(&format!("script:{}", self.at)));
                self.frame += 1;
                self.measure = measure;
                self.t_input = Some(t);
                return;
            } else if let Some(colors) = step.get("fill").and_then(Value::as_array) {
                let Some(c) = colors.get(self.frame).cloned() else {
                    self.next();
                    continue;
                };
                let ids = app.session.selection().to_vec();
                let _ = inspect::write(&mut app.session, &ids, Applies::All, "fills", json!([{"type": "solid", "color": c}]), Some(&format!("script:{}", self.at)));
                self.frame += 1;
                self.measure = measure;
                self.t_input = Some(t);
                return;
            } else if let Some(z) = step.get("zoomsweep") {
                let from = z.get("from").and_then(Value::as_f64).unwrap_or(0.03);
                let to = z.get("to").and_then(Value::as_f64).unwrap_or(2.0);
                let frames = z.get("frames").and_then(Value::as_u64).unwrap_or(120) as usize;
                if self.frame > frames {
                    self.next();
                    continue;
                }
                // Out and back, geometric.
                let half = frames as f64 / 2.0;
                let k = if (self.frame as f64) <= half { self.frame as f64 / half } else { (frames - self.frame) as f64 / half };
                let zoom = from * (to / from).powf(k);
                let c = center(s);
                let factor = zoom / s.camera.zoom;
                s.camera.zoom_about(c, factor);
                self.frame += 1;
                self.measure = measure;
                self.t_input = Some(t);
                return;
            } else if let Some(pan) = step.get("pan") {
                let frames = pan.get("frames").and_then(Value::as_u64).unwrap_or(120) as usize;
                let by = pan.get("by").and_then(|b| Some(Vec2::new(b.get(0)?.as_f64()?, b.get(1)?.as_f64()?))).unwrap_or(Vec2::new(-2000.0, 0.0));
                if self.frame >= frames {
                    self.next();
                    continue;
                }
                s.camera.pan(by / frames as f64);
                self.frame += 1;
                self.measure = measure;
                self.t_input = Some(t);
                return;
            } else if let Some(u) = step.get("uiscale").and_then(Value::as_f64) {
                app.set_ui_scale(u as f32);
            } else if step.get("zoommenu").is_some() {
                app.zoom_menu_request = true;
            } else if step.get("quit").is_some() {
                self.done = true;
                return;
            } else {
                self.log.push(format!("unknown step {step}"));
            }
            self.next();
        }
    }

    pub fn after_frame(&mut self, app: &mut App, gfx: &Gfx, _t0: Instant, canvas_ms: [f64; 2]) {
        if let (Some(m), Some(t)) = (self.measure.take(), self.t_input.take()) {
            gfx.wait();
            let ms = t.elapsed().as_secs_f64() * 1e3;
            let r = self.results.entry(m).or_default();
            r.gpu_done.push(ms);
            r.egui.push(app.egui_ms);
            r.scene.push(canvas_ms[0]);
            r.render.push(canvas_ms[1]);
            r.built.push(app.session.scene.stats.build_ms);
            r.prefetch.push(app.session.scene.stats.prefetch_ms);
            let g = gfx.vello.stats;
            r.execute.push(g.execute_ms);
            r.submit.push(g.submit_ms);
            r.glyphs.push(g.glyphs as f64);
            r.paths.push(g.paths as f64);
            r.stale += usize::from(app.session.scene.stats.stale > 0);
        }
        if self.done {
            self.finish(app);
            app.quit = true;
        }
    }

    fn finish(&mut self, app: &App) {
        let mut metrics = serde_json::Map::new();
        for (k, s) in std::mem::take(&mut self.results) {
            let series: Vec<[f64; 8]> = (0..s.gpu_done.len()).map(|i| [s.gpu_done[i], s.egui[i], s.scene[i], s.render[i], s.prefetch[i], s.execute[i], s.submit[i], s.glyphs[i]]).collect();
            metrics.insert(k, json!({
                "frames": series,
                "input_to_gpu_done": stats(s.gpu_done), "egui": stats(s.egui), "scene_frame": stats(s.scene), "vello_render_cpu": stats(s.render), "scene_build": stats(s.built), "scene_prefetch": stats(s.prefetch),
                "vello_execute": stats(s.execute), "vello_submit": stats(s.submit), "glyphs": stats(s.glyphs), "paths": stats(s.paths), "frames_with_stale_fragments": s.stale,
            }));
        }
        let doc = app.session.doc();
        let out = json!({
            "adapter": self.adapter, "document": self.document, "nodes": doc.len(),
            "page_nodes": doc.key(app.session.page()).map_or(0, |k| doc.descendants(k).len()),
            "open_ms": self.open_ms, "rss_mb": memory_stats::memory_stats().map_or(0.0, |m| m.physical_mem as f64 / 1048576.0),
            "metrics": metrics, "log": self.log,
            "undo_label": app.session.store().undo_label(),
        });
        let text = serde_json::to_string_pretty(&out).unwrap_or_default();
        match &self.out {
            Some(p) => {
                if let Some(d) = p.parent() {
                    let _ = std::fs::create_dir_all(d);
                }
                if let Err(e) = std::fs::write(p, &text) {
                    eprintln!("cannot write {}: {e}", p.display());
                }
            }
            None => println!("{text}"),
        }
        for l in &self.log {
            eprintln!("script: {l}");
        }
    }

    pub fn report_first_frame(&self, ms: f64) {
        eprintln!("first frame {ms:.0} ms after start (open {:.0} ms)", self.open_ms);
        if let Some(p) = &self.out
            && let Ok(bytes) = std::fs::read(p)
            && let Ok(mut v) = serde_json::from_slice::<Value>(&bytes)
        {
            v["first_frame_ms"] = json!(ms);
            let _ = std::fs::write(p, serde_json::to_string_pretty(&v).unwrap_or_default());
        }
    }
}
