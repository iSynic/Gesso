//! The properties panel's specification: sections and fields, each named by its JSON path in
//! the file format (`x`, `autoLayout.gap`, `style.fontSize`, `layoutChild.grow`). A panel (the
//! desktop's egui one, a later web one, Stage 6's agent schema) is generated from this table;
//! reading and writing go through the same JSON the format and the agent API use, so the panel
//! can't drift from them.
//!
//! Reading several nodes yields [`Shown::Mixed`] where they differ. Writing applies to all of
//! them in one transaction; drags pass a coalesce key so a whole drag is one undo step.

use doc::{Document, Node, NodeId, NodeKind, TxMeta};
use serde_json::{Map, Value, json};

use crate::session::{Error, Session};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Widget {
    Number { min: f64, max: f64, speed: f64, unit: &'static str, decimals: usize },
    Toggle,
    /// JSON string values (shown capitalised).
    Choice(&'static [&'static str]),
    Color,
    Text { multiline: bool },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Field {
    pub path: &'static str,
    pub label: &'static str,
    pub widget: Widget,
    /// The value when the key is absent (the format omits defaults), as JSON text.
    pub default: &'static str,
}

/// Which nodes a section applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applies {
    All,
    /// Children of an auto-layout frame.
    LayoutChild,
    /// Frames and components.
    Frames,
    /// Frames and components with auto layout.
    AutoLayout,
    /// Shapes with corners.
    Corners,
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Section {
    pub title: &'static str,
    pub applies: Applies,
    pub fields: &'static [Field],
}

const fn num(path: &'static str, label: &'static str, min: f64, max: f64, unit: &'static str, default: &'static str) -> Field {
    Field { path, label, widget: Widget::Number { min, max, speed: 1.0, unit, decimals: 2 }, default }
}

const fn frac(path: &'static str, label: &'static str, default: &'static str) -> Field {
    Field { path, label, widget: Widget::Number { min: 0.0, max: 1.0, speed: 0.01, unit: "", decimals: 2 }, default }
}

const fn choice(path: &'static str, label: &'static str, values: &'static [&'static str], default: &'static str) -> Field {
    Field { path, label, widget: Widget::Choice(values), default }
}

const fn toggle(path: &'static str, label: &'static str, default: &'static str) -> Field {
    Field { path, label, widget: Widget::Toggle, default }
}

const BIG: f64 = 1.0e6;

pub const BLEND_MODES: &[&str] = &[
    "normal", "darken", "multiply", "colorBurn", "lighten", "screen", "colorDodge", "overlay", "softLight", "hardLight", "difference", "exclusion", "hue", "saturation", "color", "luminosity",
];

