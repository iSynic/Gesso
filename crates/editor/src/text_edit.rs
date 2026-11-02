//! Editing a text node on the canvas: caret, selection, typing, deletion, navigation, styling of
//! the selected range, clipboard text and input-method composition. Offsets are byte offsets
//! into the node's joined span text, the same ones `text::ShapedText` hit-tests and draws.
//!
//! Typing coalesces into one undo step per word or pause. Composition (IME preedit) text is
//! written into the document inside an open transaction so the text reflows around it, and is
//! rolled back when the composition changes or ends; only committed text becomes an edit.

use std::sync::Arc;
use std::time::{Duration, Instant};

use doc::model::{TextDecoration, TextSpan};
use doc::{NodeId, NodeKind, TxMeta};
use kurbo::{Affine, Point, Rect};
use text::{ShapedText, TextInput, vertical_offset};

use crate::session::{Error, Session};

/// A pause longer than this starts a new undo step while typing.
const PAUSE: Duration = Duration::from_millis(1000);

/// Editing keys (the shell maps its key events to these).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditKey {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Backspace,
    Delete,
    Enter,
    SelectAll,
    Bold,
    Italic,
    Underline,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EditMods {
    pub shift: bool,
    /// Word-wise movement and deletion (Ctrl, or Alt on macOS).
    pub word: bool,
}

/// An input-method composition in progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preedit {
    pub text: String,
    /// Where the composition sits in the text (bytes).
    pub at: usize,
    /// The composition's own cursor, bytes into `text`.
    pub cursor: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Last {
    None,
    Insert,
    Delete,
}

/// What to draw for the edit: everything in world space.
#[derive(Debug, Clone, Default)]
pub struct EditOverlay {
    /// The text box (four corners).
    pub frame: [Point; 4],
    /// Caret top and bottom, when nothing is selected.
    pub caret: Option<(Point, Point)>,
    /// Selection highlight quads.
    pub selection: Vec<[Point; 4]>,
    /// Underlines under the composition text.
    pub preedit: Vec<(Point, Point)>,
}

