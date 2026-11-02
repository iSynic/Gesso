//! The canvas area: routes egui's pointer and key events to the editor's controller, and draws
//! the overlays (hover, selection and handles, marquee, guides, labels) in screen space with
//! egui's painter so they stay crisp at any zoom.

use std::time::Instant;

use doc::{NodeId, NodeKind};
use editor::commands::{Command, Key, KeyMods};
use editor::transform::{self, Handle, Mods};
use editor::{Controller, Cursor, Pointer, Session};
use egui::{Color32, CursorIcon, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Vec2};
use kurbo::{Affine, BezPath, PathEl, Point, Shape as _};

pub const ACCENT: Color32 = Color32::from_rgb(56, 132, 255);
const GUIDE: Color32 = Color32::from_rgb(242, 72, 120);
const LABEL: Color32 = Color32::from_rgb(110, 110, 120);

/// Per-frame canvas state the app keeps.
pub struct CanvasState {
    pub rect: Rect,
    last_press: Option<(Instant, Pos2)>,
    clicks: u32,
    /// Frame name labels drawn last frame (screen rects), for clicking.
    labels: Vec<(NodeId, Rect)>,
    pub space: bool,
    /// The layers exposed to assistive technology last frame, by accessibility node.
    a11y: Vec<(egui::accesskit::NodeId, NodeId)>,
}

impl Default for CanvasState {
    fn default() -> Self {
        CanvasState { rect: Rect::NOTHING, last_press: None, clicks: 0, labels: Vec::new(), space: false, a11y: Vec::new() }
    }
}

/// Something the canvas asks the app to do (it owns files and the clipboard).
pub enum Request {
    Command(Command),
    Copy,
    Cut,
    Paste(String),
    /// A bitmap from the clipboard.
    PasteImage(std::sync::Arc<egui::ColorImage>),
}

fn to_pos(p: Point) -> Pos2 {
    Pos2::new(p.x as f32, p.y as f32)
}

fn to_point(p: Pos2) -> Point {
    Point::new(f64::from(p.x), f64::from(p.y))
}

fn mods_of(m: egui::Modifiers) -> Mods {
    Mods { shift: m.shift, alt: m.alt, ctrl: m.command }
}

fn key_of(k: egui::Key) -> Option<Key> {
    use egui::Key as K;
    Some(match k {
        K::Delete => Key::Delete,
        K::Backspace => Key::Backspace,
        K::Escape => Key::Escape,
        K::Enter => Key::Enter,
        K::Tab => Key::Tab,
        K::Home => Key::Home,
        K::End => Key::End,
        K::ArrowLeft => Key::Left,
        K::ArrowRight => Key::Right,
        K::ArrowUp => Key::Up,
        K::ArrowDown => Key::Down,
        K::OpenBracket => Key::Char('['),
        K::CloseBracket => Key::Char(']'),
        K::Minus => Key::Char('-'),
        K::Plus | K::Equals => Key::Char('='),
        K::Quote => Key::Char('\''),
        _ => {
            let name = k.name();
            let mut chars = name.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Key::Char(c.to_ascii_lowercase()),
                _ => return None,
            }
        }
    })
}

fn cursor_icon(c: Cursor) -> CursorIcon {
    match c {
        Cursor::Default => CursorIcon::Default,
        Cursor::Grab => CursorIcon::Grab,
        Cursor::Grabbing => CursorIcon::Grabbing,
        Cursor::Crosshair => CursorIcon::Crosshair,
        Cursor::Move => CursorIcon::Move,
        Cursor::Rotate => CursorIcon::Alias,
        Cursor::Text => CursorIcon::Text,
        Cursor::Resize(a) => {
            let a = a.rem_euclid(180.0);
            if !(22.5..157.5).contains(&a) {
                CursorIcon::ResizeHorizontal
            } else if a < 67.5 {
                CursorIcon::ResizeNwSe
            } else if a < 112.5 {
                CursorIcon::ResizeVertical
            } else {
                CursorIcon::ResizeNeSw
            }
        }
    }
}

