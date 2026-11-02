//! The Variables and Assets tabs of the left panel. Variables: collections with modes as
//! columns and variables as rows, edited in place. Assets: the document's components; a click
//! places an instance in the middle of the view.

use crate::a11y::Named;
use doc::variables::{VariableType, VariableValue};
use doc::{Id, NodeId, NodeKind};
use editor::{Session, components, tokens};
use egui::{Color32, RichText};

#[derive(Default)]
pub struct VariablesUi {
    search: String,
    /// Bumped when a drag or edit ends, so one drag is one undo step.
    epoch: u64,
    status: Option<String>,
}

fn color32(s: &str) -> Color32 {
    let c = editor::inspect::parse_color(s).unwrap_or([0, 0, 0, 255]);
    Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3])
}

fn type_label(t: VariableType) -> &'static str {
    match t {
        VariableType::Color => "Colour",
        VariableType::Number => "Number",
        VariableType::String => "Text",
        VariableType::Boolean => "Toggle",
    }
}

impl VariablesUi {
    pub fn show(&mut self, ui: &mut egui::Ui, s: &mut Session) {
        if ui.input(|i| i.pointer.any_released() || i.key_pressed(egui::Key::Enter)) {
            self.epoch += 1;
        }
        let mut result: Result<(), editor::Error> = Ok(());
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.search).hint_text("Search variables").desired_width(f32::INFINITY));
        });
        if ui.button("+ Collection").clicked() {
            let n = s.doc().variables().collections.len() + 1;
            result = tokens::add_collection(s, &format!("Collection {n}"));
        }
        let vars = s.doc().variables().clone();
        let needle = self.search.to_lowercase();
        egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
            for c in &vars.collections {
                ui.separator();
                let mut name = c.name.clone();
                ui.horizontal(|ui| {
                    if ui.add(egui::TextEdit::singleline(&mut name).desired_width(140.0).font(egui::TextStyle::Heading)).lost_focus() && name != c.name {
                        result = tokens::rename_collection(s, c.id, &name);
                    }
                    if ui.small_button("\u{2212}").tip("Delete collection").clicked() {
                        result = tokens::delete_collection(s, c.id);
                    }
                });
                egui::Grid::new(("vars", c.id.0)).striped(true).spacing([6.0, 4.0]).show(ui, |ui| {
                    ui.label(RichText::new("Name").weak());
                    for m in &c.modes {
                        let mut mn = m.name.clone();
                        ui.horizontal(|ui| {
                            let r = ui.add(egui::TextEdit::singleline(&mut mn).desired_width(70.0));
                            if r.lost_focus() && mn != m.name {
                                result = tokens::rename_mode(s, c.id, m.id, &mn);
                            }
                            if m.id == c.default_mode {
                                r.tip("Default mode");
                            }
                            if c.modes.len() > 1 && ui.small_button("\u{2212}").tip("Delete mode").clicked() {
                                result = tokens::delete_mode(s, c.id, m.id);
                            }
                        });
                    }
                    if ui.small_button("+").tip("Add mode").clicked() {
                        result = tokens::add_mode(s, c.id, &format!("Mode {}", c.modes.len() + 1));
                    }
                    ui.end_row();
                    for v in c.variables.iter().filter(|v| needle.is_empty() || v.name.to_lowercase().contains(&needle)) {
                        let mut vn = v.name.clone();
                        ui.horizontal(|ui| {
                            if ui.add(egui::TextEdit::singleline(&mut vn).desired_width(100.0)).tip(type_label(v.ty)).lost_focus() && vn != v.name {
                                result = tokens::rename_variable(s, v.id, &vn);
                            }
                        });
                        for m in &c.modes {
                            let value = v.values.get(&m.id).or_else(|| v.values.get(&c.default_mode)).cloned().unwrap_or_else(|| tokens::default_value(v.ty));
                            if let Some((new, drag)) = self.value_cell(ui, &vars, v.ty, &value, (v.id, m.id)) {
                                let key = drag.then(|| format!("var:{}:{}:{}", v.id, m.id, self.epoch));
                                result = tokens::set_value(s, v.id, m.id, new, key.as_deref());
                            }
                        }
                        if ui.small_button("\u{2212}").tip("Delete variable").clicked() {
                            result = tokens::delete_variable(s, v.id);
                        }
                        ui.end_row();
                    }
                });
                ui.horizontal(|ui| {
                    for ty in [VariableType::Color, VariableType::Number, VariableType::String, VariableType::Boolean] {
                        if ui.small_button(format!("+ {}", type_label(ty))).clicked() {
                            let n = c.variables.len() + 1;
                            result = tokens::add_variable(s, c.id, &format!("{}-{n}", type_label(ty).to_lowercase()), ty);
                        }
                    }
                });
            }
            if vars.collections.is_empty() {
                ui.label(RichText::new("No variables yet. Add a collection, then variables and modes (themes).").weak());
            }
        });
        if let Err(e) = result {
            self.status = Some(e.to_string());
        }
        if let Some(st) = &self.status {
            ui.label(RichText::new(st).color(ui.visuals().warn_fg_color));
        }
    }

    /// One value cell. Aliases show the target's name (read-only here).
    fn value_cell(&self, ui: &mut egui::Ui, vars: &doc::Variables, ty: VariableType, value: &VariableValue, salt: (Id, Id)) -> Option<(VariableValue, bool)> {
        match (value, ty) {
            (VariableValue::Alias { alias }, _) => {
                ui.label(RichText::new(format!("\u{2192} {}", tokens::variable_name(vars, *alias))).weak());
                None
            }
            (VariableValue::String(h), VariableType::Color) => {
                let mut c = color32(h);
                let r = ui.color_edit_button_srgba(&mut c).named("Value");
                r.changed().then(|| (VariableValue::String(editor::inspect::format_color(c.to_srgba_unmultiplied())), true))
            }
            (VariableValue::Number(n), _) => {
                let mut x = *n;
                let r = ui.add(egui::DragValue::new(&mut x).speed(0.5)).named("Value");
                r.changed().then(|| (VariableValue::Number(x), r.dragged()))
            }
            (VariableValue::Boolean(b), _) => {
                let mut x = *b;
                ui.checkbox(&mut x, "").named("Value").changed().then_some((VariableValue::Boolean(x), false))
            }
            (VariableValue::String(t), _) => {
                let mut x = t.clone();
                let r = ui.push_id(salt.0.0 ^ salt.1.0, |ui| ui.add(egui::TextEdit::singleline(&mut x).desired_width(90.0))).inner;
                r.changed().then_some((VariableValue::String(x), true))
            }
        }
    }
}

