//! The layers panel (pages and a virtualised layer tree) and the properties panel (generated
//! from `editor::inspect::SECTIONS`, plus list editors for fills, strokes and effects).

use std::time::Instant;

use doc::variables::VariableType;
use doc::{Document, Id, NodeId, NodeKind, TxMeta};
use editor::commands::{self, Command};
use editor::inspect::{self, Applies, Field, Shown, Widget};
use editor::{Controller, Session, components, text_edit, tokens};
use egui::{Color32, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use serde_json::{Value, json};

use crate::a11y::Named;
use crate::canvas::{ACCENT, fmt_num};

const ROW_H: f32 = 22.0;
const INDENT: f32 = 14.0;
/// Width of the properties panel's label column.
const LABEL_W: f32 = 104.0;

// ---- layers -------------------------------------------------------------------------------------

#[derive(Default)]
pub struct LayersUi {
    renaming: Option<(NodeId, String)>,
    renaming_page: Option<(NodeId, String)>,
    /// Shift-click range anchor.
    anchor: Option<NodeId>,
    /// Rows being dragged.
    dragging: Option<Vec<NodeId>>,
    /// Selection last scrolled to.
    shown: Vec<NodeId>,
}

/// Where a drop in the tree would put the dragged rows.
#[derive(Debug, Clone, Copy)]
enum Drop {
    /// Above `row` in the panel = after it in paint order.
    Above(NodeId),
    Below(NodeId),
    Into(NodeId),
}

fn kind_icon(p: &egui::Painter, r: Rect, kind: &NodeKind, c: Color32) {
    let s = Stroke::new(1.2, c);
    let r = r.shrink(3.0);
    match kind {
        NodeKind::Frame { auto_layout, .. } => {
            p.line_segment([Pos2::new(r.left() + 3.0, r.top()), Pos2::new(r.left() + 3.0, r.bottom())], s);
            p.line_segment([Pos2::new(r.right() - 3.0, r.top()), Pos2::new(r.right() - 3.0, r.bottom())], s);
            p.line_segment([Pos2::new(r.left(), r.top() + 3.0), Pos2::new(r.right(), r.top() + 3.0)], s);
            p.line_segment([Pos2::new(r.left(), r.bottom() - 3.0), Pos2::new(r.right(), r.bottom() - 3.0)], s);
            if auto_layout.is_some() {
                p.circle_filled(r.center(), 1.5, c);
            }
        }
        NodeKind::Group {} => {
            for (a, b) in [(r.left_top(), r.right_top()), (r.right_top(), r.right_bottom()), (r.right_bottom(), r.left_bottom()), (r.left_bottom(), r.left_top())] {
                p.add(egui::Shape::dashed_line(&[a, b], s, 2.0, 2.0));
            }
        }
        NodeKind::Ellipse { .. } => {
            p.circle_stroke(r.center(), r.width() / 2.0, s);
        }
        NodeKind::Line {} => {
            p.line_segment([r.left_bottom(), r.right_top()], s);
        }
        NodeKind::Polygon { .. } => {
            p.add(egui::Shape::closed_line(vec![Pos2::new(r.center().x, r.top()), r.right_bottom(), r.left_bottom()], s));
        }
        NodeKind::Path { .. } => {
            p.add(egui::Shape::line(vec![r.left_bottom(), Pos2::new(r.left() + r.width() * 0.3, r.top()), Pos2::new(r.left() + r.width() * 0.7, r.bottom()), r.right_top()], s));
        }
        NodeKind::Text { .. } => {
            p.text(r.center(), egui::Align2::CENTER_CENTER, "T", egui::FontId::proportional(12.0), c);
        }
        NodeKind::Image { .. } => {
            p.rect_stroke(r, 1.0, s, egui::StrokeKind::Middle);
            p.add(egui::Shape::line(vec![r.left_bottom(), Pos2::new(r.center().x - 1.0, r.center().y), r.right_bottom()], s));
        }
        NodeKind::Component { .. } | NodeKind::Instance { .. } => {
            let pts = vec![Pos2::new(r.center().x, r.top()), Pos2::new(r.right(), r.center().y), Pos2::new(r.center().x, r.bottom()), Pos2::new(r.left(), r.center().y)];
            if matches!(kind, NodeKind::Component { .. }) {
                p.add(egui::Shape::convex_polygon(pts, c, Stroke::NONE));
            } else {
                p.add(egui::Shape::closed_line(pts, s));
            }
        }
        NodeKind::Note { .. } => {
            p.rect_filled(r, 1.0, c.gamma_multiply(0.5));
        }
        _ => {
            p.rect_stroke(r, 1.0, s, egui::StrokeKind::Middle);
        }
    }
}

impl LayersUi {
    pub fn show(&mut self, ui: &mut egui::Ui, s: &mut Session, cmds: &mut Vec<Command>) {
        self.pages(ui, s);
        ui.separator();
        ui.horizontal(|ui| {
            ui.strong("Layers");
        });
        self.tree(ui, s, cmds);
    }

    fn pages(&mut self, ui: &mut egui::Ui, s: &mut Session) {
        egui::CollapsingHeader::new("Pages").default_open(true).show(ui, |ui| {
            let pages = s.pages();
            for p in pages {
                let name = s.doc().get(p).map(|n| n.name.clone()).unwrap_or_default();
                if let Some((id, text)) = &mut self.renaming_page
                    && *id == p
                {
                    let r = ui.text_edit_singleline(text);
                    r.request_focus();
                    if r.lost_focus() {
                        let (id, text) = (*id, text.clone());
                        if ui.input(|i| !i.key_pressed(egui::Key::Escape)) {
                            let _ = s.transact(TxMeta::user("Rename page"), |tx| tx.update(id, doc::PropValue::Name(text)));
                        }
                        self.renaming_page = None;
                    }
                    continue;
                }
                let label = if name.is_empty() { "Page".to_string() } else { name.clone() };
                let r = ui.add(egui::Button::selectable(p == s.page(), label));
                if r.clicked() {
                    s.set_page(p);
                }
                if r.double_clicked() {
                    self.renaming_page = Some((p, name));
                }
            }
            if ui.button("+ Add page").clicked() {
                let n = s.pages().len() + 1;
                if let Ok(p) = s.transact(TxMeta::user("Add page"), |tx| tx.insert_json(None, None, json!({"type": "page", "name": format!("Page {n}")}))) {
                    s.set_page(p);
                }
            }
        });
    }

    fn tree(&mut self, ui: &mut egui::Ui, s: &mut Session, cmds: &mut Vec<Command>) {
        // Reveal and scroll to a selection made on the canvas.
        let sel: Vec<NodeId> = s.selection().to_vec();
        let mut scroll_to: Option<usize> = None;
        if sel != self.shown {
            self.shown = sel.clone();
            if let Some(&first) = sel.first() {
                s.reveal_layer(first);
                scroll_to = s.layer_rows().1.iter().position(|r| r.id == first);
            }
        }
        let (doc_len, rows): (usize, Vec<editor::Row>) = {
            let (_, rows) = s.layer_rows();
            (rows.len(), rows.to_vec())
        };
        let mut toggle_expand: Option<(NodeId, bool)> = None;
        let mut clicked: Option<(NodeId, egui::Modifiers)> = None;
        let mut drop_at: Option<Drop> = None;
        let mut start_drag: Option<NodeId> = None;
        let mut flags: Option<(NodeId, &'static str, bool)> = None;
        let pointer = ui.input(|i| i.pointer.interact_pos());
        let released = ui.input(|i| i.pointer.any_released());
        let mut area = egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("layers-tree");
        if let Some(i) = scroll_to {
            area = area.vertical_scroll_offset((i as f32 * ROW_H - 120.0).max(0.0));
        }
        area.show_rows(ui, ROW_H, doc_len, |ui, range| {
            let doc = s.doc();
            for row in &rows[range] {
                let Some(n) = doc.get(row.id) else { continue };
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::click_and_drag());
                let selected = s.selection().contains(&row.id);
                let p = ui.painter();
                if selected {
                    p.rect_filled(rect, 2.0, ACCENT.gamma_multiply(0.35));
                } else if resp.hovered() {
                    p.rect_filled(rect, 2.0, ui.visuals().widgets.hovered.weak_bg_fill);
                }
                let x0 = rect.left() + 4.0 + f32::from(row.depth) * INDENT;
                let text_col = if n.props.visible { ui.visuals().text_color() } else { ui.visuals().weak_text_color() };
                // Expand arrow.
                let arrow = Rect::from_min_size(Pos2::new(x0, rect.top()), Vec2::new(12.0, ROW_H));
                if row.has_children {
                    let c = arrow.center();
                    let pts = if row.expanded {
                        vec![c + Vec2::new(-3.5, -2.0), c + Vec2::new(3.5, -2.0), c + Vec2::new(0.0, 2.5)]
                    } else {
                        vec![c + Vec2::new(-2.0, -3.5), c + Vec2::new(2.5, 0.0), c + Vec2::new(-2.0, 3.5)]
                    };
                    p.add(egui::Shape::convex_polygon(pts, text_col, Stroke::NONE));
                }
                kind_icon(p, Rect::from_min_size(Pos2::new(x0 + 13.0, rect.top() + 3.0), Vec2::splat(ROW_H - 6.0)), &n.kind, text_col);
                // Visibility and lock toggles on the right.
                let eye = Rect::from_min_size(Pos2::new(rect.right() - 40.0, rect.top()), Vec2::new(18.0, ROW_H));
                let lock = Rect::from_min_size(Pos2::new(rect.right() - 20.0, rect.top()), Vec2::new(18.0, ROW_H));
                let show_flags = resp.hovered() || !n.props.visible || n.props.locked;
                if show_flags {
                    p.text(eye.center(), egui::Align2::CENTER_CENTER, if n.props.visible { "\u{25c9}" } else { "\u{25cb}" }, egui::FontId::proportional(12.0), text_col);
                    p.text(lock.center(), egui::Align2::CENTER_CENTER, if n.props.locked { "\u{1f512}" } else { "\u{b7}" }, egui::FontId::proportional(11.0), text_col);
                }
                let name_rect = Rect::from_min_max(Pos2::new(x0 + 32.0, rect.top()), Pos2::new(rect.right() - 44.0, rect.bottom()));
                let renaming = self.renaming.as_ref().is_some_and(|(id, _)| *id == row.id);
                if renaming {
                    if let Some((id, text)) = &mut self.renaming {
                        let r = ui.put(name_rect, egui::TextEdit::singleline(text));
                        r.request_focus();
                        if r.lost_focus() {
                            if !ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                let (id, text) = (*id, text.clone());
                                let _ = s.transact(TxMeta::user("Rename"), |tx| tx.update(id, doc::PropValue::Name(text)));
                            }
                            self.renaming = None;
                        }
                    }
                    return;
                }
                let name = if n.name.is_empty() { n.kind.type_name().to_string() } else { n.name.clone() };
                p.text(Pos2::new(name_rect.left(), rect.center().y), egui::Align2::LEFT_CENTER, name.clone(), egui::FontId::proportional(13.0), text_col);
                resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::TreeItem, true, selected, format!("{}, {}", name, n.kind.type_name())));
                // Interaction.
                if let Some(pos) = resp.interact_pointer_pos().or(pointer)
                    && resp.clicked()
                {
                    if row.has_children && arrow.contains(pos) {
                            toggle_expand = Some((row.id, !row.expanded));
                        } else if show_flags && eye.contains(pos) {
                            flags = Some((row.id, "visible", !n.props.visible));
                        } else if show_flags && lock.contains(pos) {
                            flags = Some((row.id, "locked", !n.props.locked));
                        } else {
                            clicked = Some((row.id, ui.input(|i| i.modifiers)));
                        }
                }
                if resp.double_clicked() && name_rect.contains(resp.interact_pointer_pos().unwrap_or_default()) {
                    self.renaming = Some((row.id, n.name.clone()));
                }
                if resp.drag_started() {
                    start_drag = Some(row.id);
                }
                // Drop indicator while dragging rows over this one.
                if self.dragging.is_some()
                    && let Some(pos) = pointer
                    && rect.contains(pos)
                {
                    let t = (pos.y - rect.top()) / ROW_H;
                    let can_hold = n.kind.is_container();
                    let d = if t < 0.3 || (!can_hold && t < 0.5) {
                        Drop::Above(row.id)
                    } else if t > 0.7 || !can_hold {
                        Drop::Below(row.id)
                    } else {
                        Drop::Into(row.id)
                    };
                    let stroke = Stroke::new(2.0, ACCENT);
                    match d {
                        Drop::Above(_) => {
                            p.line_segment([Pos2::new(x0, rect.top()), Pos2::new(rect.right(), rect.top())], stroke);
                        }
                        Drop::Below(_) => {
                            p.line_segment([Pos2::new(x0, rect.bottom()), Pos2::new(rect.right(), rect.bottom())], stroke);
                        }
                        Drop::Into(_) => {
                            p.rect_stroke(rect, 2.0, stroke, egui::StrokeKind::Inside);
                        }
                    }
                    drop_at = Some(d);
                }
            }
        });
        // Keyboard: arrows move through the rows while the panel is hovered and nothing else
        // has the keyboard.
        if ui.ui_contains_pointer() && crate::canvas::keyboard_free(ui.ctx()) {
            let (up, down) = ui.input_mut(|i| (i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp), i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown)));
            if (up || down)
                && let Some(&cur) = s.selection().first()
                && let Some(i) = rows.iter().position(|r| r.id == cur)
            {
                let j = if up { i.saturating_sub(1) } else { (i + 1).min(rows.len().saturating_sub(1)) };
                if let Some(r) = rows.get(j) {
                    s.select(vec![r.id]);
                }
            }
        }
        if let Some((id, on)) = toggle_expand {
            s.layers.set_expanded(id, on);
        }
        if let Some((id, key, on)) = flags {
            let v = if key == "visible" { doc::PropValue::Visible(on) } else { doc::PropValue::Locked(on) };
            let _ = s.transact(TxMeta::user(if key == "visible" { "Toggle visibility" } else { "Toggle lock" }), |tx| tx.update(id, v));
        }
        if let Some((id, m)) = clicked {
            if m.command {
                s.toggle(id);
                self.anchor = Some(id);
            } else if m.shift
                && let Some(a) = self.anchor
                && let (Some(i), Some(j)) = (rows.iter().position(|r| r.id == a), rows.iter().position(|r| r.id == id))
            {
                let (lo, hi) = (i.min(j), i.max(j));
                let doc = s.doc();
                let parent = editor::select::parent_of(doc, id);
                let ids: Vec<NodeId> = rows[lo..=hi].iter().map(|r| r.id).filter(|&r| editor::select::parent_of(doc, r) == parent).collect();
                s.select(ids);
            } else {
                s.select(vec![id]);
                self.anchor = Some(id);
            }
            self.shown = s.selection().to_vec();
        }
        if let Some(id) = start_drag {
            if !s.selection().contains(&id) {
                s.select(vec![id]);
            }
            self.dragging = Some(s.selection().to_vec());
        }
        if released
            && let Some(ids) = self.dragging.take()
            && let Some(d) = drop_at
        {
            drop_rows(s, &ids, d);
        }
        if s.selection().len() > 1 && ui.ui_contains_pointer() && ui.input(|i| i.key_pressed(egui::Key::Delete)) && crate::canvas::keyboard_free(ui.ctx()) {
            cmds.push(Command::Delete);
        }
    }
}