/// Handles this frame's input on the canvas. Returns requests for the app and whether
/// anything changed that needs another frame.
pub fn input(ui: &mut egui::Ui, st: &mut CanvasState, s: &mut Session, ctl: &mut Controller, on_input: &mut Option<Instant>) -> Vec<Request> {
    let rect = st.rect;
    let id = canvas_id();
    let resp = ui.interact(rect, id, Sense::click_and_drag());
    let ctx = ui.ctx().clone();
    let over = resp.contains_pointer();
    ctl.sync(s);
    // The canvas takes keyboard focus when pressed (or while editing), so Tab, arrows and Esc
    // reach it instead of moving focus between panel widgets.
    if resp.is_pointer_button_down_on() || (ctl.editing() && !ctx.egui_wants_keyboard_input()) {
        resp.request_focus();
    }
    let focused = resp.has_focus();
    if focused {
        ui.memory_mut(|m| m.set_focus_lock_filter(id, egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true }));
    }
    let keyboard_free = focused || !ctx.egui_wants_keyboard_input();
    // Leaving the canvas for a panel field ends a text edit's composition.
    if !keyboard_free && let Some(t) = &mut ctl.text {
        t.end_composition(s);
    }
    let mut out = Vec::new();
    let (events, modifiers, space) = ui.input(|i| (i.events.clone(), i.modifiers, i.key_down(egui::Key::Space)));
    st.space = space && keyboard_free;
    let ptr = |pos: Pos2, m: egui::Modifiers, button: editor::Button| Pointer { screen: to_point(pos - rect.min.to_vec2()), button, mods: mods_of(m), space: st.space };
    let mut stamp = || {
        on_input.get_or_insert_with(Instant::now);
    };
    for e in events {
        let r: Result<(), editor::Error> = (|| {
            match e {
                egui::Event::PointerMoved(pos) if over || ctl.busy() => {
                    stamp();
                    ctl.pointer_move(s, ptr(pos, modifiers, editor::Button::Primary))?;
                }
                egui::Event::PointerButton { pos, button, pressed, modifiers: m, .. } => {
                    let button = match button {
                        egui::PointerButton::Primary => editor::Button::Primary,
                        egui::PointerButton::Middle => editor::Button::Middle,
                        _ => editor::Button::Secondary,
                    };
                    if pressed && over && rect.contains(pos) {
                        stamp();
                        let now = Instant::now();
                        st.clicks = match st.last_press {
                            Some((t, p)) if now.duration_since(t).as_millis() < 450 && p.distance(pos) < 5.0 => st.clicks + 1,
                            _ => 1,
                        };
                        st.last_press = Some((now, pos));
                        let p = ptr(pos, m, button);
                        let label = st.labels.iter().rev().find(|(_, r)| r.contains(pos)).map(|(id, _)| *id);
                        match label {
                            Some(id) if button == editor::Button::Primary && ctl.tool == editor::Tool::Move && !p.space => ctl.press_node(s, p, id),
                            _ => ctl.pointer_down(s, p, st.clicks)?,
                        }
                    } else if !pressed && ctl.busy() {
                        stamp();
                        ctl.pointer_up(s, ptr(pos, m, button))?;
                    }
                }
                egui::Event::MouseWheel { unit, delta, modifiers: m, .. } if over => {
                    stamp();
                    let k = match unit {
                        egui::MouseWheelUnit::Point => 1.0,
                        egui::MouseWheelUnit::Line => 40.0,
                        egui::MouseWheelUnit::Page => rect.height(),
                    };
                    let d = kurbo::Vec2::new(f64::from(delta.x * k), f64::from(delta.y * k));
                    let at = ui.input(|i| i.pointer.hover_pos()).map_or(Point::new(f64::from(rect.width()) / 2.0, f64::from(rect.height()) / 2.0), |p| to_point(p - rect.min.to_vec2()));
                    if m.command {
                        ctl.wheel(s, at, kurbo::Vec2::new(0.0, -d.y), true);
                    } else if m.shift && d.x == 0.0 {
                        ctl.wheel(s, at, kurbo::Vec2::new(d.y, 0.0), false);
                    } else {
                        ctl.wheel(s, at, d, false);
                    }
                }
                egui::Event::Zoom(f) if over => {
                    stamp();
                    let at = ui.input(|i| i.pointer.hover_pos()).map_or(Point::ZERO, |p| to_point(p - rect.min.to_vec2()));
                    s.camera.zoom_about(at, f64::from(f));
                }
                egui::Event::Key { key, pressed: true, modifiers: m, .. } if keyboard_free => {
                    let km = KeyMods { ctrl: m.command, shift: m.shift, alt: m.alt };
                    if key == egui::Key::Escape && (ctl.busy() || ctl.editing()) {
                        stamp();
                        ctl.cancel(s)?;
                    } else if key == egui::Key::Escape && ctl.tool != editor::Tool::Move {
                        ctl.cancel(s)?;
                    } else if let Some(k) = key_of(key) {
                        if !ctl.busy() && ctl.key(s, k, km)? {
                            stamp();
                            return Ok(());
                        }
                        match editor::commands::shortcut(k, km) {
                            // egui delivers these as their own events.
                            Some(Command::Copy | Command::Cut | Command::Paste) => {}
                            Some(Command::Tool(t)) if !ctl.busy() => ctl.set_tool(t),
                            Some(c) if !ctl.busy() => {
                                stamp();
                                out.push(Request::Command(c));
                            }
                            _ => {}
                        }
                    }
                }
                egui::Event::Text(t) if keyboard_free && ctl.text.is_some() => {
                    stamp();
                    ctl.text_input(s, &t)?;
                }
                egui::Event::Ime(ime) if keyboard_free && ctl.text.is_some() => {
                    stamp();
                    match ime {
                        egui::ImeEvent::Preedit { text, active_range_chars } => {
                            // The input method's cursor, from characters to bytes.
                            let cursor = active_range_chars.map(|r| text.char_indices().nth(r.end).map_or(text.len(), |(i, _)| i));
                            ctl.ime_preedit(s, &text, cursor)?;
                        }
                        egui::ImeEvent::Commit(text) => ctl.ime_commit(s, &text)?,
                        _ => {}
                    }
                }
                // A screen reader's "click" or "focus" on a layer selects it.
                egui::Event::AccessKitActionRequest(req) if matches!(req.action, egui::accesskit::Action::Click | egui::accesskit::Action::Focus) => {
                    if let Some(&(_, node)) = st.a11y.iter().find(|(a, _)| *a == req.target_node) {
                        stamp();
                        ctl.end_edits(s)?;
                        s.select(vec![node]);
                    }
                }
                egui::Event::Copy if keyboard_free && ctl.text.is_some() => {
                    if let Some(t) = &ctl.text {
                        ctx.copy_text(t.selected_text(s));
                    }
                }
                egui::Event::Cut if keyboard_free && ctl.text.is_some() => {
                    if let Some(t) = &mut ctl.text {
                        let cut = t.cut(s)?;
                        ctx.copy_text(cut);
                    }
                }
                egui::Event::Paste(text) if keyboard_free && ctl.text.is_some() => {
                    stamp();
                    if let Some(t) = &mut ctl.text {
                        t.paste(s, &text)?;
                    }
                }
                egui::Event::Copy if keyboard_free => out.push(Request::Copy),
                egui::Event::Cut if keyboard_free => out.push(Request::Cut),
                egui::Event::Paste(text) if keyboard_free => out.push(Request::Paste(text)),
                egui::Event::PasteImage(img) if keyboard_free => out.push(Request::PasteImage(img)),
                _ => {}
            }
            Ok(())
        })();
        if let Err(err) = r {
            // Edits are transactional: a failed one changed nothing. Show it and carry on.
            out.push(Request::Command(Command::Escape));
            eprintln!("edit failed: {err}");
        }
    }
    accessibility(ui, st, s, ctl, &resp);
    if over || ctl.busy() {
        let text_hover = ctl.text.as_ref().is_some_and(|_| ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| rect.contains(p)));
        let c = if st.space && !ctl.busy() { Cursor::Grab } else if text_hover && !ctl.busy() { text_cursor(ui, rect, s, ctl) } else { ctl.cursor };
        ctx.set_cursor_icon(cursor_icon(c));
    }
    // An edited text asks for the input method, positioned at the caret.
    if keyboard_free
        && let Some(t) = &ctl.text
        && let Some(o) = t.overlay(s)
    {
        let cam = s.camera.affine();
        let caret = o.caret.map_or_else(
            || Rect::from_points(&o.selection.iter().flatten().map(|&q| rect.min + to_pos(cam * q).to_vec2()).collect::<Vec<_>>()),
            |(a, b)| Rect::from_two_pos(rect.min + to_pos(cam * a).to_vec2(), rect.min + to_pos(cam * b).to_vec2()),
        );
        ctx.output_mut(|out| {
            out.ime = Some(egui::output::IMEOutput { purpose: egui::IMEPurpose::Normal, rect, cursor_rect: caret, should_interrupt_composition: false });
        });
    }
    out
}

