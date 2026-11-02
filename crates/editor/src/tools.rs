//! Tools and the pointer state machine. The shell feeds pointer events in screen space; the
//! controller turns them into selection changes, gestures, drawing and panning, and exposes
//! what to draw (hover, marquee, feedback) and which cursor to show.

use doc::{NodeId, TxMeta};
use kurbo::{Affine, Point, Rect, Vec2};
use serde_json::{Value, json};

use crate::commands::{Key, KeyMods};
use crate::path_edit::{Grab, PathEdit, Pen, PenPress};
use crate::session::{ClickMods, Error, Session};
use crate::text_edit::{EditKey, EditMods, TextEdit};
use crate::snap::{self, Feedback, Targets};
use crate::transform::{self, Grip, Handle, Mods, MoveGesture, ResizeGesture, RotateGesture};
use crate::xform::Placement;

/// Pointer travel (screen px) before a press becomes a drag.
pub const DRAG_PX: f64 = 3.0;

/// Size of a shape placed by a click without dragging.
pub const DEFAULT_SIZE: f64 = 100.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tool {
    Move,
    Hand,
    Frame,
    Rectangle,
    Ellipse,
    Line,
    Pen,
    Text,
}

impl Tool {
    pub const ALL: [Tool; 8] = [Tool::Move, Tool::Hand, Tool::Frame, Tool::Rectangle, Tool::Ellipse, Tool::Line, Tool::Pen, Tool::Text];