fn drop_rows(s: &mut Session, ids: &[NodeId], d: Drop) {
    let doc = s.doc();
    let (parent, anchor_after) = match d {
        Drop::Into(p) => (Some(p), None),
        Drop::Above(r) => (editor::select::parent_of(doc, r), Some((r, true))),
        Drop::Below(r) => (editor::select::parent_of(doc, r), Some((r, false))),
    };
    let Some(parent) = parent else { return };
    if ids.contains(&parent) {
        return;
    }
    // Slot among the parent's children without the dragged ones. Above a row (in the panel) =
    // just after it in paint order.
    let rest: Vec<NodeId> = doc.get(parent).map(|p| p.children.iter().filter_map(|&k| doc.node(k)).map(|n| n.id).filter(|c| !ids.contains(c)).collect()).unwrap_or_default();
    let slot = match anchor_after {
        None => rest.len(),
        Some((r, above)) => match rest.iter().position(|&c| c == r) {
            Some(i) => i + usize::from(above),
            None => return,
        },
    };
    let _ = commands::move_layers(s, ids, parent, slot);
}

// ---- properties ---------------------------------------------------------------------------------

/// Export settings (kept between exports) and a request from the panel's button.
#[derive(Clone)]
pub struct ExportSettings {
    pub format: export::Format,
    pub scale: f64,
    pub quality: u8,
    pub requested: bool,
}