/// The canvas in the accessibility tree: a canvas node naming the page and describing the
/// selection (or the text being edited), with a child per layer around the selection.
fn accessibility(ui: &mut egui::Ui, st: &mut CanvasState, s: &Session, ctl: &Controller, resp: &egui::Response) {
    use egui::accesskit::Role;
    let ctx = ui.ctx().clone();
    let page = s.doc().get(s.page()).map_or_else(String::new, |p| p.name.clone());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Canvas, true, format!("Canvas, page {page}")));
    let editing = ctl.text.as_ref().map(|t| t.text(s));
    let hint = "Tab and Shift+Tab move between layers, Enter goes into a layer or edits text, Shift+Enter goes to the parent";
    ctx.accesskit_node_builder(resp.id, |n| {
        n.set_role(Role::Canvas);
        n.set_description(format!("{}. {hint}", describe(s)));
        if let Some(t) = &editing {
            n.set_value(t.clone());
        }
    });
    st.a11y.clear();
    let rect = st.rect;
    for a in editor::a11y::canvas_nodes(s) {
        let bounds = a.bounds.map_or(Rect::from_min_size(rect.min, Vec2::ZERO), |b| {
            Rect::from_min_max(rect.min + Vec2::new(b.x0 as f32, b.y0 as f32), rect.min + Vec2::new(b.x1 as f32, b.y1 as f32))
        });
        let role = match a.role {
            editor::a11y::Role::Group => Role::Group,
            editor::a11y::Role::Text => Role::Label,
            editor::a11y::Role::Image => Role::Image,
            editor::a11y::Role::Shape => Role::GraphicsObject,
        };
        let child = ui.new_child(
            egui::UiBuilder::new()
                .id_salt(("a11y", a.id.to_string()))
                .max_rect(bounds.intersect(rect))
                .accessibility_parent(resp.id)
                .accessibility_role(role)
                .accessibility_label(a.label.clone()),
        );
        let id = child.unique_id();
        ctx.accesskit_node_builder(id, |n| {
            n.set_bounds(egui::accesskit::Rect { x0: bounds.min.x.into(), y0: bounds.min.y.into(), x1: bounds.max.x.into(), y1: bounds.max.y.into() });
            n.set_selected(a.selected);
            if let Some(v) = &a.value {
                n.set_value(v.clone());
            }
            n.add_action(egui::accesskit::Action::Click);
        });
        st.a11y.push((id.accesskit_id(), a.id));
    }
}

