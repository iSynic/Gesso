//! Reading the document for scripts: node JSON (with world bounds), queries, and free space.

use doc::{Document, Node, NodeId, NodeKey, NodeKind};
use editor::Session;
use kurbo::{Point, Rect};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// One value or several.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    fn contains(&self, s: &str) -> bool {
        match self {
            OneOrMany::One(v) => v == s,
            OneOrMany::Many(v) => v.iter().any(|x| x == s),
        }
    }
}

/// Which nodes a `get`/`find` query matches. All given conditions must hold.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Query {
    /// One node by id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Node type(s), e.g. `"text"` or `["frame", "rectangle"]`.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub ty: Option<OneOrMany>,
    /// Name, exact or with `*` wildcards (case-sensitive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Text content contains this (case-insensitive); text nodes only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Only descendants of this node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub within: Option<String>,
    /// `"current"` (default), `"all"`, or a page id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<String>,
    /// Only visible (true) or hidden (false) nodes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
    /// At most this many results (default 1000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

pub(crate) fn parse_id(s: &str) -> Result<NodeId, String> {
    s.parse::<NodeId>().map_err(|e| format!("{s:?} is not a node id ({e})"))
}

pub(crate) fn key_of(doc: &Document, s: &str) -> Result<NodeKey, String> {
    let id = parse_id(s)?;
    doc.key(id).ok_or_else(|| format!("no node with id {s}"))
}

/// `*` wildcard match.
#[cfg(test)]
fn glob(pat: &str, s: &str) -> bool {
    Glob::new(pat).is_match(s)
}

/// A name pattern split at its `*`s once, so matching a node doesn't allocate.
struct Glob<'a> {
    parts: Vec<&'a str>,
}

impl<'a> Glob<'a> {
    fn new(pat: &'a str) -> Self {
        Self { parts: pat.split('*').collect() }
    }

    fn is_match(&self, s: &str) -> bool {
        let [first, middle @ .., last] = self.parts.as_slice() else {
            return self.parts.first().is_some_and(|p| *p == s);
        };
        if !s.starts_with(first) || s.len() < first.len() + last.len() || !s.ends_with(last) {
            return false;
        }
        let mut rest = &s[first.len()..s.len() - last.len()];
        for p in middle {
            match rest.find(p) {
                Some(i) => rest = &rest[i + p.len()..],
                None => return false,
            }
        }
        true
    }
}

fn plain_text(n: &Node) -> Option<String> {
    match &n.kind {
        NodeKind::Text { spans, .. } => Some(spans.iter().map(|s| s.text.as_str()).collect()),
        NodeKind::Note { text } => Some(text.clone()),
        _ => None,
    }
}

/// Nodes matching `q`, in document order.
pub(crate) fn find(s: &Session, q: &Query) -> Result<Vec<NodeKey>, String> {
    let doc = s.doc();
    let limit = q.limit.unwrap_or(1000);
    if let Some(id) = &q.id {
        let k = key_of(doc, id)?;
        return Ok(doc.node(k).filter(|n| Matcher::new(q).hit(n)).map(|_| k).into_iter().collect());
    }
    let roots: Vec<NodeKey> = match (&q.within, q.page.as_deref()) {
        (Some(w), _) => vec![key_of(doc, w)?],
        (None, Some("all")) => doc.pages().to_vec(),
        (None, None | Some("current")) => doc.key(s.page()).into_iter().collect(),
        (None, Some(p)) => vec![key_of(doc, p)?],
    };
    let text = q.text.as_ref().map(|t| t.to_lowercase());
    let m = Matcher::new(q);
    let hit = |n: &Node| m.hit(n) && text.as_ref().is_none_or(|t| plain_text(n).is_some_and(|p| p.to_lowercase().contains(t.as_str())));
    if q.within.is_none() {
        return Ok(scan_pages(doc, &roots, limit, hit));
    }
    let mut out = Vec::new();
    for r in roots {
        for k in doc.descendants(r).into_iter().skip(1) {
            let Some(n) = doc.node(k) else { continue };
            if !hit(n) {
                continue;
            }
            out.push(k);
            if out.len() >= limit {
                return Ok(out);
            }
        }
    }
    Ok(out)
}