impl Default for ExportSettings {
    fn default() -> Self {
        ExportSettings { format: export::Format::Png, scale: 1.0, quality: 90, requested: false }
    }
}

#[derive(Default)]
pub struct PropsUi {
    pub export: ExportSettings,
    /// Bumped at the end of each pointer interaction or text commit, so a drag (or a run of
    /// keystrokes) is one undo step and separate edits are separate steps.
    epoch: u64,
    /// Text being typed into a field (edits apply live; this holds the buffer while focused).
    editing: Option<(String, String)>,
}

fn title_case(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i == 0 {
            out.extend(c.to_uppercase());
        } else if c.is_uppercase() {
            out.push(' ');
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// A small menu button that binds `path` to a variable of type `ty`. Returns the new binding
/// (`Some(None)` = unbind) when one is picked.
fn bind_menu(ui: &mut egui::Ui, doc: &Document, ids: &[NodeId], path: &str, ty: VariableType) -> Option<Option<Id>> {
    let current = tokens::bound(doc, ids, path);
    let vars = tokens::variables_of(doc.variables(), ty);
    if vars.is_empty() && current.is_none() {
        return None;
    }
    let mut out = None;
    let r = ui.menu_button(if current.is_some() { "\u{25c8}" } else { "\u{25c7}" }, |ui| {
        if let Some(c) = current {
            ui.label(RichText::new(tokens::variable_name(doc.variables(), c)).strong());
            if ui.button("Unbind").clicked() {
                out = Some(None);
                ui.close();
            }
            ui.separator();
        }
        for (id, name) in &vars {
            if ui.selectable_label(current == Some(*id), name).clicked() {
                out = Some(Some(*id));
                ui.close();
            }
        }
    });
    let hover = current.map_or_else(|| "Bind to a variable".to_string(), |c| format!("Bound to {}", tokens::variable_name(doc.variables(), c)));
    r.response.tip(hover);
    out
}

fn color32(v: &Value) -> Color32 {
    let c = v.as_str().and_then(inspect::parse_color).unwrap_or([0, 0, 0, 255]);
    Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3])
}