pub static SECTIONS: &[Section] = &[
    Section {
        title: "Position and size",
        applies: Applies::All,
        fields: &[
            num("x", "X", -BIG, BIG, "px", "0"),
            num("y", "Y", -BIG, BIG, "px", "0"),
            num("width", "W", 0.0, BIG, "px", "0"),
            num("height", "H", 0.0, BIG, "px", "0"),
            num("rotation", "Rotation", -180.0, 180.0, "\u{b0}", "0"),
            toggle("flipX", "Flip horizontal", "false"),
            toggle("flipY", "Flip vertical", "false"),
            choice("constraints.h", "Horizontal constraint", &["left", "right", "leftRight", "center", "scale"], "\"left\""),
            choice("constraints.v", "Vertical constraint", &["top", "bottom", "topBottom", "center", "scale"], "\"top\""),
        ],
    },
    Section {
        title: "Layout child",
        applies: Applies::LayoutChild,
        fields: &[
            choice("sizingH", "Width sizing", &["fixed", "hug", "fill"], "\"fixed\""),
            choice("sizingV", "Height sizing", &["fixed", "hug", "fill"], "\"fixed\""),
            toggle("layoutChild.absolute", "Absolute position", "false"),
            num("layoutChild.grow", "Grow", 0.0, 100.0, "", "0"),
            num("minWidth", "Min W", 0.0, BIG, "px", "null"),
            num("maxWidth", "Max W", 0.0, BIG, "px", "null"),
            num("minHeight", "Min H", 0.0, BIG, "px", "null"),
            num("maxHeight", "Max H", 0.0, BIG, "px", "null"),
        ],
    },
    Section {
        title: "Auto layout",
        applies: Applies::AutoLayout,
        fields: &[
            choice("autoLayout.direction", "Direction", &["horizontal", "vertical", "grid"], "\"horizontal\""),
            num("autoLayout.gap", "Gap", 0.0, BIG, "px", "0"),
            num("autoLayout.crossGap", "Cross gap", 0.0, BIG, "px", "0"),
            num("autoLayout.padding.0", "Padding top", 0.0, BIG, "px", "0"),
            num("autoLayout.padding.1", "Padding right", 0.0, BIG, "px", "0"),
            num("autoLayout.padding.2", "Padding bottom", 0.0, BIG, "px", "0"),
            num("autoLayout.padding.3", "Padding left", 0.0, BIG, "px", "0"),
            choice("autoLayout.mainAlign", "Distribute", &["start", "center", "end", "spaceBetween"], "\"start\""),
            choice("autoLayout.crossAlign", "Align", &["start", "center", "end", "stretch", "baseline"], "\"start\""),
            toggle("autoLayout.wrap", "Wrap", "false"),
            choice("sizingH", "Width sizing", &["fixed", "hug", "fill"], "\"fixed\""),
            choice("sizingV", "Height sizing", &["fixed", "hug", "fill"], "\"fixed\""),
        ],
    },
    Section {
        title: "Appearance",
        applies: Applies::All,
        fields: &[
            frac("opacity", "Opacity", "1"),
            choice("blend", "Blend", BLEND_MODES, "\"normal\""),
            toggle("visible", "Visible", "true"),
            toggle("locked", "Locked", "false"),
        ],
    },
    Section {
        title: "Corners",
        applies: Applies::Corners,
        fields: &[
            num("radius.0", "Top left", 0.0, BIG, "px", "0"),
            num("radius.1", "Top right", 0.0, BIG, "px", "0"),
            num("radius.2", "Bottom right", 0.0, BIG, "px", "0"),
            num("radius.3", "Bottom left", 0.0, BIG, "px", "0"),
        ],
    },
    Section { title: "Frame", applies: Applies::Frames, fields: &[toggle("clip", "Clip content", "true")] },
    Section {
        title: "Text",
        applies: Applies::Text,
        fields: &[
            Field { path: "text", label: "Content", widget: Widget::Text { multiline: true }, default: "\"\"" },
            Field { path: "style.fontFamily", label: "Font", widget: Widget::Text { multiline: false }, default: "\"Inter\"" },
            num("style.fontSize", "Size", 1.0, 2000.0, "px", "16"),
            Field { path: "style.fontWeight", label: "Weight", widget: Widget::Number { min: 100.0, max: 900.0, speed: 10.0, unit: "", decimals: 0 }, default: "400" },
            toggle("style.italic", "Italic", "false"),
            Field { path: "style.color", label: "Colour", widget: Widget::Color, default: "\"#000000\"" },
            choice("align", "Align", &["left", "center", "right", "justify"], "\"left\""),
            choice("verticalAlign", "Vertical align", &["top", "middle", "bottom"], "\"top\""),
            num("style.lineHeight", "Line height", 0.0, 2000.0, "px", "null"),
            Field { path: "style.letterSpacing", label: "Letter spacing", widget: Widget::Number { min: -100.0, max: 100.0, speed: 0.1, unit: "px", decimals: 2 }, default: "0" },
            choice("style.decoration", "Decoration", &["none", "underline", "strikethrough"], "\"none\""),
        ],
    },
];

/// The value a field shows for a selection.
#[derive(Debug, Clone, PartialEq)]
pub enum Shown {
    Value(Value),
    Mixed,
}

fn is_auto_layout_frame(n: &Node) -> bool {
    matches!(n.kind, NodeKind::Frame { auto_layout: Some(_), .. } | NodeKind::Component { auto_layout: Some(_), .. })
}