    pub fn label(self) -> &'static str {
        match self {
            Tool::Move => "Move",
            Tool::Hand => "Hand",
            Tool::Frame => "Frame",
            Tool::Rectangle => "Rectangle",
            Tool::Ellipse => "Ellipse",
            Tool::Line => "Line",
            Tool::Pen => "Pen",
            Tool::Text => "Text",
        }
    }

    pub fn key(self) -> char {
        match self {
            Tool::Move => 'V',
            Tool::Hand => 'H',
            Tool::Frame => 'F',
            Tool::Rectangle => 'R',
            Tool::Ellipse => 'O',
            Tool::Line => 'L',
            Tool::Pen => 'P',
            Tool::Text => 'T',
        }
    }

    fn draws(self) -> bool {
        matches!(self, Tool::Frame | Tool::Rectangle | Tool::Ellipse | Tool::Line | Tool::Text)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Button {
    #[default]
    Primary,
    Middle,
    Secondary,
}

/// One pointer event.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Pointer {
    pub screen: Point,
    pub button: Button,
    pub mods: Mods,
    /// Space is held (temporary hand).
    pub space: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Cursor {
    Default,
    Grab,
    Grabbing,
    Crosshair,
    Move,
    /// Resize along a direction (degrees, 0 = horizontal), for picking the nearest arrow.
    Resize(f64),
    Rotate,
    Text,
}

enum State {
    Idle,
    /// Pressed, not yet dragged far enough to decide.
    Pressed { at: Point, world: Point, on_selection: bool, picked: Option<NodeId>, toggled_off: bool },
    Moving(Box<MoveGesture>),
    Resizing(ResizeGesture),
    Rotating(RotateGesture),
    Marquee { start: Point, now: Point, add: bool },
    Panning { last: Point },
    Drawing { tool: Tool, at: Point, start: Point, node: Option<NodeId>, key: String, parent: NodeId, targets: Targets },
    /// Selecting text with the button down.
    TextSelect,
    /// Dragging points or handles in path edit mode.
    PathDrag,
    /// The pen's button is down on the point just placed (dragging out its handles).
    PenDrag,
}

pub struct Controller {
    pub tool: Tool,
    state: State,
    /// Node a click would select now (outline on hover).
    pub hover: Option<NodeId>,
    pub feedback: Feedback,
    pub cursor: Cursor,
    /// Text being edited on the canvas.
    pub text: Option<TextEdit>,
    /// A path in path edit mode.
    pub path: Option<PathEdit>,
    /// A path being drawn with the pen.
    pub pen: Option<Pen>,
    gesture_count: u64,
}

impl Default for Controller {
    fn default() -> Self {
        Controller { tool: Tool::Move, state: State::Idle, hover: None, feedback: Feedback::default(), cursor: Cursor::Default, text: None, path: None, pen: None, gesture_count: 0 }
    }
}

fn resize_angle(b: &transform::SelBox, h: Handle) -> f64 {
    let v = b.handle(h) - b.centre();
    let a = v.y.atan2(v.x).to_degrees();
    if h.is_corner() { a } else { (a / 90.0).round() * 90.0 }
}

fn default_node(tool: Tool) -> Value {
    let grey = json!([{"type": "solid", "color": "#d9d9d9"}]);
    match tool {
        Tool::Frame => json!({"type": "frame", "name": "Frame", "fills": [{"type": "solid", "color": "#ffffff"}]}),
        Tool::Rectangle => json!({"type": "rectangle", "name": "Rectangle", "fills": grey}),
        Tool::Ellipse => json!({"type": "ellipse", "name": "Ellipse", "fills": grey}),
        Tool::Line => json!({"type": "line", "name": "Line", "strokes": [{"paint": {"type": "solid", "color": "#000000"}, "weight": 1}]}),
        _ => json!({"type": "text", "name": "Text", "sizingH": "hug", "sizingV": "hug", "spans": [{"text": "Text"}]}),
    }
}

impl Controller {
    /// True while a press or gesture is in progress.
    pub fn busy(&self) -> bool {
        !matches!(self.state, State::Idle)
    }

    /// The marquee rectangle (world), while dragging one.
    pub fn marquee(&self) -> Option<Rect> {
        match self.state {
            State::Marquee { start, now, .. } => Some(Rect::from_points(start, now)),
            _ => None,
        }
    }

    /// The rotation applied by the current rotate gesture (degrees), for a label.
    pub fn rotating(&self) -> Option<f64> {
        match &self.state {
            State::Rotating(r) => Some(r.angle),
            _ => None,
        }
    }

    pub fn set_tool(&mut self, tool: Tool) {
        self.tool = tool;
        if tool != Tool::Pen {
            self.pen = None;
        }
        self.cursor = match tool {
            Tool::Hand => Cursor::Grab,
            Tool::Text => Cursor::Text,
            Tool::Move => Cursor::Default,
            _ => Cursor::Crosshair,
        };
    }

    pub fn pointer_down(&mut self, s: &mut Session, p: Pointer, clicks: u32) -> Result<(), Error> {
        let world = s.camera.to_world(p.screen);
        self.feedback = Feedback::default();
        if p.button == Button::Middle || p.space || self.tool == Tool::Hand {
            self.state = State::Panning { last: p.screen };
            self.cursor = Cursor::Grabbing;
            return Ok(());
        }
        if p.button != Button::Primary {
            return Ok(());
        }
        if let Some(t) = &mut self.text {
            if t.contains(s, world) {
                t.pointer_down(s, world, p.mods.shift, clicks);
                self.state = State::TextSelect;
                return Ok(());
            }
            self.end_text(s)?;
        }
        if let Some(e) = &mut self.path {
            if clicks >= 2
                && let Some(Grab::Anchor(r)) = e.grab_at(s, p.screen)
            {
                e.toggle_smooth(s, r)?;
                return Ok(());
            }
            if e.pointer_down(s, p.screen, p.mods.shift)? {
                self.state = State::PathDrag;
                return Ok(());
            }
            let inside = s.scene.index.bounds(e.node).is_some_and(|b| b.contains(world));
            if inside || p.mods.shift {
                return Ok(());
            }
            self.path = None;
        }
        if self.tool == Tool::Pen {
            self.gesture_count += 1;
            let key = format!("pen#{}", self.gesture_count);
            let pen = self.pen.get_or_insert_with(|| Pen::new(s, world, key));
            match pen.pointer_down(s, world, p.mods.shift)? {
                PenPress::Added => self.state = State::PenDrag,
                PenPress::Closed => self.finish_pen(s)?,
            }
            return Ok(());
        }
        if self.tool.draws() {
            let index = &s.scene.index;
            let parent = transform::drop_target(s.doc(), index, s.page(), world, &[]);
            let exclude = Default::default();
            let targets = Targets::collect(s.doc(), index, s.page(), parent, &exclude, s.view());
            self.gesture_count += 1;
            self.state = State::Drawing { tool: self.tool, at: p.screen, start: world, node: None, key: format!("draw#{}", self.gesture_count), parent, targets };
            return Ok(());
        }
        if let Some(b) = transform::selection_box(s) {
            match transform::grip_at(&b, &s.camera, p.screen) {
                Some(Grip::Resize(h)) => {
                    if let Some(g) = ResizeGesture::begin(s, h) {
                        self.state = State::Resizing(g);
                    }
                    return Ok(());
                }
                Some(Grip::Rotate(_)) => {
                    if let Some(g) = RotateGesture::begin(s, world) {
                        self.state = State::Rotating(g);
                        self.cursor = Cursor::Rotate;
                    }
                    return Ok(());
                }
                None => {}
            }
        }
        if clicks >= 2 {
            self.state = State::Idle;
            // A double-click on a selected text or path edits it.
            let on = s.selection().first().copied().filter(|_| s.selection().len() == 1).filter(|&id| s.pick(world, true) == Some(id) || s.hit(world) == Some(id));
            if let Some(id) = on
                && self.edit(s, id, Some(world))
            {
                if let Some(t) = &mut self.text {
                    t.pointer_down(s, world, false, 2);
                }
                return Ok(());
            }
            s.double_click(world);
            return Ok(());
        }
        let picked = s.pick(world, p.mods.ctrl);
        let selected = picked.is_some_and(|id| s.selection().contains(&id));
        let in_box = transform::selection_box(s).is_some_and(|b| {
            let l = b.world.inverse() * world;
            l.x >= 0.0 && l.y >= 0.0 && l.x <= b.w && l.y <= b.h
        });
        // A press on something inside a selected node (a card in a selected screen) drags the
        // selection rather than picking the child.
        let inside_selected = in_box && !p.mods.ctrl && picked.is_some_and(|id| within_selection(s, id));
        let mut toggled_off = false;
        let on_selection = match picked {
            // Keep a multi-selection when pressing on one of its members, so it can be dragged.
            Some(_) if (selected || inside_selected) && !p.mods.shift => true,
            Some(id) => {
                s.click(world, ClickMods { toggle: p.mods.shift, deep: p.mods.ctrl });
                toggled_off = !s.selection().contains(&id);
                !toggled_off
            }
            None if in_box && s.selection().len() > 1 && !p.mods.shift => true,
            None => false,
        };
        self.state = State::Pressed { at: p.screen, world, on_selection, picked, toggled_off };
        Ok(())
    }

    /// A press that selects `id` directly (a frame's name label) and may start moving it.
    pub fn press_node(&mut self, s: &mut Session, p: Pointer, id: NodeId) {
        if p.mods.shift {
            s.toggle(id);
        } else if !s.selection().contains(&id) {
            s.select(vec![id]);
        }
        let on = s.selection().contains(&id);
        self.state = State::Pressed { at: p.screen, world: s.camera.to_world(p.screen), on_selection: on, picked: Some(id), toggled_off: !on };
    }

    pub fn pointer_move(&mut self, s: &mut Session, p: Pointer) -> Result<(), Error> {
        let world = s.camera.to_world(p.screen);
        match &mut self.state {
            State::Idle => {
                if let Some(pen) = &mut self.pen {
                    pen.pointer_move(s, world, p.mods.alt)?;
                }
                let editing = self.text.is_some() || self.path.is_some();
                self.hover = if self.tool == Tool::Move && !p.space && !editing { s.pick(world, p.mods.ctrl) } else { None };
                self.cursor = self.idle_cursor(s, p);
            }
            State::TextSelect => {
                if let Some(t) = &mut self.text {
                    t.pointer_drag(s, world);
                }
            }
            State::PathDrag => {
                if let Some(e) = &mut self.path {
                    e.pointer_move(s, p.screen, p.mods.alt)?;
                }
            }
            State::PenDrag => {
                if let Some(pen) = &mut self.pen {
                    pen.pointer_move(s, world, p.mods.alt)?;
                }
            }
            State::Panning { last } => {
                s.camera.pan(p.screen - *last);
                *last = p.screen;
            }
            State::Pressed { at, world: start, on_selection, .. } => {
                if p.screen.distance(*at) < DRAG_PX {
                    return Ok(());
                }
                let start = *start;
                if *on_selection {
                    match MoveGesture::begin(s, start, p.mods.alt)? {
                        Some(mut g) => {
                            self.feedback = g.update(s, world, p.mods)?.clone();
                            self.state = State::Moving(Box::new(g));
                            self.cursor = Cursor::Move;
                        }
                        None => self.state = State::Idle,
                    }
                } else {
                    self.state = State::Marquee { start, now: world, add: p.mods.shift };
                }
                self.hover = None;
            }
            State::Moving(g) => self.feedback = g.update(s, world, p.mods)?.clone(),
            State::Resizing(g) => self.feedback = g.update(s, world, p.mods)?.clone(),
            State::Rotating(g) => g.update(s, world, p.mods)?,
            State::Marquee { now, .. } => *now = world,
            State::Drawing { tool, at, start, node, key, parent, targets } => {
                if node.is_none() && p.screen.distance(*at) < DRAG_PX {
                    return Ok(());
                }
                let thr = snap::SNAP_PX / s.camera.zoom;
                let (mut end, fb) = if s.snap_to_objects || s.snap_to_pixels {
                    let t = if s.snap_to_objects { targets.clone() } else { Targets::default() };
                    snap::snap_point(world, &t, thr, s.snap_to_pixels, (true, true))
                } else {
                    (world, Feedback::default())
                };
                let mut start = *start;
                if *tool == Tool::Line {
                    if p.mods.shift {
                        let d = end - start;
                        let a = (d.y.atan2(d.x) / std::f64::consts::FRAC_PI_4).round() * std::f64::consts::FRAC_PI_4;
                        end = start + Vec2::from_angle(a) * d.hypot();
                    }
                } else {
                    let mut d = end - start;
                    if p.mods.shift {
                        let m = d.x.abs().max(d.y.abs());
                        d = Vec2::new(m.copysign(d.x), m.copysign(d.y));
                        end = start + d;
                    }
                    if p.mods.alt {
                        start -= d;
                    }
                }
                self.feedback = fb;
                let created = draw(s, *tool, *parent, node, key, start, end)?;
                if created {
                    self.hover = None;
                }
            }
        }
        Ok(())
    }

    pub fn pointer_up(&mut self, s: &mut Session, p: Pointer) -> Result<(), Error> {
        let world = s.camera.to_world(p.screen);
        let state = std::mem::replace(&mut self.state, State::Idle);
        self.feedback = Feedback::default();
        match state {
            State::Idle | State::Panning { .. } | State::Resizing(_) | State::Rotating(_) | State::TextSelect => {}
            State::PathDrag => {
                if let Some(e) = &mut self.path {
                    e.pointer_up();
                }
            }
            State::PenDrag => {
                if let Some(pen) = &mut self.pen {
                    pen.pointer_up();
                }
            }
            State::Pressed { picked, on_selection, toggled_off, world: at, .. } => {
                // A click inside a selected node picks what was under the pointer, as usual.
                if on_selection && !p.mods.shift && picked.is_some_and(|id| !s.selection().contains(&id)) {
                    s.click(at, ClickMods { toggle: false, deep: false });
                } else if let Some(id) = picked.filter(|_| on_selection && !toggled_off && !p.mods.shift && s.selection().len() > 1) {
                    // A click on a member of a multi-selection selects just that member.
                    s.select(vec![id]);
                } else if picked.is_none() && !on_selection && !p.mods.shift {
                    s.clear_selection();
                }
            }
            State::Moving(g) => g.finish(s)?,
            State::Marquee { start, add, .. } => {
                s.marquee(start, world, add);
            }
            State::Drawing { tool, start, node, key, parent, .. } => {
                let id = match node {
                    Some(id) => Some(id),
                    None => {
                        // A click: default size from the click point.
                        let mut n = None;
                        let end = if tool == Tool::Line { start + Vec2::new(DEFAULT_SIZE, 0.0) } else { start + Vec2::new(DEFAULT_SIZE, DEFAULT_SIZE) };
                        draw(s, tool, parent, &mut n, &key, start, end)?;
                        n
                    }
                };
                self.set_tool(Tool::Move);
                if let Some(id) = id {
                    s.select(vec![id]);
                    if tool == Tool::Text {
                        // A new text starts in edit mode with its placeholder selected.
                        self.text = TextEdit::begin(s, id, None);
                    }
                }
            }
        }
        self.cursor = self.idle_cursor(s, p);
        Ok(())
    }

    /// Esc: abandons the current gesture (undoing it). Returns `false` if nothing was running.
    pub fn cancel(&mut self, s: &mut Session) -> Result<bool, Error> {
        let state = std::mem::replace(&mut self.state, State::Idle);
        self.feedback = Feedback::default();
        match state {
            State::PathDrag => {
                if let Some(e) = &mut self.path {
                    e.cancel_drag(s)?;
                }
                return Ok(true);
            }
            State::PenDrag | State::Idle if self.pen.is_some() => {
                self.finish_pen(s)?;
                return Ok(true);
            }
            State::Idle | State::TextSelect if self.text.is_some() => {
                self.end_text(s)?;
                return Ok(true);
            }
            State::Idle if self.path.is_some() => {
                self.path = None;
                return Ok(true);
            }
            State::Idle => {
                if self.tool != Tool::Move {
                    self.set_tool(Tool::Move);
                    return Ok(true);
                }
                return Ok(false);
            }
            State::Moving(g) => g.cancel(s)?,
            State::Resizing(g) => g.cancel(s)?,
            State::Rotating(g) => g.cancel(s)?,
            State::Drawing { node: Some(_), .. } => {
                s.undo()?;
            }
            _ => {}
        }
        Ok(true)
    }

    /// Scroll: pan, or zoom about the pointer with Ctrl (or a pinch). `delta` is in screen px.
    pub fn wheel(&mut self, s: &mut Session, screen: Point, delta: Vec2, zoom: bool) {
        if zoom {
            let factor = (-delta.y / 300.0).exp2();
            s.camera.zoom_about(screen, factor);
        } else {
            s.camera.pan(delta);
        }
    }

    /// Enters text editing or path edit mode on `id` (caret at the world point `at`, or all
    /// text selected). Returns `false` if `id` is neither text nor a path.
    pub fn edit(&mut self, s: &mut Session, id: NodeId, at: Option<Point>) -> bool {
        let Some(n) = s.doc().get(id) else { return false };
        match n.kind {
            doc::NodeKind::Text { .. } => {
                self.path = None;
                self.text = TextEdit::begin(s, id, at);
                self.text.is_some()
            }
            doc::NodeKind::Path { .. } if n.virt.is_none() => {
                self.text = None;
                self.path = PathEdit::begin(s, id);
                self.path.is_some()
            }
            _ => false,
        }
    }

    /// Leaves text editing (deleting the node if its text is now empty).
    pub fn end_text(&mut self, s: &mut Session) -> Result<(), Error> {
        if let Some(mut t) = self.text.take() {
            t.finish(s)?;
        }
        Ok(())
    }

    /// Ends every edit mode and drawing.
    pub fn end_edits(&mut self, s: &mut Session) -> Result<(), Error> {
        self.end_text(s)?;
        self.path = None;
        if self.pen.is_some() {
            self.finish_pen(s)?;
        }
        Ok(())
    }

    fn finish_pen(&mut self, s: &mut Session) -> Result<(), Error> {
        self.state = State::Idle;
        if let Some(pen) = self.pen.take() {
            pen.finish(s)?;
        }
        self.set_tool(Tool::Move);
        Ok(())
    }

    /// Keeps edit modes valid after the document changed (undo, scripts): drops a mode whose
    /// node is gone. Call once per frame.
    pub fn sync(&mut self, s: &Session) {
        if self.text.as_mut().is_some_and(|t| !t.sync(s)) {
            self.text = None;
        }
        if self.path.as_mut().is_some_and(|e| !e.sync(s)) {
            self.path = None;
        }
    }

    /// Whether keys go to an edit mode (text, path or pen) rather than shortcuts.
    pub fn editing(&self) -> bool {
        self.text.is_some() || self.path.is_some() || self.pen.is_some()
    }

    /// A key press. Edit modes take their keys first; Enter on a text or path starts editing
    /// it. Returns whether the key was used (otherwise the caller looks up a shortcut).
    pub fn key(&mut self, s: &mut Session, key: Key, m: KeyMods) -> Result<bool, Error> {
        if let Some(t) = &mut self.text {
            if t.preedit.is_some() {
                return Ok(true);
            }
            let em = EditMods { shift: m.shift, word: m.ctrl };
            let ek = match (key, m.ctrl) {
                (Key::Left, _) => EditKey::Left,
                (Key::Right, _) => EditKey::Right,
                (Key::Up, _) => EditKey::Up,
                (Key::Down, _) => EditKey::Down,
                (Key::Home, _) => EditKey::Home,
                (Key::End, _) => EditKey::End,
                (Key::Backspace, _) => EditKey::Backspace,
                (Key::Delete, _) => EditKey::Delete,
                (Key::Enter, false) => EditKey::Enter,
                (Key::Enter, true) | (Key::Escape, _) => {
                    self.end_text(s)?;
                    return Ok(true);
                }
                (Key::Char('a'), true) => EditKey::SelectAll,
                (Key::Char('b'), true) => EditKey::Bold,
                (Key::Char('i'), true) => EditKey::Italic,
                (Key::Char('u'), true) => EditKey::Underline,
                // Undo, redo, clipboard and saving stay shortcuts; plain characters arrive as
                // text input.
                (Key::Char(_), true) | (Key::Tab, _) => return Ok(false),
                (Key::Char(_), false) => return Ok(true),
            };
            t.key(s, ek, em)?;
            return Ok(true);
        }
        if self.pen.is_some() {
            if matches!(key, Key::Enter | Key::Escape) {
                self.finish_pen(s)?;
                return Ok(true);
            }
            return Ok(false);
        }
        if let Some(e) = &mut self.path {
            let step = if m.shift { 10.0 } else { 1.0 };
            match key {
                Key::Enter | Key::Escape => self.path = None,
                Key::Delete | Key::Backspace => {
                    if !e.delete_selected(s)? {
                        self.path = None;
                    }
                }
                Key::Left => e.nudge(s, Vec2::new(-step, 0.0))?,
                Key::Right => e.nudge(s, Vec2::new(step, 0.0))?,
                Key::Up => e.nudge(s, Vec2::new(0.0, -step))?,
                Key::Down => e.nudge(s, Vec2::new(0.0, step))?,
                _ => return Ok(false),
            }
            return Ok(true);
        }
        if key == Key::Enter && !m.shift && !m.ctrl && s.selection().len() == 1 {
            let id = s.selection()[0];
            if self.edit(s, id, None) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Typed text (not composition). Returns whether a text edit took it.
    pub fn text_input(&mut self, s: &mut Session, text: &str) -> Result<bool, Error> {
        let Some(t) = &mut self.text else { return Ok(false) };
        // Control characters arrive as keys.
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        t.type_text(s, &text)?;
        Ok(true)
    }

    /// An input-method composition update (`cursor` in bytes into `text`).
    pub fn ime_preedit(&mut self, s: &mut Session, text: &str, cursor: Option<usize>) -> Result<(), Error> {
        if let Some(t) = &mut self.text {
            t.set_preedit(s, text, cursor)?;
        }
        Ok(())
    }

    /// Composed text committed by the input method.
    pub fn ime_commit(&mut self, s: &mut Session, text: &str) -> Result<(), Error> {
        if let Some(t) = &mut self.text {
            t.commit(s, text)?;
        }
        Ok(())
    }

    fn idle_cursor(&self, s: &Session, p: Pointer) -> Cursor {
        if p.space || self.tool == Tool::Hand {
            return Cursor::Grab;
        }
        if self.tool == Tool::Text {
            return Cursor::Text;
        }
        if self.path.is_some() {
            return Cursor::Default;
        }
        if self.tool.draws() {
            return Cursor::Crosshair;
        }
        if let Some(b) = transform::selection_box(s) {
            match transform::grip_at(&b, &s.camera, p.screen) {
                Some(Grip::Resize(h)) => return Cursor::Resize(resize_angle(&b, h)),
                Some(Grip::Rotate(_)) => return Cursor::Rotate,
                None => {}
            }
        }
        Cursor::Default
    }
}

/// Creates (first call) or updates the node a drawing gesture makes, spanning `a`→`b` (world),
/// inside `parent`. Returns whether it was created now.
fn draw(s: &mut Session, tool: Tool, parent: NodeId, node: &mut Option<NodeId>, key: &str, a: Point, b: Point) -> Result<bool, Error> {
    let inv = s.scene.index.world(parent).unwrap_or(Affine::IDENTITY).inverse();
    let (la, lb) = (inv * a, inv * b);
    let place = if tool == Tool::Line {
        let d = lb - la;
        Placement { x: 0.0, y: 0.0, w: d.hypot(), h: 0.0, rotation: d.y.atan2(d.x).to_degrees(), flip_x: false, flip_y: false }.with_centre(la.midpoint(lb))
    } else {
        let r = Rect::from_points(la, lb);
        Placement { x: r.x0, y: r.y0, w: r.width(), h: r.height(), rotation: 0.0, flip_x: false, flip_y: false }
    };
    let text = tool == Tool::Text;
    let label = format!("Draw {}", tool.label().to_lowercase());
    let created = node.is_none();
    let existing = *node;
    let id = s.transact(TxMeta::user(label).coalesce(key.to_string()), |tx| {
        let id = match existing {
            Some(id) => id,
            None => {
                let mut v = default_node(tool);
                if let Some(o) = v.as_object_mut() {
                    o.insert("x".into(), json!(place.x));
                    o.insert("y".into(), json!(place.y));
                }
                tx.insert_json(Some(parent), None, v)?
            }
        };
        let n = tx.doc().get(id).ok_or(doc::Error::Input("drawn node went away".into()))?;
        let old = Placement::of(n);
        let hugging = n.props.sizing_h != doc::model::Sizing::Fixed;
        // Text hugs its content unless dragged out to a box.
        let new = if text && created { Placement { x: place.x, y: place.y, ..old } } else { place };
        for u in new.diff(&old) {
            tx.update(id, u)?;
        }
        if text && !created && hugging {
            tx.update(id, doc::PropValue::SizingH(doc::model::Sizing::Fixed))?;
        }
        Ok(id)
    })?;
    *node = Some(id);
    Ok(created)
}

/// `id` is a strict descendant of a selected node.
fn within_selection(s: &Session, id: NodeId) -> bool {
    let doc = s.doc();
    let mut at = crate::select::parent_of(doc, id);
    while let Some(p) = at {
        if s.selection().contains(&p) {
            return true;
        }
        at = crate::select::parent_of(doc, p);
    }
    false
}