/// Nothing but the canvas has keyboard focus (no panel field is being typed into), so canvas
/// and panel shortcuts may act.
pub fn keyboard_free(ctx: &egui::Context) -> bool {
    ctx.memory(|m| m.focused().is_none_or(|f| f == canvas_id()))
}

/// The canvas widget's id (its keyboard focus and accessibility node).
pub fn canvas_id() -> egui::Id {
    egui::Id::NULL.with("canvas")
}

/// The I-beam over the edited text, the usual cursor elsewhere.
fn text_cursor(ui: &egui::Ui, rect: Rect, s: &mut Session, ctl: &mut Controller) -> Cursor {
    let Some(pos) = ui.input(|i| i.pointer.hover_pos()) else { return ctl.cursor };
    let world = s.camera.to_world(to_point(pos - rect.min.to_vec2()));
    match &ctl.text {
        Some(t) if t.contains(s, world) => Cursor::Text,
        _ => ctl.cursor,
    }
}

// ---- overlays -----------------------------------------------------------------------------------

struct Painter<'a> {
    p: &'a egui::Painter,
    origin: Pos2,
    /// World → canvas-local screen.
    cam: Affine,
}

impl Painter<'_> {
    fn pt(&self, w: Point) -> Pos2 {
        self.origin + to_pos(self.cam * w).to_vec2()
    }

    fn line(&self, a: Point, b: Point, stroke: Stroke) {
        self.p.line_segment([self.pt(a), self.pt(b)], stroke);
    }

    /// A world-space path, flattened in screen space.
    fn path(&self, path: &BezPath, to_world: Affine, stroke: Stroke) {
        let xf = self.cam * to_world;
        let mut pts: Vec<Pos2> = Vec::new();
        let flush = |pts: &mut Vec<Pos2>, closed: bool| {
            if pts.len() >= 2 {
                let shape = if closed { Shape::closed_line(std::mem::take(pts), stroke) } else { Shape::line(std::mem::take(pts), stroke) };
                self.p.add(shape);
            }
            pts.clear();
        };
        let mut closed = false;
        kurbo::flatten((xf * path.clone()).iter(), 0.25, |el| match el {
            PathEl::MoveTo(p) => {
                flush(&mut pts, closed);
                closed = false;
                pts.push(self.origin + to_pos(p).to_vec2());
            }
            PathEl::LineTo(p) => pts.push(self.origin + to_pos(p).to_vec2()),
            PathEl::ClosePath => closed = true,
            _ => {}
        });
        flush(&mut pts, closed);
    }

    /// A filled label centred on `at`, moved right past any rect in `avoid` it would cover.
    /// Returns where it was drawn.
    fn label(&self, at: Pos2, text: String, fill: Color32, avoid: &[Rect]) -> Rect {
        let galley = self.p.layout_no_wrap(text, egui::FontId::proportional(11.0), Color32::WHITE);
        let mut r = Rect::from_center_size(at, galley.size() + Vec2::new(8.0, 4.0));
        while let Some(o) = avoid.iter().find(|o| o.intersects(r)) {
            r = r.translate(Vec2::new(o.max.x + 4.0 - r.min.x, 0.0));
        }
        self.p.rect_filled(r, 3.0, fill);
        self.p.galley(r.min + Vec2::new(4.0, 2.0), galley, Color32::WHITE);
        r
    }
}