/// Whole-page queries scan the node storage in memory order (much faster than walking the tree
/// at 100k nodes), keep matches on `pages`, then put them in document order.
fn scan_pages(doc: &Document, pages: &[NodeKey], limit: usize, hit: impl Fn(&Node) -> bool) -> Vec<NodeKey> {
    let hits: Vec<NodeKey> = doc.iter().filter(|(_, n)| n.parent.is_some() && hit(n)).map(|(k, _)| k).collect();
    // Many matches: one walk of the pages puts them in order faster than a path per match.
    if hits.len() * 16 >= doc.len() {
        let set: rustc_hash::FxHashSet<NodeKey> = hits.into_iter().collect();
        return pages.iter().flat_map(|&p| doc.descendants(p)).filter(|k| set.contains(k)).take(limit).collect();
    }
    let mut found: Vec<(NodeKey, Vec<usize>, NodeKey)> = Vec::new();
    for k in hits {
        // The path of child indices from the page down, which orders nodes as the tree does.
        let mut path = Vec::new();
        let mut cur = k;
        while let Some(p) = doc.node(cur).and_then(|c| c.parent) {
            path.push(doc.node(p).and_then(|pn| pn.children.iter().position(|&c| c == cur)).unwrap_or(0));
            cur = p;
        }
        if pages.contains(&cur) {
            path.reverse();
            found.push((cur, path, k));
        }
    }
    found.sort_by(|a, b| {
        let page = |k: NodeKey| pages.iter().position(|&p| p == k);
        page(a.0).cmp(&page(b.0)).then_with(|| a.1.cmp(&b.1))
    });
    found.into_iter().take(limit).map(|(_, _, k)| k).collect()
}

/// A query's node conditions, with the name pattern prepared once.
struct Matcher<'q> {
    q: &'q Query,
    name: Option<Glob<'q>>,
}

impl<'q> Matcher<'q> {
    fn new(q: &'q Query) -> Self {
        Self { q, name: q.name.as_deref().map(Glob::new) }
    }

    fn hit(&self, n: &Node) -> bool {
        self.q.ty.as_ref().is_none_or(|t| t.contains(n.kind.type_name()))
            && self.name.as_ref().is_none_or(|g| g.is_match(&n.name))
            && self.q.visible.is_none_or(|v| n.props.visible == v)
    }
}

/// A world box, rounded to 1/100 px like stored geometry.
fn rect_json(r: Rect) -> Value {
    let q = |v: f64| (v * 100.0).round() / 100.0;
    json!({"x": q(r.x0), "y": q(r.y0), "width": q(r.width()), "height": q(r.height())})
}

/// A node as one JSON object: `id`, `type`, `name`, its properties (as accepted by `insert`), a
/// plain `text` for text nodes, `parentId`, and `bounds` (world box, current page only). Children
/// are nested `depth` levels deep; below that only `childIds` are listed.
pub fn node_json(s: &Session, key: NodeKey, depth: usize, fields: Option<&[String]>) -> Value {
    let doc = s.doc();
    let Some(n) = doc.node(key) else { return Value::Null };
    let mut o = match editor::inspect::node_json(n) {
        Value::Object(o) => o,
        _ => Map::new(),
    };
    if let Some(fields) = fields {
        o.retain(|k, _| k == "type" || fields.iter().any(|f| f == k));
    }
    let mut out = Map::new();
    out.insert("id".into(), Value::String(n.id.to_string()));
    out.extend(o);
    let want = |k: &str| fields.is_none_or(|f| f.iter().any(|x| x == k));
    if want("parentId")
        && let Some(p) = n.parent.and_then(|p| doc.node(p))
    {
        out.insert("parentId".into(), Value::String(p.id.to_string()));
    }
    if want("bounds")
        && let Some(b) = editor::xform::geometric_bounds(&s.scene.index, n)
    {
        out.insert("bounds".into(), rect_json(b));
    }
    // Content shown by an instance: which instance, and the override path to this node.
    if let Some((instance, path)) = doc::components::override_target(doc, n.id) {
        out.insert("instanceId".into(), Value::String(instance.to_string()));
        out.insert("instancePath".into(), Value::Array(path.iter().map(|i| Value::String(i.to_string())).collect()));
    }
    // Asking for depth means wanting the children, whatever `fields` lists.
    if !n.children.is_empty() {
        if depth > 0 {
            out.insert("children".into(), Value::Array(n.children.iter().map(|&c| node_json(s, c, depth - 1, fields)).collect()));
        } else if want("children") || want("childIds") {
            let ids = n.children.iter().filter_map(|&c| doc.node(c)).map(|c| Value::String(c.id.to_string())).collect();
            out.insert("childIds".into(), Value::Array(ids));
        }
    }
    Value::Object(out)
}