/// Does `section` apply to node `n`?
pub fn applies(doc: &Document, n: &Node, a: Applies) -> bool {
    match a {
        Applies::All => true,
        Applies::LayoutChild => n.parent.and_then(|p| doc.node(p)).is_some_and(is_auto_layout_frame),
        Applies::Frames => matches!(n.kind, NodeKind::Frame { .. } | NodeKind::Component { .. }),
        Applies::AutoLayout => is_auto_layout_frame(n),
        Applies::Corners => matches!(n.kind, NodeKind::Frame { .. } | NodeKind::Component { .. } | NodeKind::Rectangle {} | NodeKind::Image { .. } | NodeKind::Instance { .. }),
        Applies::Text => matches!(n.kind, NodeKind::Text { .. }),
    }
}

/// A node as one JSON object: common properties and its kind's fields, plus `text` (the plain
/// content of a text node).
pub fn node_json(n: &Node) -> Value {
    let mut o = match serde_json::to_value(&n.props) {
        Ok(Value::Object(o)) => o,
        _ => Map::new(),
    };
    if let Ok(Value::Object(k)) = serde_json::to_value(&n.kind) {
        o.extend(k);
    }
    if let NodeKind::Text { spans, .. } = &n.kind {
        o.insert("text".into(), Value::String(spans.iter().map(|s| s.text.as_str()).collect()));
    }
    o.insert("name".into(), Value::String(n.name.clone()));
    Value::Object(o)
}

fn lookup<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(v, |v, k| match k.parse::<usize>() {
        Ok(i) => v.get(i),
        Err(_) => v.get(k),
    })
}

/// A field's value on one node (its default when absent). Radius corners read a uniform radius
/// as the same value on all four.
pub fn value_of(n: &Node, f: &Field) -> Value {
    let j = node_json(n);
    if f.path.starts_with("radius.")
        && let Some(r) = j.get("radius").and_then(Value::as_f64)
    {
        return json!(r);
    }
    lookup(&j, f.path).cloned().unwrap_or_else(|| serde_json::from_str(f.default).unwrap_or(Value::Null))
}

/// The node whose values a field shows: an instance's fills, strokes and corners are its
/// content root's (writing them on the instance writes overrides).
pub fn shown_node<'a>(doc: &'a Document, n: &'a Node, path: &str) -> &'a Node {
    let look = ["fills", "strokes", "radius"].iter().any(|k| path == *k || path.starts_with(&format!("{k}.")));
    match (&n.kind, n.children.first()) {
        (NodeKind::Instance { .. }, Some(&root)) if look => doc.node(root).unwrap_or(n),
        _ => n,
    }
}

/// What `f` shows for `ids`; `None` if the field applies to none of them.
pub fn read(doc: &Document, ids: &[NodeId], section: &Section, f: &Field) -> Option<Shown> {
    let mut out: Option<Value> = None;
    for n in ids.iter().filter_map(|&id| doc.get(id)).filter(|n| applies(doc, n, section.applies)) {
        let v = value_of(shown_node(doc, n, f.path), f);
        match &out {
            None => out = Some(v),
            Some(o) if !same(o, &v) => return Some(Shown::Mixed),
            Some(_) => {}
        }
    }
    out.map(Shown::Value)
}

fn same(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => (x - y).abs() < 1e-9,
        _ => a == b,
    }
}

/// The patch setting `path` to `value` on `n`: the whole top-level key, with the nested field
/// replaced.
fn patch(n: &Node, path: &str, value: Value) -> Map<String, Value> {
    let mut m = Map::new();
    if path == "text" {
        if let NodeKind::Text { spans, .. } = &n.kind {
            // Keep the first span's styling; editing replaces the content as one run.
            let mut first = spans.first().and_then(|s| serde_json::to_value(s).ok()).unwrap_or_else(|| json!({}));
            if let Some(o) = first.as_object_mut() {
                o.insert("text".into(), value);
            }
            m.insert("spans".into(), json!([first]));
        }
        return m;
    }
    let mut parts = path.split('.');
    let Some(top) = parts.next() else { return m };
    let rest: Vec<&str> = parts.collect();
    if rest.is_empty() {
        m.insert(top.into(), value);
        return m;
    }
    let j = node_json(n);
    let mut cur = j.get(top).cloned().unwrap_or(Value::Null);
    if top == "radius" {
        // A uniform radius becomes four corners when one corner changes.
        if let Some(r) = cur.as_f64() {
            cur = json!([r, r, r, r]);
        } else if cur.is_null() {
            cur = json!([0, 0, 0, 0]);
        }
    }
    if cur.is_null() {
        cur = json!({});
    }
    set_path(&mut cur, &rest, value);
    m.insert(top.into(), cur);
    m
}