fn color_json(c: Color32) -> Value {
    Value::String(inspect::format_color(c.to_srgba_unmultiplied()))
}

impl PropsUi {
    fn key(&self, path: &str) -> String {
        format!("panel:{path}:{}", self.epoch)
    }

    pub fn show(&mut self, ui: &mut egui::Ui, s: &mut Session, ctl: &mut Controller, on_input: &mut Option<Instant>) {
        if ui.input(|i| i.pointer.any_released() || i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Tab)) {
            self.epoch += 1;
        }
        let ids: Vec<NodeId> = s.selection().to_vec();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            if ids.is_empty() {
                self.page(ui, s);
                self.export_section(ui, "Export page");
                return;
            }
            self.header(ui, s, &ids);
            // While its text is edited on the canvas, a text's style fields show and set the
            // style of the selected characters (or of what is typed next).
            let range_style = ctl.text.as_ref().filter(|t| ids == [t.node]).and_then(|t| t.current_style(s));
            let mut edits: Vec<(Applies, &'static str, Value, bool)> = Vec::new();
            let mut binds: Vec<(String, Option<Id>)> = Vec::new();
            for section in inspect::SECTIONS {
                let doc = s.doc();
                if !ids.iter().filter_map(|&id| doc.get(id)).any(|n| inspect::applies(doc, n, section.applies)) {
                    continue;
                }
                egui::CollapsingHeader::new(section.title).default_open(true).show(ui, |ui| {
                    // A fixed label column keeps every section aligned and the panel from growing
                    // with its widest label; long labels truncate (full text on hover).
                    egui::Grid::new(section.title).num_columns(2).spacing([8.0, 4.0]).min_col_width(LABEL_W).show(ui, |ui| {
                        for f in section.fields {
                            let Some(mut shown) = inspect::read(s.doc(), &ids, section, f) else { continue };
                            if let Some(v) = range_style.as_ref().and_then(|st| text_edit::span_value(st, f.path)) {
                                shown = Shown::Value(v);
                            }
                            ui.allocate_ui_with_layout(egui::vec2(LABEL_W, 18.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                ui.set_width(LABEL_W);
                                ui.add(egui::Label::new(f.label).truncate());
                            });
                            let bindable = tokens::bindable(f.path);
                            let bound = bindable.as_ref().and_then(|(p, _)| tokens::bound(s.doc(), &ids, p));
                            ui.horizontal(|ui| {
                                if let Some(v) = bound {
                                    // A bound field shows its variable; unbind to edit the value.
                                    ui.label(RichText::new(tokens::variable_name(s.doc().variables(), v)).background_color(ui.visuals().faint_bg_color));
                                } else if let Some((v, drag)) = self.field(ui, f, &shown) {
                                    edits.push((section.applies, f.path, v, drag));
                                }
                                if let Some((path, ty)) = &bindable
                                    && let Some(b) = bind_menu(ui, s.doc(), &ids, path, *ty)
                                {
                                    binds.push((path.clone(), b));
                                }
                            });
                            ui.end_row();
                        }
                    });
                    if section.applies == Applies::Frames {
                        let has = {
                            let doc = s.doc();
                            ids.iter().filter_map(|&id| doc.get(id)).any(|n| matches!(n.kind, NodeKind::Frame { auto_layout: Some(_), .. } | NodeKind::Component { auto_layout: Some(_), .. }))
                        };
                        let label = if has { "Remove auto layout" } else { "Add auto layout" };
                        if ui.button(label).clicked() {
                            let _ = inspect::set_auto_layout(s, &ids, !has);
                        }
                    }
                });
            }
            for (applies, path, v, drag) in edits {
                on_input.get_or_insert_with(Instant::now);
                let key = drag.then(|| self.key(path));
                if let Some(t) = ctl.text.as_mut().filter(|t| ids == [t.node])
                    && text_edit::SPAN_FIELDS.contains(&path)
                {
                    let label = format!("Set {}", path.trim_start_matches("style.").to_lowercase());
                    let _ = t.apply_style(s, &label, key.as_deref(), |sp| {
                        text_edit::set_span_field(sp, path, &v);
                    });
                    continue;
                }
                let _ = inspect::write(s, &ids, applies, path, v, key.as_deref());
            }
            for (path, b) in binds {
                on_input.get_or_insert_with(Instant::now);
                let _ = tokens::bind(s, &ids, &path, b);
            }
            self.paints(ui, s, &ids, "fills", "Fills", on_input);
            self.strokes(ui, s, &ids, on_input);
            self.effects(ui, s, &ids, on_input);
            self.info(ui, s, &ids);
            self.export_section(ui, "Export");
        });
    }

    fn export_section(&mut self, ui: &mut egui::Ui, title: &str) {
        egui::CollapsingHeader::new(title).default_open(false).show(ui, |ui| {
            let e = &mut self.export;
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("export-format").selected_text(e.format.extension().to_uppercase()).show_ui(ui, |ui| {
                    for f in export::Format::ALL {
                        ui.selectable_value(&mut e.format, f, f.extension().to_uppercase());
                    }
                }).response.named("Export format");
                let unit = if e.format == export::Format::Pdf { " pt/unit" } else { "\u{d7}" };
                egui::ComboBox::from_id_salt("export-scale").selected_text(format!("{}{unit}", e.scale)).show_ui(ui, |ui| {
                    for v in [0.5, 1.0, 2.0, 3.0, 4.0] {
                        ui.selectable_value(&mut e.scale, v, format!("{v}{unit}"));
                    }
                }).response.named("Export scale");
            });
            if e.format == export::Format::Jpeg {
                ui.add(egui::Slider::new(&mut e.quality, 1..=100).text("Quality"));
            }
            if ui.button("Export\u{2026}").tip("Ctrl+Shift+E").clicked() {
                e.requested = true;
            }
        });
    }

    fn page(&mut self, ui: &mut egui::Ui, s: &mut Session) {
        ui.heading("Page");
        let page = s.page();
        let bg = s.doc().get(page).map(inspect::node_json).and_then(|j| j.get("background").cloned()).unwrap_or(json!("#f5f5f5"));
        let mut c = color32(&bg);
        ui.horizontal(|ui| {
            ui.label("Background");
            if ui.color_edit_button_srgba(&mut c).named("Page background").changed() {
                let key = self.key("background");
                let mut m = serde_json::Map::new();
                m.insert("background".into(), color_json(c));
                let _ = s.transact(TxMeta::user("Set page background").coalesce(key), |tx| tx.update_json(page, &m));
            }
        });
        ui.add_space(8.0);
        ui.label(RichText::new("Select a layer to edit its properties.").weak());
    }

    fn header(&mut self, ui: &mut egui::Ui, s: &mut Session, ids: &[NodeId]) {
        let doc = s.doc();
        match ids {
            [one] => {
                let Some(n) = doc.get(*one) else { return };
                let kind = n.kind.type_name();
                let mut name = n.name.clone();
                ui.horizontal(|ui| {
                    ui.label(RichText::new(title_case(kind)).weak());
                    let r = ui.add(egui::TextEdit::singleline(&mut name).desired_width(f32::INFINITY));
                    if r.changed() {
                        let id = *one;
                        let key = self.key("name");
                        let _ = s.transact(TxMeta::user("Rename").coalesce(key), |tx| tx.update(id, doc::PropValue::Name(name.clone())));
                    }
                });
            }
            many => {
                ui.label(format!("{} layers", many.len()));
            }
        }
        ui.separator();
    }

    /// One field's widget. Returns a new value and whether it came from a drag.
    fn field(&mut self, ui: &mut egui::Ui, f: &Field, shown: &Shown) -> Option<(Value, bool)> {
        let mixed = matches!(shown, Shown::Mixed);
        let v = match shown {
            Shown::Value(v) => v.clone(),
            Shown::Mixed => Value::Null,
        };
        match f.widget {
            Widget::Number { min, max, speed, unit, decimals } => {
                let auto = v.is_null() && !mixed;
                let mut x = v.as_f64().unwrap_or(0.0);
                let mut dv = egui::DragValue::new(&mut x).range(min..=max).speed(speed).max_decimals(decimals);
                if !unit.is_empty() && unit != "px" {
                    dv = dv.suffix(unit);
                }
                if mixed {
                    dv = dv.custom_formatter(|_, _| "Mixed".into());
                } else if auto {
                    dv = dv.custom_formatter(|_, _| "Auto".into());
                }
                let r = ui.add(dv).named(f.label);
                if r.changed() {
                    let drag = r.dragged();
                    return Some((json!(x), drag));
                }
                if auto && r.secondary_clicked() {
                    return None;
                }
                None
            }
            Widget::Toggle => {
                let mut b = v.as_bool().unwrap_or(false);
                let r = ui.add(egui::Checkbox::new(&mut b, "").indeterminate(mixed)).named(f.label);
                r.changed().then(|| (json!(b), false))
            }
            Widget::Choice(values) => {
                let current = if mixed { "Mixed".to_string() } else { title_case(v.as_str().unwrap_or("")) };
                let mut out = None;
                egui::ComboBox::from_id_salt(f.path).selected_text(current).show_ui(ui, |ui| {
                    for &c in values {
                        if ui.selectable_label(v.as_str() == Some(c), title_case(c)).clicked() {
                            out = Some((json!(c), false));
                        }
                    }
                }).response.named(f.label);
                out
            }
            Widget::Color => {
                let mut c = color32(&v);
                let r = ui.color_edit_button_srgba(&mut c).named(f.label);
                r.changed().then(|| (color_json(c), true))
            }
            Widget::Text { multiline } => {
                let mut text = if mixed { String::new() } else { v.as_str().unwrap_or("").to_string() };
                let edit = if multiline { egui::TextEdit::multiline(&mut text).desired_rows(3) } else { egui::TextEdit::singleline(&mut text) };
                let r = ui.add(edit.hint_text(if mixed { "Mixed" } else { "" }).desired_width(f32::INFINITY)).named(f.label);
                if r.changed() {
                    self.editing = Some((f.path.to_string(), text.clone()));
                    return Some((json!(text), true));
                }
                None
            }
        }
    }

    fn list_header(ui: &mut egui::Ui, title: &str) -> bool {
        let mut add = false;
        ui.horizontal(|ui| {
            ui.strong(title);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                add = ui.small_button("+").tip(format!("Add to {}", title.to_lowercase())).clicked();
            });
        });
        add
    }

    /// Shared list or `None` (mixed: offer to replace).
    fn read_list(ui: &mut egui::Ui, s: &Session, ids: &[NodeId], key: &str) -> Option<Vec<Value>> {
        match inspect::read_list(s.doc(), ids, key) {
            Some(Shown::Value(Value::Array(a))) => Some(a),
            Some(Shown::Value(_)) | None => Some(Vec::new()),
            Some(Shown::Mixed) => {
                ui.label(RichText::new("Mixed").weak());
                None
            }
        }
    }

    fn write_list(&self, s: &mut Session, ids: &[NodeId], key: &'static str, list: Vec<Value>, drag: bool, on_input: &mut Option<Instant>) {
        on_input.get_or_insert_with(Instant::now);
        let k = drag.then(|| self.key(key));
        let _ = inspect::write(s, ids, Applies::All, key, Value::Array(list), k.as_deref());
    }

    fn paint_editor(ui: &mut egui::Ui, p: &mut Value, salt: &str) -> (bool, bool) {
        let (mut changed, mut drag) = (false, false);
        let ty = p.get("type").and_then(Value::as_str).unwrap_or("solid").to_string();
        let mut visible = p.get("visible").and_then(Value::as_bool).unwrap_or(true);
        if ui.checkbox(&mut visible, "").tip("Visible").changed() {
            p["visible"] = json!(visible);
            changed = true;
        }
        let mut new_ty = ty.clone();
        egui::ComboBox::from_id_salt(format!("{salt}-type")).width(90.0).selected_text(title_case(&ty)).show_ui(ui, |ui| {
            for t in ["solid", "linearGradient", "radialGradient", "angularGradient"] {
                ui.selectable_value(&mut new_ty, t.to_string(), title_case(t));
            }
        }).response.named("Paint type");
        if new_ty != ty {
            let first = p.get("color").cloned().or_else(|| p.get("stops").and_then(|s| s.get(0)).and_then(|s| s.get("color")).cloned()).unwrap_or(json!("#d9d9d9"));
            let stops = json!([{"offset": 0, "color": first}, {"offset": 1, "color": "#ffffff"}]);
            let mut q = match new_ty.as_str() {
                "solid" => json!({"type": "solid", "color": first}),
                "linearGradient" => json!({"type": "linearGradient", "stops": stops, "start": [0, 0.5], "end": [1, 0.5]}),
                "radialGradient" => json!({"type": "radialGradient", "stops": stops, "center": [0.5, 0.5], "radius": [0.5, 0.5]}),
                _ => json!({"type": "angularGradient", "stops": stops, "center": [0.5, 0.5]}),
            };
            for k in ["opacity", "visible", "blend"] {
                if let Some(v) = p.get(k) {
                    q[k] = v.clone();
                }
            }
            *p = q;
            return (true, false);
        }
        match ty.as_str() {
            "solid" => {
                let mut c = color32(p.get("color").unwrap_or(&Value::Null));
                if ui.color_edit_button_srgba(&mut c).named("Colour").changed() {
                    p["color"] = color_json(c);
                    (changed, drag) = (true, true);
                }
            }
            "image" => {
                let fit = p.get("fit").and_then(Value::as_str).unwrap_or("fill").to_string();
                let mut new_fit = fit.clone();
                egui::ComboBox::from_id_salt(format!("{salt}-fit")).width(70.0).selected_text(title_case(&fit)).show_ui(ui, |ui| {
                    for f in ["fill", "fit", "crop", "tile"] {
                        ui.selectable_value(&mut new_fit, f.to_string(), title_case(f));
                    }
                }).response.named("Image fit");
                if new_fit != fit {
                    p["fit"] = json!(new_fit);
                    changed = true;
                }
            }
            _ => {
                if let Some(stops) = p.get_mut("stops").and_then(Value::as_array_mut) {
                    for st in stops.iter_mut() {
                        let mut c = color32(st.get("color").unwrap_or(&Value::Null));
                        if ui.color_edit_button_srgba(&mut c).named("Stop colour").changed() {
                            st["color"] = color_json(c);
                            (changed, drag) = (true, true);
                        }
                    }
                    if ui.small_button("+").tip("Add stop").clicked() {
                        stops.push(json!({"offset": 1, "color": "#000000"}));
                        changed = true;
                    }
                }
            }
        }
        let mut op = p.get("opacity").and_then(Value::as_f64).unwrap_or(1.0) * 100.0;
        let r = ui.add(egui::DragValue::new(&mut op).range(0.0..=100.0).suffix("%").speed(1.0)).named("Opacity");
        if r.changed() {
            p["opacity"] = json!(op / 100.0);
            changed = true;
            drag |= r.dragged();
        }
        (changed, drag)
    }

    fn paints(&mut self, ui: &mut egui::Ui, s: &mut Session, ids: &[NodeId], key: &'static str, title: &str, on_input: &mut Option<Instant>) {
        ui.separator();
        let add = Self::list_header(ui, title);
        let Some(mut list) = Self::read_list(ui, s, ids, key) else {
            if add {
                self.write_list(s, ids, key, vec![json!({"type": "solid", "color": "#d9d9d9"})], false, on_input);
            }
            return;
        };
        let (mut changed, mut drag) = (add, false);
        if add {
            list.push(json!({"type": "solid", "color": "#d9d9d9"}));
        }
        let mut remove = None;
        let mut bind = None;
        // Topmost paint first, as it stacks.
        for i in (0..list.len()).rev() {
            ui.horizontal(|ui| {
                let (c, d) = Self::paint_editor(ui, &mut list[i], &format!("{key}{i}"));
                changed |= c;
                drag |= d;
                if list[i].get("type").and_then(Value::as_str) == Some("solid") {
                    let path = format!("{key}/{i}/color");
                    if let Some(b) = bind_menu(ui, s.doc(), ids, &path, VariableType::Color) {
                        bind = Some((path, b));
                    }
                }
                if ui.small_button("\u{2212}").tip("Remove").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some((path, b)) = bind {
            on_input.get_or_insert_with(Instant::now);
            let _ = tokens::bind(s, ids, &path, b);
            return;
        }
        if let Some(i) = remove {
            list.remove(i);
            changed = true;
        }
        if changed {
            self.write_list(s, ids, key, list, drag, on_input);
        }
    }

    fn strokes(&mut self, ui: &mut egui::Ui, s: &mut Session, ids: &[NodeId], on_input: &mut Option<Instant>) {
        ui.separator();
        let add = Self::list_header(ui, "Strokes");
        let new = json!({"paint": {"type": "solid", "color": "#000000"}, "weight": 1});
        let Some(mut list) = Self::read_list(ui, s, ids, "strokes") else {
            if add {
                self.write_list(s, ids, "strokes", vec![new], false, on_input);
            }
            return;
        };
        let (mut changed, mut drag) = (add, false);
        if add {
            list.push(new);
        }
        let mut remove = None;
        for (i, st) in list.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                let mut paint = st.get("paint").cloned().unwrap_or(json!({"type": "solid", "color": "#000000"}));
                let (c, d) = Self::paint_editor(ui, &mut paint, &format!("stroke{i}"));
                if c {
                    st["paint"] = paint;
                    changed = true;
                    drag |= d;
                }
                if ui.small_button("\u{2212}").tip("Remove").clicked() {
                    remove = Some(i);
                }
            });
            ui.horizontal(|ui| {
                let mut w = st.get("weight").and_then(Value::as_f64).unwrap_or(1.0);
                let r = ui.add(egui::DragValue::new(&mut w).range(0.0..=1000.0).speed(0.1).prefix("Weight ")).named("Stroke weight");
                if r.changed() {
                    st["weight"] = json!(w);
                    changed = true;
                    drag |= r.dragged();
                }
                for (k, values, default) in [("align", &["inside", "center", "outside"][..], "inside"), ("cap", &["butt", "round", "square"][..], "butt"), ("join", &["miter", "round", "bevel"][..], "miter")] {
                    let cur = st.get(k).and_then(Value::as_str).unwrap_or(default).to_string();
                    let mut next = cur.clone();
                    egui::ComboBox::from_id_salt(format!("stroke{i}{k}")).width(64.0).selected_text(title_case(&cur)).show_ui(ui, |ui| {
                        for v in values {
                            ui.selectable_value(&mut next, (*v).to_string(), title_case(v));
                        }
                    }).response.named(title_case(k));
                    if next != cur {
                        st[k] = json!(next);
                        changed = true;
                    }
                }
            });
            let mut dash = st.get("dash").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_f64).map(fmt_num).collect::<Vec<_>>().join(" ")).unwrap_or_default();
            ui.horizontal(|ui| {
                ui.label("Dash");
                if ui.add(egui::TextEdit::singleline(&mut dash).hint_text("e.g. 4 2").desired_width(80.0)).changed() {
                    let v: Vec<f64> = dash.split([' ', ',']).filter_map(|t| t.trim().parse().ok()).collect();
                    st["dash"] = json!(v);
                    changed = true;
                }
            });
        }
        if let Some(i) = remove {
            list.remove(i);
            changed = true;
        }
        if changed {
            self.write_list(s, ids, "strokes", list, drag, on_input);
        }
    }

    fn effects(&mut self, ui: &mut egui::Ui, s: &mut Session, ids: &[NodeId], on_input: &mut Option<Instant>) {
        ui.separator();
        let add = Self::list_header(ui, "Effects");
        let new = json!({"type": "dropShadow", "color": "#00000040", "offset": [0, 4], "blur": 8});
        let Some(mut list) = Self::read_list(ui, s, ids, "effects") else {
            if add {
                self.write_list(s, ids, "effects", vec![new], false, on_input);
            }
            return;
        };
        let (mut changed, mut drag) = (add, false);
        if add {
            list.push(new);
        }
        let mut remove = None;
        for (i, e) in list.iter_mut().enumerate() {
            let ty = e.get("type").and_then(Value::as_str).unwrap_or("dropShadow").to_string();
            ui.horizontal(|ui| {
                let mut visible = e.get("visible").and_then(Value::as_bool).unwrap_or(true);
                if ui.checkbox(&mut visible, "").tip("Visible").changed() {
                    e["visible"] = json!(visible);
                    changed = true;
                }
                let mut next = ty.clone();
                egui::ComboBox::from_id_salt(format!("effect{i}")).width(110.0).selected_text(title_case(&ty)).show_ui(ui, |ui| {
                    for t in ["dropShadow", "innerShadow", "layerBlur", "backgroundBlur"] {
                        ui.selectable_value(&mut next, t.to_string(), title_case(t));
                    }
                }).response.named("Effect type");
                if next != ty {
                    *e = match next.as_str() {
                        "dropShadow" | "innerShadow" => json!({"type": next, "color": "#00000040", "offset": [0, 4], "blur": 8}),
                        _ => json!({"type": next, "radius": 8}),
                    };
                    changed = true;
                }
                if ui.small_button("\u{2212}").tip("Remove").clicked() {
                    remove = Some(i);
                }
            });
            ui.horizontal(|ui| {
                // Returns Some(dragged) when the value changed.
                let num = |ui: &mut egui::Ui, e: &mut Value, label: &str, path: &[&str], min: f64| -> Option<bool> {
                    let mut x = path
                        .iter()
                        .try_fold(&*e, |v, k| match k.parse::<usize>() {
                            Ok(i) => v.get(i),
                            Err(_) => v.get(*k),
                        })
                        .and_then(Value::as_f64)
                        .unwrap_or(0.0);
                    let r = ui.add(egui::DragValue::new(&mut x).range(min..=10_000.0).speed(0.5).prefix(label)).named(label.trim());
                    if !r.changed() {
                        return None;
                    }
                    match path {
                        [k] => e[*k] = json!(x),
                        [k, i] => {
                            let i: usize = i.parse().unwrap_or(0);
                            if !e[*k].is_array() {
                                e[*k] = json!([0, 0]);
                            }
                            e[*k][i] = json!(x);
                        }
                        _ => {}
                    }
                    Some(r.dragged())
                };
                let mut hits: Vec<Option<bool>> = Vec::new();
                if ty.ends_with("Shadow") {
                    let mut c = color32(e.get("color").unwrap_or(&Value::Null));
                    if ui.color_edit_button_srgba(&mut c).named("Shadow colour").changed() {
                        e["color"] = color_json(c);
                        changed = true;
                        drag = true;
                    }
                    hits.push(num(ui, e, "X ", &["offset", "0"], -10_000.0));
                    hits.push(num(ui, e, "Y ", &["offset", "1"], -10_000.0));
                    hits.push(num(ui, e, "Blur ", &["blur"], 0.0));
                    hits.push(num(ui, e, "Spread ", &["spread"], -10_000.0));
                } else {
                    hits.push(num(ui, e, "Radius ", &["radius"], 0.0));
                }
                for d in hits.into_iter().flatten() {
                    changed = true;
                    drag |= d;
                }
            });
        }
        if let Some(i) = remove {
            list.remove(i);
            changed = true;
        }
        if changed {
            self.write_list(s, ids, "effects", list, drag, on_input);
        }
    }

    fn info(&mut self, ui: &mut egui::Ui, s: &mut Session, ids: &[NodeId]) {
        let [one] = ids else { return };
        let one = *one;
        let Some(n) = s.doc().get(one).cloned() else { return };
        let mut run: Option<Command> = None;
        match &n.kind {
            NodeKind::Component { description, .. } => {
                ui.separator();
                ui.strong("Component");
                let mut d = description.clone();
                if ui.add(egui::TextEdit::multiline(&mut d).hint_text("Description").desired_rows(2).desired_width(f32::INFINITY)).changed() {
                    let key = self.key("description");
                    let mut m = serde_json::Map::new();
                    m.insert("description".into(), json!(d));
                    let _ = s.transact(TxMeta::user("Set description").coalesce(key), |tx| tx.update_json(one, &m));
                }
                let doc = s.doc();
                let uses: Vec<NodeId> = doc.instances_of(one).filter_map(|k| doc.node(k)).filter(|i| editor::select::page_of(doc, i.id) == Some(s.page())).map(|i| i.id).collect();
                let all = doc.instances_of(one).count();
                ui.horizontal(|ui| {
                    ui.label(format!("{all} instance(s)"));
                    if ui.add_enabled(!uses.is_empty(), egui::Button::new("Select on page")).clicked() {
                        s.select(uses.clone());
                    }
                    if ui.button("Place instance").clicked() {
                        let _ = components::insert_instance(s, one, None);
                    }
                });
            }
            NodeKind::Instance { component, overrides } => {
                ui.separator();
                ui.strong("Instance");
                let main = s.doc().get(*component).map_or_else(|| "missing".to_string(), |c| c.name.clone());
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Main").weak());
                    if ui.link(main).tip("Go to main component").clicked() {
                        run = Some(Command::GoToMainComponent);
                    }
                });
                ui.horizontal(|ui| {
                    let real = n.virt.is_none();
                    if real && ui.add_enabled(!overrides.is_empty(), egui::Button::new(format!("Reset overrides ({})", overrides.len()))).clicked() {
                        run = Some(Command::ResetOverrides);
                    }
                    if real && ui.button("Detach").tip("Ctrl+Alt+B").clicked() {
                        run = Some(Command::DetachInstance);
                    }
                });
            }
            _ => {}
        }
        // Inside an instance: which properties this node overrides, each resettable.
        if let Some((instance, path)) = doc::components::override_target(s.doc(), one) {
            ui.separator();
            let inst_name = s.doc().get(instance).map_or_else(String::new, |i| i.name.clone());
            ui.strong(format!("In instance {inst_name}"));
            let props: Vec<doc::PropKey> = match s.doc().get(instance).map(|i| &i.kind) {
                Some(NodeKind::Instance { overrides, .. }) => overrides.iter().filter(|o| o.target == path).map(|o| o.value.key()).collect(),
                _ => Vec::new(),
            };
            if props.is_empty() {
                ui.label(RichText::new("No overrides. Edits here override the component for this instance.").weak());
            }
            for k in props {
                ui.horizontal(|ui| {
                    ui.label(format!("{} overridden", title_case(&k.as_str())));
                    if ui.small_button("Reset").clicked() {
                        let _ = s.transact(TxMeta::user("Reset override"), |tx| tx.reset_overrides(one, Some(k)));
                    }
                });
            }
        }
        // Modes, for nodes that choose them.
        if n.kind.takes_modes() && n.virt.is_none() && !s.doc().variables().collections.iter().all(|c| c.modes.len() < 2) {
            ui.separator();
            ui.strong("Modes");
            let collections = s.doc().variables().collections.clone();
            for c in collections.iter().filter(|c| c.modes.len() > 1) {
                let chosen = n.props.modes.get(&c.id).copied();
                let shown = chosen.and_then(|m| c.modes.iter().find(|x| x.id == m)).map_or_else(|| "Auto".to_string(), |m| m.name.clone());
                let mut pick: Option<Option<Id>> = None;
                ui.horizontal(|ui| {
                    ui.label(&c.name);
                    egui::ComboBox::from_id_salt(("mode", c.id.0)).selected_text(shown).show_ui(ui, |ui| {
                        if ui.selectable_label(chosen.is_none(), "Auto (inherited)").clicked() {
                            pick = Some(None);
                        }
                        for m in &c.modes {
                            if ui.selectable_label(chosen == Some(m.id), &m.name).clicked() {
                                pick = Some(Some(m.id));
                            }
                        }
                    }).response.named(format!("Mode for {}", c.name));
                });
                if let Some(m) = pick {
                    let _ = tokens::set_mode(s, &[one], c.id, m);
                }
            }
        }
        if !n.props.bindings.is_empty() {
            ui.separator();
            ui.strong("Variables");
            for (path, var) in &n.props.bindings {
                let name = tokens::variable_name(s.doc().variables(), *var);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(path).weak());
                    ui.label(RichText::new(name).background_color(ui.visuals().faint_bg_color));
                    if ui.small_button("\u{d7}").tip("Unbind").clicked() {
                        let _ = tokens::bind(s, &[one], path, None);
                    }
                });
            }
        }
        if let Some(c) = run {
            let _ = commands::execute(s, c);
        }
    }
}