/// Light per-node records for `find(fn)`.
pub(crate) fn summary(s: &Session, key: NodeKey) -> Value {
    let doc = s.doc();
    let Some(n) = doc.node(key) else { return Value::Null };
    let p = &n.props;
    let mut o = json!({
        "id": n.id.to_string(),
        "type": n.kind.type_name(),
        "name": n.name,
        "x": p.x, "y": p.y, "width": p.width, "height": p.height,
        "visible": p.visible,
    });
    if let Some(parent) = n.parent.and_then(|k| doc.node(k)) {
        o["parentId"] = Value::String(parent.id.to_string());
    }
    if let Some(t) = plain_text(n) {
        o["text"] = Value::String(t);
    }
    if let Some(b) = editor::xform::geometric_bounds(&s.scene.index, n) {
        o["bounds"] = rect_json(b);
    }
    o
}

/// Options for `findEmptySpace`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SpaceOpts {
    /// Prefer the free spot closest to this point (default: right of the existing content).
    pub near: Option<PointArg>,
    /// Clear space kept around the result (default 100).
    pub margin: Option<f64>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub(crate) struct PointArg {
    pub x: f64,
    pub y: f64,
}

/// Candidates nearest `near` that are tested for overlap.
const SPACE_CANDIDATES: usize = 64;

/// A free `w`×`h` spot on the current page, `margin` clear of everything. Without `near`: to the
/// right of all content, top-aligned with it.
pub(crate) fn empty_space(s: &Session, w: f64, h: f64, o: &SpaceOpts) -> Result<Point, String> {
    if !(w > 0.0 && h > 0.0 && w.is_finite() && h.is_finite()) {
        return Err("width and height must be positive numbers".into());
    }
    let margin = o.margin.unwrap_or(100.0).max(0.0);
    let doc = s.doc();
    let index = &s.scene.index;
    let page = s.page();
    let tops: Vec<Rect> = doc
        .key(page)
        .and_then(|k| doc.node(k))
        .map(|p| p.children.iter().filter_map(|&c| doc.node(c)).filter_map(|n| index.bounds(n.id)).collect())
        .unwrap_or_default();
    let Some(content) = tops.iter().copied().reduce(|a, b| a.union(b)) else {
        return Ok(o.near.map_or(Point::ZERO, |p| Point::new(p.x, p.y)));
    };
    let Some(near) = o.near else {
        return Ok(Point::new((content.x1 + margin).round(), content.y0.round()));
    };
    let near = Point::new(near.x, near.y);
    let free = |p: Point| {
        let r = Rect::new(p.x - margin, p.y - margin, p.x + w + margin, p.y + h + margin);
        !tops.iter().any(|t| t.overlaps(r))
    };
    // Candidate corners: the point itself, and beside each nearby block on all four sides.
    let mut nearby = tops.clone();
    nearby.sort_by(|a, b| dist(a, near).total_cmp(&dist(b, near)));
    nearby.truncate(SPACE_CANDIDATES);
    let mut cands = vec![near];
    for r in &nearby {
        cands.extend([
            Point::new(r.x1 + margin, near.y.clamp(r.y0, r.y1)),
            Point::new(r.x0 - margin - w, near.y.clamp(r.y0, r.y1)),
            Point::new(near.x.clamp(r.x0, r.x1), r.y1 + margin),
            Point::new(near.x.clamp(r.x0, r.x1), r.y0 - margin - h),
            Point::new(r.x1 + margin, r.y0),
            Point::new(r.x0, r.y1 + margin),
        ]);
    }
    cands.sort_by(|a, b| a.distance(near).total_cmp(&b.distance(near)));
    let found = cands.into_iter().map(|p| Point::new(p.x.round(), p.y.round())).find(|&p| free(p));
    // Right of everything is always free.
    Ok(found.unwrap_or(Point::new((content.x1 + margin).round(), near.y.round())))
}