fn set_path(v: &mut Value, path: &[&str], value: Value) {
    let Some((k, rest)) = path.split_first() else {
        *v = value;
        return;
    };
    let slot = match k.parse::<usize>() {
        Ok(i) => {
            if !v.is_array() {
                *v = json!([]);
            }
            let Some(a) = v.as_array_mut() else { return };
            while a.len() <= i {
                a.push(Value::from(0));
            }
            &mut a[i]
        }
        Err(_) => {
            if !v.is_object() {
                *v = json!({});
            }
            let Some(o) = v.as_object_mut() else { return };
            o.entry(k.to_string()).or_insert(Value::Null)
        }
    };
    set_path(slot, rest, value);
}

/// Sets `path` to `value` on every node in `ids` the section applies to, as one transaction
/// (merged into the previous one when `coalesce` matches, for drags).
pub fn write(s: &mut Session, ids: &[NodeId], applies_to: Applies, path: &str, value: Value, coalesce: Option<&str>) -> Result<(), Error> {
    let doc = s.doc();
    let patches: Vec<(NodeId, Map<String, Value>)> = ids.iter().filter_map(|&id| doc.get(id)).filter(|n| applies(doc, n, applies_to)).map(|n| (n.id, patch(shown_node(doc, n, path), path, value.clone()))).collect();
    if patches.is_empty() {
        return Ok(());
    }
    let mut meta = TxMeta::user(format!("Set {}", label_for(path)));
    if let Some(k) = coalesce {
        meta = meta.coalesce(k);
    }
    s.transact(meta, |tx| {
        for (id, p) in &patches {
            tx.update_json(*id, p)?;
        }
        Ok(())
    })
}

fn label_for(path: &str) -> String {
    SECTIONS.iter().flat_map(|s| s.fields).find(|f| f.path == path).map_or_else(|| path.to_string(), |f| f.label.to_lowercase())
}

/// Turns auto layout on (with defaults) or off for frames in `ids`.
pub fn set_auto_layout(s: &mut Session, ids: &[NodeId], on: bool) -> Result<(), Error> {
    let value = if on { json!({"direction": "vertical", "gap": 8, "padding": [8, 8, 8, 8]}) } else { Value::Null };
    write(s, ids, Applies::Frames, "autoLayout", value, None)
}

/// A list property (`fills`, `strokes`, `effects`) for the selection: the shared list, or
/// `Mixed` when the nodes differ.
pub fn read_list(doc: &Document, ids: &[NodeId], key: &str) -> Option<Shown> {
    let f = Field { path: match key {
        "fills" => "fills",
        "strokes" => "strokes",
        _ => "effects",
    }, label: "", widget: Widget::Toggle, default: "[]" };
    let section = Section { title: "", applies: Applies::All, fields: &[] };
    read(doc, ids, &section, &f)
}

/// Colour helpers for `#rrggbb[aa]` strings.
pub fn parse_color(s: &str) -> Option<[u8; 4]> {
    let h = s.strip_prefix('#')?;
    let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
    match h.len() {
        6 => Some([byte(0)?, byte(2)?, byte(4)?, 255]),
        8 => Some([byte(0)?, byte(2)?, byte(4)?, byte(6)?]),
        _ => None,
    }
}

pub fn format_color(c: [u8; 4]) -> String {
    if c[3] == 255 { format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]) } else { format!("#{:02x}{:02x}{:02x}{:02x}", c[0], c[1], c[2], c[3]) }
}