fn node_outline(n: &doc::Node) -> BezPath {
    match render::geometry::outline(n) {
        Some(o) => o.path,
        None => kurbo::Rect::new(0.0, 0.0, n.props.width, n.props.height).to_path(0.1),
    }
}

/// The selection described for the status bar and screen readers.
pub fn describe(s: &Session) -> String {
    let doc = s.doc();
    match s.selection() {
        [] => "Nothing selected".into(),
        [one] => doc.get(*one).map_or_else(String::new, |n| format!("{} \u{201c}{}\u{201d}, {} \u{d7} {}", n.kind.type_name(), n.name, fmt_num(n.props.width), fmt_num(n.props.height))),
        many => format!("{} layers selected", many.len()),
    }
}

pub fn fmt_num(v: f64) -> String {
    let r = (v * 100.0).round() / 100.0;
    if r.fract() == 0.0 { format!("{r:.0}") } else { format!("{r}") }
}

pub fn paint(ui: &egui::Ui, st: &mut CanvasState, s: &mut Session, ctl: &Controller, tex: egui::TextureId) {
    let text = ctl.text.as_ref().and_then(|t| t.overlay(s));
    let s = &*s;
    let rect = st.rect;
    let painter = ui.painter_at(rect);
    painter.image(tex, rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
    let p = Painter { p: &painter, origin: rect.min, cam: s.camera.affine() };
    let doc = s.doc();
    let index = &s.scene.index;
    let thin = Stroke::new(1.0, ACCENT);

    // Frame name labels for top-level frames in view.
    st.labels.clear();
    let view = s.view();
    if let Some(page) = doc.get(s.page()) {
        for n in page.children.iter().filter_map(|&k| doc.node(k)) {
            if !matches!(n.kind, NodeKind::Frame { .. } | NodeKind::Component { .. }) || !n.props.visible {
                continue;
            }
            let Some(b) = index.bounds(n.id).filter(|b| b.overlaps(view)) else { continue };
            let w = (b.width() * s.camera.zoom) as f32;
            if w < 24.0 {
                continue;
            }
            let at = p.pt(Point::new(b.x0, b.y0)) + Vec2::new(0.0, -4.0);
            let selected = s.selection().contains(&n.id);
            let colour = if selected { ACCENT } else { LABEL };
            // One line, elided to the frame's width.
            let mut job = egui::text::LayoutJob::simple_singleline(n.name.clone(), egui::FontId::proportional(11.0), colour);
            job.wrap = egui::text::TextWrapping::truncate_at_width(w);
            let galley = painter.layout_job(job);
            let r = Rect::from_min_size(at - Vec2::new(0.0, galley.size().y), Vec2::new(galley.size().x.min(w), galley.size().y));
            painter.galley(r.min, galley, colour);
            st.labels.push((n.id, r));
        }
    }

    // Hover outline.
    if let Some(h) = ctl.hover.filter(|h| !s.selection().contains(h))
        && let (Some(n), Some(w)) = (doc.get(h), index.world(h))
    {
        p.path(&node_outline(n), w, Stroke::new(1.5, ACCENT));
    }

    // Selected nodes' outlines.
    for &id in s.selection() {
        if let (Some(n), Some(w)) = (doc.get(id), index.world(id)) {
            p.path(&node_outline(n), w, thin);
        }
    }

    // Drop target and reorder line.
    let fb = &ctl.feedback;
    if let Some(t) = fb.target
        && let (Some(n), Some(w)) = (doc.get(t), index.world(t))
    {
        p.path(&node_outline(n), w, Stroke::new(2.0, ACCENT));
    }
    if let Some((a, b)) = fb.reorder {
        p.line(a, b, Stroke::new(3.0, ACCENT));
    }

    // Text being edited: its box, the selection or caret, and the composition underline.
    if let Some(o) = &text {
        painter.add(Shape::closed_line(o.frame.iter().map(|&c| p.pt(c)).collect(), thin));
        for q in &o.selection {
            painter.add(Shape::convex_polygon(q.iter().map(|&c| p.pt(c)).collect(), ACCENT.gamma_multiply(0.3), Stroke::NONE));
        }
        for &(a, b) in &o.preedit {
            p.line(a, b, Stroke::new(1.5, Color32::BLACK));
        }
        if let Some((a, b)) = o.caret {
            p.line(a, b, Stroke::new(1.5, ACCENT));
        }
    }

    // Path edit mode: the outline, handles and points.
    if let Some(e) = &ctl.path {
        let o = e.overlay(s);
        p.path(&o.outline, Affine::IDENTITY, thin);
        for &(a, h) in &o.handles {
            p.line(a, h, thin);
            painter.circle(p.pt(h), 3.5, Color32::WHITE, thin);
        }
        for &(a, sel) in &o.anchors {
            let r = Rect::from_center_size(p.pt(a), Vec2::splat(7.0));
            painter.rect(r, 0.0, if sel { ACCENT } else { Color32::WHITE }, thin, StrokeKind::Middle);
        }
    }

    // The pen: the path so far, its points and the next segment.
    if let Some(pen) = &ctl.pen {
        p.path(&pen.outline(s), Affine::IDENTITY, Stroke::new(1.5, ACCENT));
        if let Some((a, b)) = pen.preview(s).filter(|_| !pen.is_pressed()) {
            p.line(a, b, Stroke::new(1.0, ACCENT.gamma_multiply(0.6)));
        }
        let pts = pen.anchors(s);
        let closing = pts.len() > 2 && pen.hover.is_some_and(|h| s.camera.to_screen(h).distance(s.camera.to_screen(pts[0])) <= editor::path_edit::CLOSE_PX);
        for (i, &a) in pts.iter().enumerate() {
            let r = Rect::from_center_size(p.pt(a), Vec2::splat(if i == 0 && closing { 10.0 } else { 7.0 }));
            painter.rect(r, 0.0, Color32::WHITE, thin, StrokeKind::Middle);
        }
    }

    // Handles and size label (measure labels move aside for it).
    let mut size_label: Option<Rect> = None;
    if (!ctl.busy() || ctl.marquee().is_none())
        && text.is_none()
        && ctl.path.is_none()
        && let Some(b) = transform::selection_box(s)
    {
            let corners = b.corners().map(|c| p.pt(c));
            painter.add(Shape::closed_line(corners.to_vec(), thin));
            let big = corners[0].distance(corners[1]) > 20.0 && corners[1].distance(corners[2]) > 20.0;
            for h in Handle::ALL {
                if !h.is_corner() && !big {
                    continue;
                }
                let c = p.pt(b.handle(h));
                let r = Rect::from_center_size(c, Vec2::splat(transform::HANDLE_PX as f32 * 1.6));
                painter.rect(r, 1.0, Color32::WHITE, thin, StrokeKind::Middle);
            }
            let bottom = corners.iter().map(|c| c.y).fold(f32::MIN, f32::max);
            let cx = corners.iter().map(|c| c.x).sum::<f32>() / 4.0;
            let text = match ctl.rotating() {
                Some(a) => format!("{}\u{b0}", fmt_num(a)),
                None => format!("{} \u{d7} {}", fmt_num(b.w), fmt_num(b.h)),
            };
            size_label = Some(p.label(Pos2::new(cx, bottom + 14.0), text, ACCENT, &[]));
    }

    // Marquee.
    if let Some(m) = ctl.marquee() {
        let r = Rect::from_two_pos(p.pt(Point::new(m.x0, m.y0)), p.pt(Point::new(m.x1, m.y1)));
        painter.rect(r, 0.0, ACCENT.gamma_multiply(0.08), thin, StrokeKind::Middle);
    }

    // Snapping feedback.
    let guide = Stroke::new(1.0, GUIDE);
    for g in &fb.guides {
        p.line(g.a, g.b, guide);
    }
    for m in fb.gaps.iter().chain(&fb.measures) {
        p.line(m.a, m.b, guide);
        // End ticks.
        let d = m.b - m.a;
        let n = kurbo::Vec2::new(-d.y, d.x).normalize() * (3.0 / s.camera.zoom);
        p.line(m.a - n, m.a + n, guide);
        p.line(m.b - n, m.b + n, guide);
        if (m.b - m.a).hypot() * s.camera.zoom > 14.0 {
            p.label(p.pt(m.a.midpoint(m.b)), fmt_num(m.value), GUIDE, size_label.as_slice());
        }
    }
}