/// A component in the Assets list: id, name and how many instances use it.
type Asset = (NodeId, String, usize);

/// The Assets tab: every component in the document, by page.
pub fn assets(ui: &mut egui::Ui, s: &mut Session) {
    let doc = s.doc();
    let mut rows: Vec<(String, Vec<Asset>)> = Vec::new();
    for &p in doc.pages() {
        let Some(page) = doc.node(p) else { continue };
        let comps: Vec<Asset> = doc
            .descendants(p)
            .into_iter()
            .filter_map(|k| doc.node(k))
            .filter(|n| n.virt.is_none() && matches!(n.kind, NodeKind::Component { .. }))
            .map(|n| (n.id, if n.name.is_empty() { "Component".into() } else { n.name.clone() }, doc.instances_of(n.id).count()))
            .collect();
        if !comps.is_empty() {
            rows.push((page.name.clone(), comps));
        }
    }
    let mut insert = None;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        if rows.is_empty() {
            ui.label(RichText::new("No components yet. Select layers and press Ctrl+Alt+K.").weak());
        }
        for (page, comps) in &rows {
            ui.label(RichText::new(if page.is_empty() { "Page" } else { page }).weak());
            for (id, name, uses) in comps {
                let r = ui.button(format!("\u{25c6} {name}")).tip(format!("{uses} instance(s). Click to place one in the view."));
                if r.clicked() {
                    insert = Some(*id);
                }
            }
        }
    });
    if let Some(c) = insert {
        let _ = components::insert_instance(s, c, None);
    }
}