fn dist(r: &Rect, p: Point) -> f64 {
    let dx = (r.x0 - p.x).max(0.0).max(p.x - r.x1);
    let dy = (r.y0 - p.y).max(0.0).max(p.y - r.y1);
    dx.hypot(dy)
}

/// Defaults for new nodes (`insert`, `replace`), where the key is absent, so nothing an agent
/// creates is invisible by accident:
/// - text without `width`, `sizingH` or `autoResize` hugs its content (`autoResize:
///   "widthAndHeight"`);
/// - an auto-layout frame or component without `width`/`height` hugs on that axis;
/// - shapes and frames without `width`/`height` are 100 × 100 (a line is 100 wide).
pub(crate) fn defaults(v: &mut Value) {
    let Some(o) = v.as_object_mut() else { return };
    let ty = o.get("type").and_then(Value::as_str).unwrap_or("").to_string();
    let has = |o: &Map<String, Value>, k: &str| o.get(k).is_some_and(|x| !x.is_null());
    match ty.as_str() {
        "text" => {
            if !has(o, "width") && !has(o, "sizingH") && !has(o, "autoResize") {
                o.insert("autoResize".into(), json!("widthAndHeight"));
            }
        }
        "frame" | "component" if has(o, "autoLayout") => {
            if !has(o, "width") && !has(o, "sizingH") {
                o.insert("sizingH".into(), json!("hug"));
            }
            if !has(o, "height") && !has(o, "sizingV") {
                o.insert("sizingV".into(), json!("hug"));
            }
        }
        "frame" | "component" | "rectangle" | "ellipse" | "polygon" | "line" => {
            if !has(o, "width") {
                o.insert("width".into(), json!(100.0));
            }
            if !has(o, "height") && ty != "line" {
                o.insert("height".into(), json!(100.0));
            }
        }
        _ => {}
    }
    if let Some(Value::Array(children)) = o.get_mut("children") {
        children.iter_mut().for_each(defaults);
    }
}

/// `#rgb` / `#rgba` as `#rrggbb` / `#rrggbbaa`; other strings unchanged.
pub(crate) fn expand_hex(c: &str) -> String {
    let h = c.strip_prefix('#').unwrap_or("");
    if (h.len() == 3 || h.len() == 4) && h.chars().all(|ch| ch.is_ascii_hexdigit()) {
        let mut out = String::from("#");
        for ch in h.chars() {
            out.push(ch);
            out.push(ch);
        }
        return out;
    }
    c.to_string()
}