pub struct TextEdit {
    pub node: NodeId,
    pub anchor: usize,
    pub cursor: usize,
    pub preedit: Option<Preedit>,
    /// Style for the next typed text (bold toggled with nothing selected).
    pending: Option<TextSpan>,
    /// Horizontal position kept across Up and Down.
    goal_x: Option<f64>,
    /// Undo step counter for coalescing.
    step: u64,
    last: Last,
    last_at: Option<Instant>,
    /// Word or line selection by drag after a double or triple click.
    unit: Unit,
    /// The range first selected by a multi-click, kept while dragging.
    origin: (usize, usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unit {
    Char,
    Word,
    All,
}

/// The shaped text of node `id`, the map from its layout space to world space, and its box
/// in its own space (the layout space is shifted down by the vertical alignment).
pub fn shaped(s: &mut Session, id: NodeId) -> Option<(Arc<ShapedText>, Affine, Rect)> {
    let world = s.scene.index.world(id)?;
    let (scene, doc) = s.scene_and_doc();
    let n = doc.get(id)?;
    let NodeKind::Text { vertical_align, .. } = &n.kind else { return None };
    let input = TextInput::from_kind(&n.kind)?;
    let wrap = layout::semantics::text_wraps(n);
    let (w, h) = (n.props.width, n.props.height);
    let t = scene.text.shape(input, wrap.then_some(w as f32));
    let dy = if f64::from(t.height) > h { 0.0 } else { f64::from(vertical_offset(*vertical_align, h as f32, t.height)) };
    Some((t, world * Affine::translate((0.0, dy)), Rect::new(0.0, -dy, w, h - dy)))
}

fn spans_of(s: &Session, id: NodeId) -> Option<Vec<TextSpan>> {
    match &s.doc().get(id)?.kind {
        NodeKind::Text { spans, .. } => Some(spans.clone()),
        _ => None,
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn prev_char(t: &str, i: usize) -> usize {
    t[..i].char_indices().next_back().map_or(0, |(j, _)| j)
}

fn next_char(t: &str, i: usize) -> usize {
    t[i..].chars().next().map_or(i, |c| i + c.len_utf8())
}

/// Start of the word before `i` (skipping spaces first).
fn prev_word(t: &str, i: usize) -> usize {
    let mut j = i;
    while j > 0 && !is_word(t[..j].chars().next_back().unwrap_or(' ')) {
        j = prev_char(t, j);
    }
    while j > 0 && is_word(t[..j].chars().next_back().unwrap_or(' ')) {
        j = prev_char(t, j);
    }
    j
}

/// End of the word after `i` (skipping spaces first).
fn next_word(t: &str, i: usize) -> usize {
    let mut j = i;
    while j < t.len() && !is_word(t[j..].chars().next().unwrap_or(' ')) {
        j = next_char(t, j);
    }
    while j < t.len() && is_word(t[j..].chars().next().unwrap_or(' ')) {
        j = next_char(t, j);
    }
    j
}

/// The word (or run of other characters) around `i`.
fn word_at(t: &str, i: usize) -> (usize, usize) {
    let class = |c: char| if is_word(c) { 0 } else if c.is_whitespace() { 1 } else { 2 };
    let Some(c) = t[i..].chars().next().or_else(|| t[..i].chars().next_back()) else { return (i, i) };
    let k = class(c);
    let mut a = i;
    while a > 0 && t[..a].chars().next_back().is_some_and(|c| class(c) == k) {
        a = prev_char(t, a);
    }
    let mut b = i;
    while b < t.len() && t[b..].chars().next().is_some_and(|c| class(c) == k) {
        b = next_char(t, b);
    }
    (a, b)
}

impl TextEdit {
    /// Starts editing text node `id`: the caret at the world point `at`, or everything
    /// selected. Returns `None` if `id` is not a text node.
    pub fn begin(s: &mut Session, id: NodeId, at: Option<Point>) -> Option<TextEdit> {
        let spans = spans_of(s, id)?;
        let len = doc::richtext::len(&spans);
        let mut e = TextEdit {
            node: id,
            anchor: 0,
            cursor: len,
            preedit: None,
            pending: None,
            goal_x: None,
            step: 0,
            last: Last::None,
            last_at: None,
            unit: Unit::Char,
            origin: (0, len),
        };
        if let Some(p) = at {
            let i = e.hit(s, p);
            e.anchor = i;
            e.cursor = i;
        }
        s.select(vec![id]);
        Some(e)
    }

    /// The selected byte range, in order.
    pub fn range(&self) -> (usize, usize) {
        (self.anchor.min(self.cursor), self.anchor.max(self.cursor))
    }

    pub fn has_selection(&self) -> bool {
        self.anchor != self.cursor
    }

    /// The node's text, without any composition.
    pub fn text(&self, s: &Session) -> String {
        spans_of(s, self.node).map(|sp| doc::richtext::plain(&sp)).unwrap_or_default()
    }

    /// Whether a world point is on the text box (to keep clicks inside the edit).
    pub fn contains(&self, s: &mut Session, world: Point) -> bool {
        let Some((t, xf, r)) = shaped(s, self.node) else { return false };
        let l = xf.inverse() * world;
        let text_box = Rect::new(0.0, 0.0, f64::from(t.width), f64::from(t.height));
        let slack = 4.0 / s.camera.zoom;
        r.union(text_box).inflate(slack, slack).contains(l)
    }

    /// The byte offset nearest a world point.
    pub fn hit(&self, s: &mut Session, world: Point) -> usize {
        let Some((t, xf, _)) = shaped(s, self.node) else { return 0 };
        let l = xf.inverse() * world;
        t.hit(l.x as f32, l.y as f32)
    }

    /// Keeps offsets valid after the document changed under the edit (undo, another tool).
    /// Returns `false` if the node is gone or no longer text.
    pub fn sync(&mut self, s: &Session) -> bool {
        let Some(spans) = spans_of(s, self.node) else { return false };
        if self.preedit.is_some() {
            return true;
        }
        self.anchor = doc::richtext::floor_boundary(&spans, self.anchor);
        self.cursor = doc::richtext::floor_boundary(&spans, self.cursor);
        true
    }

    // ---- pointer ----------------------------------------------------------------------------

    /// A press inside the text: places the caret (Shift extends), selects a word on a double
    /// click and everything on a triple click.
    pub fn pointer_down(&mut self, s: &mut Session, world: Point, shift: bool, clicks: u32) {
        self.end_composition(s);
        let i = self.hit(s, world);
        let t = self.text(s);
        self.goal_x = None;
        self.new_step();
        match clicks {
            1 if shift => {
                self.cursor = i;
                self.unit = Unit::Char;
            }
            1 => {
                self.anchor = i;
                self.cursor = i;
                self.unit = Unit::Char;
            }
            2 => {
                let (a, b) = word_at(&t, i);
                (self.anchor, self.cursor, self.origin, self.unit) = (a, b, (a, b), Unit::Word);
            }
            _ => {
                (self.anchor, self.cursor, self.origin, self.unit) = (0, t.len(), (0, t.len()), Unit::All);
            }
        }
    }

    /// Dragging after a press extends the selection (by words after a double click).
    pub fn pointer_drag(&mut self, s: &mut Session, world: Point) {
        let i = self.hit(s, world);
        match self.unit {
            Unit::Char => self.cursor = i,
            Unit::Word => {
                let t = self.text(s);
                let (a, b) = word_at(&t, i);
                let (oa, ob) = self.origin;
                if a < oa {
                    (self.anchor, self.cursor) = (ob, a);
                } else {
                    (self.anchor, self.cursor) = (oa, b.max(ob));
                }
            }
            Unit::All => {}
        }
    }

    // ---- keys -------------------------------------------------------------------------------

    /// Handles one editing key. Returns whether it did anything.
    pub fn key(&mut self, s: &mut Session, key: EditKey, m: EditMods) -> Result<bool, Error> {
        if self.preedit.is_some() {
            // The input method owns the keys while composing.
            return Ok(false);
        }
        let t = self.text(s);
        let keep_goal = matches!(key, EditKey::Up | EditKey::Down);
        if !keep_goal {
            self.goal_x = None;
        }
        let (a, b) = self.range();
        let mv = |e: &mut TextEdit, to: usize| {
            e.cursor = to;
            if !m.shift {
                e.anchor = to;
            }
            e.pending = None;
            e.new_step();
        };
        match key {
            EditKey::Left if !m.shift && a != b => mv(self, a),
            EditKey::Right if !m.shift && a != b => mv(self, b),
            EditKey::Left => mv(self, if m.word { prev_word(&t, self.cursor) } else { prev_char(&t, self.cursor) }),
            EditKey::Right => mv(self, if m.word { next_word(&t, self.cursor) } else { next_char(&t, self.cursor) }),
            EditKey::Up | EditKey::Down => {
                let to = self.vertical(s, key == EditKey::Down);
                mv(self, to);
            }
            EditKey::Home | EditKey::End => {
                let to = if m.word { if key == EditKey::Home { 0 } else { t.len() } } else { self.line_edge(s, key == EditKey::End) };
                mv(self, to);
            }
            EditKey::SelectAll => {
                self.anchor = 0;
                self.cursor = t.len();
                self.new_step();
            }
            EditKey::Backspace | EditKey::Delete => {
                let range = if a != b {
                    (a, b)
                } else if key == EditKey::Backspace {
                    (if m.word { prev_word(&t, a) } else { prev_char(&t, a) }, a)
                } else {
                    (a, if m.word { next_word(&t, a) } else { next_char(&t, a) })
                };
                if range.0 == range.1 {
                    return Ok(false);
                }
                self.edit(s, range, "", Last::Delete)?;
            }
            EditKey::Enter => self.type_text(s, "\n")?,
            EditKey::Bold => self.toggle(s, Toggle::Bold)?,
            EditKey::Italic => self.toggle(s, Toggle::Italic)?,
            EditKey::Underline => self.toggle(s, Toggle::Underline)?,
        }
        Ok(true)
    }

    fn vertical(&mut self, s: &mut Session, down: bool) -> usize {
        let Some((t, _, _)) = shaped(s, self.node) else { return self.cursor };
        let c = t.caret(self.cursor);
        let x = *self.goal_x.get_or_insert(c[0]);
        let h = (c[3] - c[1]).max(1.0);
        let y = if down { c[3] + h / 2.0 } else { c[1] - h / 2.0 };
        if y < 0.0 {
            return 0;
        }
        if y > f64::from(t.height) {
            return t.text.len();
        }
        t.hit(x as f32, y as f32)
    }

    fn line_edge(&self, s: &mut Session, end: bool) -> usize {
        let Some((t, _, _)) = shaped(s, self.node) else { return self.cursor };
        let c = t.caret(self.cursor);
        let y = (c[1] + c[3]) / 2.0;
        let i = t.hit(if end { f32::MAX / 4.0 } else { -f32::MAX / 4.0 }, y as f32);
        // A soft-wrapped line's end lands after its trailing space; stay before a newline.
        if end && t.text[..i].ends_with('\n') && i > self.cursor { i - 1 } else { i }
    }

    fn new_step(&mut self) {
        self.step += 1;
        self.last = Last::None;
    }

    // ---- text changes -----------------------------------------------------------------------

    /// Types `text` over the selection.
    pub fn type_text(&mut self, s: &mut Session, text: &str) -> Result<(), Error> {
        self.end_composition(s);
        if text.is_empty() {
            return Ok(());
        }
        let range = self.range();
        self.edit(s, range, text, Last::Insert)
    }

    /// Pastes plain text over the selection as its own undo step.
    pub fn paste(&mut self, s: &mut Session, text: &str) -> Result<(), Error> {
        self.end_composition(s);
        self.new_step();
        let text = text.replace("\r\n", "\n");
        let range = self.range();
        self.edit(s, range, &text, Last::Insert)?;
        self.new_step();
        Ok(())
    }

    /// The selected text, for Copy.
    pub fn selected_text(&self, s: &Session) -> String {
        let (a, b) = self.range();
        self.text(s).get(a..b).unwrap_or_default().to_string()
    }

    /// Cut: returns the selected text and deletes it.
    pub fn cut(&mut self, s: &mut Session) -> Result<String, Error> {
        let out = self.selected_text(s);
        if !out.is_empty() {
            self.new_step();
            self.edit(s, self.range(), "", Last::Delete)?;
            self.new_step();
        }
        Ok(out)
    }

    fn edit(&mut self, s: &mut Session, (a, b): (usize, usize), text: &str, kind: Last) -> Result<(), Error> {
        let meta = self.step_meta(s, a, text, kind);
        let id = self.node;
        let style = self.pending.take();
        s.transact(meta, |tx| tx.edit_spans(id, |sp| doc::richtext::replace(sp, a..b, text, style.as_ref())))?;
        let to = a + text.len();
        self.anchor = to;
        self.cursor = to;
        Ok(())
    }

    /// The undo label and coalescing key for an edit putting `text` at `a`, starting a new
    /// step when the edit kind changes, after a pause, or when a word ends.
    fn step_meta(&mut self, s: &Session, a: usize, text: &str, kind: Last) -> TxMeta {
        let now = Instant::now();
        let paused = self.last_at.is_some_and(|t| now.duration_since(t) > PAUSE);
        // A word ends when a space follows a letter; the next word is its own step.
        let word_end = kind == Last::Insert && text.starts_with(char::is_whitespace) && {
            let t = self.text(s);
            t[..a].chars().next_back().is_some_and(|c| !c.is_whitespace())
        };
        if self.last != kind || paused || word_end {
            self.new_step();
        }
        self.last = kind;
        self.last_at = Some(now);
        let label = if kind == Last::Insert { "Type" } else { "Delete text" };
        TxMeta::user(label).coalesce(format!("text#{}#{}", self.node, self.step))
    }

    // ---- styles -----------------------------------------------------------------------------

    /// Applies `f` to the selected range's spans, as one undo step (edits with the same
    /// `coalesce` key merge, for a dragged value). With nothing selected, the style applies to
    /// what is typed next.
    pub fn apply_style(&mut self, s: &mut Session, label: &str, coalesce: Option<&str>, f: impl Fn(&mut TextSpan)) -> Result<(), Error> {
        let Some(spans) = spans_of(s, self.node) else { return Ok(()) };
        let (a, b) = self.range();
        if a == b {
            let mut p = self.pending.take().unwrap_or_else(|| doc::richtext::style_at(&spans, a));
            f(&mut p);
            self.pending = Some(p);
            return Ok(());
        }
        self.new_step();
        let id = self.node;
        let meta = match coalesce {
            Some(k) => TxMeta::user(label).coalesce(k),
            None => TxMeta::user(label),
        };
        s.transact(meta, |tx| tx.edit_spans(id, |sp| doc::richtext::restyle(sp, a..b, &f)))?;
        Ok(())
    }

    /// The style at the caret or of the selection's first character, with the node's base
    /// style filled in, for the properties panel.
    pub fn current_style(&self, s: &Session) -> Option<TextSpan> {
        let n = s.doc().get(self.node)?;
        let NodeKind::Text { spans, style, .. } = &n.kind else { return None };
        let (a, b) = self.range();
        let mut st = match &self.pending {
            Some(p) if a == b => p.clone(),
            _ if a == b => doc::richtext::style_at(spans, a),
            _ => doc::richtext::slice(spans, a..b).first().map(doc::richtext::style_of).unwrap_or_default(),
        };
        st.font_family.get_or_insert_with(|| style.font_family.clone());
        st.font_size.get_or_insert(style.font_size);
        st.font_weight.get_or_insert(style.font_weight);
        st.italic.get_or_insert(style.italic);
        st.decoration.get_or_insert(style.decoration);
        st.color.get_or_insert(style.color);
        Some(st)
    }

    fn toggle(&mut self, s: &mut Session, what: Toggle) -> Result<(), Error> {
        let Some(n) = s.doc().get(self.node) else { return Ok(()) };
        let NodeKind::Text { spans, style, .. } = &n.kind else { return Ok(()) };
        let base = style.clone();
        let (a, b) = self.range();
        let pieces = if a == b { vec![self.pending.clone().unwrap_or_else(|| doc::richtext::style_at(spans, a))] } else { doc::richtext::slice(spans, a..b) };
        let on = |p: &TextSpan| match what {
            Toggle::Bold => p.font_weight.unwrap_or(base.font_weight) >= 600,
            Toggle::Italic => p.italic.unwrap_or(base.italic),
            Toggle::Underline => p.decoration.unwrap_or(base.decoration) == TextDecoration::Underline,
        };
        // Off only when the whole range is already on.
        let set = !pieces.iter().all(on);
        let label = match what {
            Toggle::Bold => "Bold",
            Toggle::Italic => "Italic",
            Toggle::Underline => "Underline",
        };
        // A value equal to the base style is stored as no override.
        self.apply_style(s, label, None, |p| match what {
            Toggle::Bold => {
                let w = if set { 700 } else { 400 };
                p.font_weight = (w != base.font_weight).then_some(w);
            }
            Toggle::Italic => p.italic = (set != base.italic).then_some(set),
            Toggle::Underline => {
                let d = if set { TextDecoration::Underline } else { TextDecoration::None };
                p.decoration = (d != base.decoration).then_some(d);
            }
        })
    }

    // ---- input methods ----------------------------------------------------------------------

    /// Shows composition text at the caret, replacing any earlier composition. An empty text
    /// ends the composition without inserting anything.
    pub fn set_preedit(&mut self, s: &mut Session, text: &str, cursor: Option<usize>) -> Result<(), Error> {
        if text.is_empty() {
            self.end_composition(s);
            return Ok(());
        }
        // An update replaces the composed text inside the composition's open transaction: one
        // layout per update rather than a rollback and a fresh insert.
        if let Some(p) = &self.preedit
            && s.tx_open()
        {
            let (at, old) = (p.at, p.at + p.text.len());
            let id = self.node;
            let style = self.pending.clone();
            let r = s.in_tx(|tx| tx.edit_spans(id, |sp| doc::richtext::replace(sp, at..old, text, style.as_ref())));
            if let Err(e) = r.and_then(|()| s.sync_tx()) {
                self.end_composition(s);
                return Err(e);
            }
            self.preedit = Some(Preedit { text: text.to_string(), at, cursor: cursor.map(|c| c.min(text.len())) });
            return Ok(());
        }
        self.end_composition(s);
        let (a, b) = self.range();
        // Composition replaces the selection once committed; delete it now as its own edit.
        if a != b {
            self.edit(s, (a, b), "", Last::Delete)?;
        }
        let at = self.cursor;
        let id = self.node;
        let style = self.pending.clone();
        s.begin(TxMeta::user("Compose"))?;
        let r = s.in_tx(|tx| tx.edit_spans(id, |sp| doc::richtext::insert(sp, at, text, style.as_ref())));
        if let Err(e) = r.and_then(|()| s.sync_tx()) {
            s.rollback();
            return Err(e);
        }
        self.preedit = Some(Preedit { text: text.to_string(), at, cursor: cursor.map(|c| c.min(text.len())) });
        Ok(())
    }

    /// Commits composed text: ends the composition and types `text`. The composition's open
    /// transaction becomes the typing step itself (the composed text replaced by `text`), so
    /// committing lays out once.
    pub fn commit(&mut self, s: &mut Session, text: &str) -> Result<(), Error> {
        if !text.is_empty()
            && let Some(p) = self.preedit.take()
            && s.tx_open()
        {
            let (at, old) = (p.at, p.at + p.text.len());
            let meta = self.step_meta(s, at, text, Last::Insert);
            let id = self.node;
            let style = self.pending.take();
            if let Err(e) = s.in_tx(|tx| tx.edit_spans(id, |sp| doc::richtext::replace(sp, at..old, text, style.as_ref()))) {
                s.rollback();
                return Err(e);
            }
            s.set_tx_meta(meta);
            s.commit()?;
            self.anchor = at + text.len();
            self.cursor = self.anchor;
            return Ok(());
        }
        self.end_composition(s);
        self.type_text(s, text)
    }

    /// Drops the composition text (rolls back its open transaction).
    pub fn end_composition(&mut self, s: &mut Session) {
        if self.preedit.take().is_some() && s.tx_open() {
            s.rollback();
        }
    }

    /// Ends the edit: drops any composition, and deletes the node if its text is empty (one
    /// undo step). Returns whether the node was deleted.
    pub fn finish(&mut self, s: &mut Session) -> Result<bool, Error> {
        self.end_composition(s);
        let empty = spans_of(s, self.node).is_some_and(|sp| doc::richtext::len(&sp) == 0);
        let real = s.doc().get(self.node).is_some_and(|n| n.virt.is_none());
        if empty && real {
            let id = self.node;
            s.transact(TxMeta::user("Delete empty text"), |tx| tx.delete(id))?;
            s.clear_selection();
            return Ok(true);
        }
        Ok(false)
    }

    // ---- geometry ---------------------------------------------------------------------------

    /// Caret, selection and composition geometry in world space.
    pub fn overlay(&self, s: &mut Session) -> Option<EditOverlay> {
        let (t, xf, r) = shaped(s, self.node)?;
        let quad = |xf: Affine, b: [f64; 4]| [xf * Point::new(b[0], b[1]), xf * Point::new(b[2], b[1]), xf * Point::new(b[2], b[3]), xf * Point::new(b[0], b[3])];
        let r = r.union(Rect::new(0.0, 0.0, r.x1, f64::from(t.height)));
        let mut out = EditOverlay { frame: quad(xf, [r.x0, r.y0, r.x1, r.y1]), ..Default::default() };
        let (a, b) = self.range();
        match &self.preedit {
            Some(p) => {
                for q in t.selection(p.at, p.at + p.text.len()) {
                    out.preedit.push((xf * Point::new(q[0], q[3] - 1.0), xf * Point::new(q[2], q[3] - 1.0)));
                }
                let c = t.caret(p.at + p.cursor.unwrap_or(p.text.len()));
                out.caret = Some((xf * Point::new(c[0], c[1]), xf * Point::new(c[0], c[3])));
            }
            None if a == b => {
                let c = t.caret(self.cursor);
                out.caret = Some((xf * Point::new(c[0], c[1]), xf * Point::new(c[0], c[3])));
            }
            None => out.selection = t.selection(a, b).into_iter().map(|q| quad(xf, q)).collect(),
        }
        Some(out)
    }
}

/// The properties panel's text style fields (inspect paths) that a span can override.
pub const SPAN_FIELDS: [&str; 6] = ["style.fontFamily", "style.fontSize", "style.fontWeight", "style.italic", "style.color", "style.decoration"];

/// A span style's value for one of [`SPAN_FIELDS`], as the panel shows it.
pub fn span_value(st: &TextSpan, path: &str) -> Option<serde_json::Value> {
    use serde_json::json;
    Some(match path {
        "style.fontFamily" => json!(st.font_family.as_ref()?),
        "style.fontSize" => json!(st.font_size?),
        "style.fontWeight" => json!(st.font_weight?),
        "style.italic" => json!(st.italic?),
        "style.color" => serde_json::to_value(st.color?).ok()?,
        "style.decoration" => serde_json::to_value(st.decoration?).ok()?,
        _ => return None,
    })
}

/// Sets one of [`SPAN_FIELDS`] on a span from a panel value. Returns `false` for other fields
/// or values of the wrong type.
pub fn set_span_field(st: &mut TextSpan, path: &str, v: &serde_json::Value) -> bool {
    match path {
        "style.fontFamily" => v.as_str().map(|f| st.font_family = Some(f.to_string())).is_some(),
        "style.fontSize" => v.as_f64().filter(|x| *x > 0.0).map(|x| st.font_size = Some(x)).is_some(),
        "style.fontWeight" => v.as_f64().map(|x| st.font_weight = Some(x.round().clamp(1.0, 1000.0) as u16)).is_some(),
        "style.italic" => v.as_bool().map(|b| st.italic = Some(b)).is_some(),
        "style.color" => serde_json::from_value(v.clone()).map(|c| st.color = Some(c)).is_ok(),
        "style.decoration" => serde_json::from_value(v.clone()).map(|d| st.decoration = Some(d)).is_ok(),
        _ => false,
    }
}

#[derive(Clone, Copy)]
enum Toggle {
    Bold,
    Italic,
    Underline,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words() {
        let t = "hello, big world";
        assert_eq!(next_word(t, 0), 5);
        assert_eq!(next_word(t, 5), 10);
        assert_eq!(prev_word(t, t.len()), 11);
        assert_eq!(prev_word(t, 11), 7);
        assert_eq!(word_at(t, 8), (7, 10));
        assert_eq!(word_at(t, 5), (5, 6));
        assert_eq!(word_at("日本語 テキスト", 0), (0, 9));
    }
}