/// Expands short hex in every `color` field below `v` (paints, shadows, stops, text styles).
fn expand_colors(v: &mut Value) {
    match v {
        Value::Object(o) => {
            for (k, x) in o.iter_mut() {
                match x {
                    Value::String(c) if k == "color" => *c = expand_hex(c),
                    _ => expand_colors(x),
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(expand_colors),
        _ => {}
    }
}

/// Agent-friendly shorthands in node and property JSON, expanded to the document format:
/// - a text node's `text: "…"` becomes `spans: [{text: "…"}]`;
/// - a partial text `style` is completed from `base` (the node's style, or the defaults);
/// - colours may be `#rgb` or `#rgba`;
/// - `fills: ["#hex", …]` becomes solid paints; a stroke may be `"#hex"` (1 px) or have
///   `paint: "#hex"`;
/// - `autoLayout.padding` as a number or `[vertical, horizontal]` becomes four sides;
/// - `parentId`, `bounds` and `childIds` (from `get`) are dropped.
pub(crate) fn normalize(v: &mut Value, base_style: Option<&Value>, base_layout: Option<&Value>) {
    let Some(o) = v.as_object_mut() else { return };
    for k in ["parentId", "bounds", "childIds"] {
        o.remove(k);
    }
    let is_text = o.get("type").and_then(Value::as_str).is_none_or(|t| t == "text");
    if is_text
        && let Some(t) = o.get("text").cloned()
        && (o.get("type").and_then(Value::as_str) == Some("text") || base_style.is_some())
    {
        o.remove("text");
        if !o.contains_key("spans") {
            o.insert("spans".into(), json!([{ "text": t }]));
        }
    }
    if let Some(Value::Object(style)) = o.get_mut("style") {
        let mut full = base_style.cloned().unwrap_or_else(|| json!({"fontFamily": "Inter", "fontSize": 14.0, "color": "#000000"}));
        if let Some(f) = full.as_object_mut() {
            f.extend(std::mem::take(style));
            *style = std::mem::take(f);
        }
    }
    if let Some(Value::Array(fills)) = o.get_mut("fills") {
        for f in fills {
            if let Value::String(c) = f {
                *f = json!({"type": "solid", "color": c});
            }
        }
    }
    if let Some(Value::Array(strokes)) = o.get_mut("strokes") {
        for st in strokes {
            match st {
                Value::String(c) => *st = json!({"paint": {"type": "solid", "color": c}, "weight": 1.0}),
                Value::Object(so) => {
                    if let Some(Value::String(c)) = so.get("paint").cloned() {
                        so.insert("paint".into(), json!({"type": "solid", "color": c}));
                    }
                }
                _ => {}
            }
        }
    }
    if let Some(Value::Object(al)) = o.get_mut("autoLayout") {
        if let Some(Value::Object(base)) = base_layout {
            let mut full = base.clone();
            full.extend(std::mem::take(al));
            *al = full;
        }
        if let Some(p) = al.get_mut("padding") {
            *p = match p {
                Value::Number(n) => json!([n, n, n, n]),
                Value::Array(a) if a.len() == 2 => json!([a[0], a[1], a[0], a[1]]),
                Value::Array(a) if a.len() == 1 => json!([a[0], a[0], a[0], a[0]]),
                _ => p.clone(),
            };
        }
    }
    for k in ["fills", "strokes", "effects", "style", "spans", "background"] {
        if let Some(x) = o.get_mut(k) {
            if let Value::String(c) = x {
                *c = expand_hex(c);
            }
            expand_colors(x);
        }
    }
    if let Some(Value::Array(children)) = o.get_mut("children") {
        for c in children {
            normalize(c, None, None);
        }
    }
}

#[cfg(test)]
#[allow(clippy::panic, clippy::type_complexity)]
mod tests {
    #[test]
    fn page_scans_match_a_tree_walk() {
        let doc = doc::Document::from_data(doc::testing::generate(&doc::testing::GenConfig { target_nodes: 3000, ..Default::default() })).unwrap_or_else(|e| panic!("{e}"));
        let pages = doc.pages().to_vec();
        for pick in [&pages[..1], &pages[..]] {
            for (name, ty) in [(Some("Card *"), None), (None, Some("text")), (Some("Title"), Some("text")), (Some("*"), None)] {
                let q = super::Query { name: name.map(String::from), ty: ty.map(|t| super::OneOrMany::One(t.into())), ..Default::default() };
                let walk: Vec<_> = pick.iter().flat_map(|&p| doc.descendants(p).into_iter().skip(1)).filter(|&k| doc.node(k).is_some_and(|n| super::Matcher::new(&q).hit(n))).collect();
                for limit in [5, usize::MAX] {
                    let scan = super::scan_pages(&doc, pick, limit, |n| super::Matcher::new(&q).hit(n));
                    assert_eq!(scan, walk.iter().copied().take(limit).collect::<Vec<_>>(), "{name:?} {ty:?}");
                }
            }
        }
    }

    use super::*;

    #[test]
    fn globs() {
        assert!(glob("Card", "Card"));
        assert!(!glob("Card", "Card 1"));
        assert!(glob("Card*", "Card 1"));
        assert!(glob("*Card*", "My Card 1"));
        assert!(glob("C*d*1", "Card 1"));
        assert!(!glob("C*x", "Card"));
        assert!(glob("*", ""));
    }

    #[test]
    fn shorthands() {
        let mut v = json!({"type": "text", "text": "Hi", "style": {"fontSize": 20}, "parentId": "x",
            "children": [{"type": "frame", "fills": ["#ff0000"], "autoLayout": {"padding": [8, 16]}}]});
        normalize(&mut v, None, None);
        assert_eq!(v["spans"], json!([{"text": "Hi"}]));
        assert_eq!(v["style"], json!({"fontFamily": "Inter", "fontSize": 20, "color": "#000000"}));
        assert!(v.get("parentId").is_none() && v.get("text").is_none());
        assert_eq!(v["children"][0]["fills"], json!([{"type": "solid", "color": "#ff0000"}]));
        assert_eq!(v["children"][0]["autoLayout"]["padding"], json!([8, 16, 8, 16]));
    }
}

