//! The host side of the script API: one dispatcher for every call the prelude makes.

use std::path::{Component, Path, PathBuf};

use doc::{NodeId, NodeKind};
use kurbo::Rect;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use crate::query::{self, Query, SpaceOpts, key_of, parse_id};
use crate::{components, vars};
use crate::{Image, State};

type R = Result<Value, String>;

/// The name a call has in the script (for error messages).
pub(crate) fn public_name(call: &str) -> &str {
    match call {
        "query" => "get",
        "summaries" => "find",
        "delete" => "remove",
        "export" => "exportImage",
        other => other,
    }
}

fn arg<T: DeserializeOwned>(args: &[Value], i: usize, what: &str) -> Result<T, String> {
    serde_json::from_value(args.get(i).cloned().unwrap_or(Value::Null)).map_err(|e| format!("{what}: {e}"))
}

fn opt_id(args: &[Value], i: usize, what: &str) -> Result<Option<NodeId>, String> {
    arg::<Option<String>>(args, i, what)?.map(|s| parse_id(&s)).transpose()
}

fn id_arg(args: &[Value], i: usize) -> Result<NodeId, String> {
    let s: String = arg(args, i, "id")?;
    parse_id(&s)
}

fn obj_arg(args: &[Value], i: usize, what: &str) -> Result<Map<String, Value>, String> {
    match args.get(i) {
        Some(Value::Object(o)) => Ok(o.clone()),
        _ => Err(format!("{what} must be an object")),
    }
}

fn doc_err(e: doc::Error) -> String {
    match e {
        doc::Error::Input(m) => m,
        e => e.to_string(),
    }
}

pub(crate) fn edit_err(e: editor::Error) -> String {
    match e {
        editor::Error::Doc(d) => doc_err(d),
        e => e.to_string(),
    }
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GetOpts {
    /// Levels of children to include as nodes (default 0: ids only).
    #[serde(default)]
    depth: usize,
    /// Only these properties (plus `id` and `type`).
    fields: Option<Vec<String>>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ShotOpts {
    /// Pixels per unit (default 1; reduced to fit the size limit).
    scale: Option<f64>,
    /// Extra space around the content, in units.
    padding: Option<f64>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ExportOpts {
    /// Pixels (or PDF points) per unit (default 1).
    scale: Option<f64>,
    /// Extra space around the content, in units.
    padding: Option<f64>,
    /// JPEG quality 1–100 (default 90).
    quality: Option<u8>,
}

pub(crate) fn call(st: &mut State, name: &str, a: &[Value]) -> R {
    match name {
        "insert" => insert(st, a),
        "update" => update(st, a),
        "replace" => replace(st, a),
        "move" => {
            let id = id_arg(a, 0)?;
            let parent = opt_id(a, 1, "parentId")?.unwrap_or(st.session.page());
            let index: Option<usize> = arg(a, 2, "index")?;
            st.session.in_tx(|tx| tx.move_node(id, Some(parent), index)).map_err(edit_err)?;
            st.dirty = true;
            Ok(Value::Null)
        }
        "copy" => {
            let id = id_arg(a, 0)?;
            let parent = match opt_id(a, 1, "parentId")? {
                Some(p) => p,
                None => editor::select::parent_of(st.session.doc(), id).ok_or_else(|| format!("no node with id {id}"))?,
            };
            let overrides: Option<Map<String, Value>> = arg(a, 2, "overrides")?;
            let overrides = overrides.map(|o| props_for(st, id, o)).transpose()?;
            let index: Option<usize> = arg(a, 3, "index")?;
            let new = st.session.in_tx(|tx| tx.copy(id, Some(parent), index, overrides.as_ref())).map_err(edit_err)?;
            created(st, new)
        }
        "delete" => {
            let id = id_arg(a, 0)?;
            if st.session.doc().pages().len() == 1 && id == st.session.page() {
                return Err("the last page can't be removed".into());
            }
            st.session.in_tx(|tx| tx.delete(id)).map_err(edit_err)?;
            st.dirty = true;
            Ok(Value::Null)
        }
        "get" => {
            sync(st)?;
            let s: String = arg(a, 0, "id")?;
            let o: GetOpts = arg::<Option<GetOpts>>(a, 1, "options")?.unwrap_or_default();
            let Ok(id) = parse_id(&s) else { return Err(format!("{s:?} is not a node id; pass a query object to search")) };
            Ok(match st.session.doc().key(id) {
                Some(k) => query::node_json(&st.session, k, o.depth, o.fields.as_deref()),
                None => Value::Null,
            })
        }
        "query" => {
            sync(st)?;
            let q: Query = arg(a, 0, "query")?;
            let o: GetOpts = arg::<Option<GetOpts>>(a, 1, "options")?.unwrap_or_default();
            let keys = query::find(&st.session, &q)?;
            Ok(Value::Array(keys.into_iter().map(|k| query::node_json(&st.session, k, o.depth, o.fields.as_deref())).collect()))
        }
        "find" => {
            let q: Query = arg(a, 0, "query")?;
            let doc = st.session.doc();
            let keys = query::find(&st.session, &q)?;
            Ok(Value::Array(keys.into_iter().filter_map(|k| doc.node(k)).map(|n| Value::String(n.id.to_string())).collect()))
        }
        "summaries" => {
            sync(st)?;
            let q: Query = arg(a, 0, "query")?;
            let q = Query { limit: q.limit.or(Some(usize::MAX)), ..q };
            let keys = query::find(&st.session, &q)?;
            Ok(Value::Array(keys.into_iter().map(|k| query::summary(&st.session, k)).collect()))
        }
        "setVariables" => {
            let input = a.first().cloned().unwrap_or(Value::Null);
            let current = st.session.doc().variables().clone();
            let v = st
                .session
                .in_tx(|tx| {
                    let v = vars::build(input, &current, &mut || tx.new_id()).map_err(doc::Error::Input)?;
                    tx.set_variables(v.clone())?;
                    Ok(v)
                })
                .map_err(edit_err)?;
            st.dirty = true;
            Ok(vars::friendly(&v))
        }
        "getVariables" => Ok(vars::friendly(st.session.doc().variables())),
        "createComponent" => {
            let ids: Vec<String> = arg(a, 0, "ids")?;
            let ids = ids.iter().map(|s| parse_id(s)).collect::<Result<Vec<_>, _>>()?;
            let name: Option<String> = arg(a, 1, "name")?;
            let id = st.session.in_tx(|tx| tx.create_component(&ids, name.as_deref().unwrap_or("Component"))).map_err(edit_err)?;
            if let Some(n) = name {
                // A converted frame keeps its own name unless one is given.
                st.session.in_tx(|tx| tx.update(id, doc::PropValue::Name(n))).map_err(edit_err)?;
            }
            created(st, id)
        }
        "detach" => {
            let id = id_arg(a, 0)?;
            // Detaching copies the expanded content, so expand first.
            sync(st)?;
            let frame = st.session.in_tx(|tx| tx.detach(id)).map_err(edit_err)?;
            st.dirty = true;
            Ok(Value::String(frame.to_string()))
        }
        "findEmptySpace" => {
            sync(st)?;
            let w: f64 = arg(a, 0, "width")?;
            let h: f64 = arg(a, 1, "height")?;
            let o: SpaceOpts = arg::<Option<SpaceOpts>>(a, 2, "options")?.unwrap_or_default();
            let p = query::empty_space(&st.session, w, h, &o)?;
            Ok(json!({"x": p.x, "y": p.y}))
        }
        "screenshot" => {
            let ids: Option<Vec<String>> = arg(a, 0, "ids")?;
            let o: ShotOpts = arg::<Option<ShotOpts>>(a, 1, "options")?.unwrap_or_default();
            let img = shoot(st, ids, &o)?;
            let r = json!({"image": st.out.images.len(), "width": img.width, "height": img.height});
            st.out.images.push(img);
            Ok(r)
        }
        "export" => {
            let ids: Option<Vec<String>> = arg(a, 0, "ids")?;
            let format: String = arg(a, 1, "format")?;
            let path: String = arg(a, 2, "path")?;
            let o: ExportOpts = arg::<Option<ExportOpts>>(a, 3, "options")?.unwrap_or_default();
            let fmt = export::Format::parse(&format).ok_or_else(|| format!("unknown format {format:?}; use \"png\", \"jpeg\", \"webp\", \"svg\" or \"pdf\""))?;
            let path = resolve_path(&st.opts.roots, &path)?;
            let (bytes, w, h) = export_nodes(st, ids, fmt, &o)?;
            std::fs::write(&path, &bytes).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
            let shown = path.display().to_string();
            st.out.exports.push(shown.clone());
            Ok(json!({"path": shown, "width": w, "height": h, "bytes": bytes.len()}))
        }
        "select" => {
            let ids: Vec<String> = arg(a, 0, "ids")?;
            let ids = ids.iter().map(|s| parse_id(s)).collect::<Result<Vec<_>, _>>()?;
            st.session.select(ids);
            Ok(Value::Null)
        }
        "setPage" => {
            let id = id_arg(a, 0)?;
            if !st.session.doc().get(id).is_some_and(|n| matches!(n.kind, NodeKind::Page { .. })) {
                return Err(format!("{id} is not a page"));
            }
            sync(st)?;
            st.session.set_page(id);
            Ok(Value::Null)
        }
        "print" => {
            let s: String = arg(a, 0, "text")?;
            st.out.logs.push(s);
            Ok(Value::Null)
        }
        "state" => {
            let v = st.session.view();
            Ok(json!({
                "page": st.session.page().to_string(),
                "selection": st.session.selection().iter().map(|i| i.to_string()).collect::<Vec<_>>(),
                "viewport": {"x": v.x0, "y": v.y0, "width": v.width(), "height": v.height(), "zoom": st.session.zoom()},
            }))
        }
        other => Err(format!("unknown call {other:?}")),
    }
}

fn created(st: &mut State, id: NodeId) -> R {
    st.dirty = true;
    let s = id.to_string();
    st.out.created.push(s.clone());
    Ok(Value::String(s))
}

/// Brings layout and the index up to date with the script's edits, for reads.
pub(crate) fn sync(st: &mut State) -> Result<(), String> {
    if std::mem::take(&mut st.dirty) {
        st.session.sync_tx().map_err(edit_err)?;
    }
    Ok(())
}

fn insert(st: &mut State, a: &[Value]) -> R {
    let mut node = match a.get(1) {
        Some(v @ Value::Object(_)) => v.clone(),
        _ => return Err("node must be an object with a \"type\"".into()),
    };
    if node.get("type").and_then(Value::as_str) == Some("svg") {
        return insert_svg(st, a, node);
    }
    query::normalize(&mut node, None, None);
    query::defaults(&mut node);
    let overrides = prepare(st, &mut node)?;
    let is_page = node.get("type").and_then(Value::as_str) == Some("page");
    let parent = match opt_id(a, 0, "parentId")? {
        Some(p) => Some(p),
        None if is_page => None,
        None => Some(st.session.page()),
    };
    let index: Option<usize> = arg(a, 2, "index")?;
    let id = st.session.in_tx(|tx| tx.insert_json(parent, index, node)).map_err(edit_err)?;
    let r = created(st, id);
    for (instance, map) in overrides {
        components::apply_overrides(st, instance, map)?;
    }
    r
}

/// `{type: "svg", svg: "<svg…", name?, x?, y?}`: the SVG imported as a frame of shapes. What
/// could not be imported exactly is printed to the logs.
fn insert_svg(st: &mut State, a: &[Value], node: Value) -> R {
    let source = node.get("svg").and_then(Value::as_str).ok_or("an svg node needs \"svg\": the SVG source")?;
    let name = node.get("name").and_then(Value::as_str).unwrap_or("SVG");
    let imp = st.session.import_svg(source, name).map_err(edit_err)?;
    let mut frame = imp.node;
    for k in ["x", "y"] {
        if let (Some(v), Some(o)) = (node.get(k), frame.as_object_mut()) {
            o.insert(k.into(), v.clone());
        }
    }
    let parent = opt_id(a, 0, "parentId")?.unwrap_or_else(|| st.session.page());
    let index: Option<usize> = arg(a, 2, "index")?;
    let id = st
        .session
        .in_tx(|tx| {
            for (id, meta) in imp.assets {
                tx.put_asset(id, meta)?;
            }
            tx.insert_json(Some(parent), index, frame)
        })
        .map_err(edit_err)?;
    st.out.logs.extend(imp.warnings.into_iter().map(|w| format!("svg import: {w}")));
    created(st, id)
}

/// Resolves names (components, variables, modes) in a node to insert, and takes out by-name
/// instance overrides, to apply once the instances exist.
fn prepare(st: &mut State, node: &mut Value) -> Result<Vec<components::Named>, String> {
    if components::has_instance(node) {
        // Instances take their component's laid-out size.
        sync(st)?;
    }
    components::resolve_components(node, st.session.doc())?;
    vars::resolve_names(node, st.session.doc().variables())?;
    let mut overrides = Vec::new();
    st.session.in_tx(|tx| components::take_overrides(node, &mut || tx.new_id(), &mut overrides).map_err(doc::Error::Input)).map_err(edit_err)?;
    Ok(overrides)
}

fn update(st: &mut State, a: &[Value]) -> R {
    let id = id_arg(a, 0)?;
    let mut props = obj_arg(a, 1, "props")?;
    // By-name overrides edit inside the instance; a list replaces them all.
    let named = match props.get("overrides") {
        Some(Value::Object(_)) => props.remove("overrides").and_then(|v| match v {
            Value::Object(m) => Some(m),
            _ => None,
        }),
        _ => None,
    };
    if let Some(Value::String(c)) = props.get_mut("component") {
        *c = components::component_id(st.session.doc(), c)?.to_string();
    }
    let props = props_for(st, id, props)?;
    if !props.is_empty() {
        st.session.in_tx(|tx| tx.update_json(id, &props)).map_err(edit_err)?;
        st.dirty = true;
    }
    if let Some(m) = named {
        components::apply_overrides(st, id, m)?;
    }
    Ok(Value::Null)
}

/// Expands shorthands in properties for node `id` (as `update` and `copy` overrides take them):
/// text style, auto layout and bindings merge into the node's own.
pub(crate) fn props_for(st: &State, id: NodeId, props: Map<String, Value>) -> Result<Map<String, Value>, String> {
    let mut props = Value::Object(props);
    let doc = st.session.doc();
    let n = doc.get(id).ok_or_else(|| format!("no node with id {id}"))?;
    let current = editor::inspect::node_json(n);
    let style = matches!(n.kind, NodeKind::Text { .. }).then(|| current.get("style").cloned().unwrap_or(Value::Null));
    query::normalize(&mut props, style.as_ref(), current.get("autoLayout"));
    vars::resolve_names(&mut props, doc.variables())?;
    // Bindings and modes merge into the node's existing ones; `null` removes one.
    for field in ["bindings", "modes"] {
        if let Some(Value::Object(b)) = props.get_mut(field) {
            let mut merged = current.get(field).and_then(Value::as_object).cloned().unwrap_or_default();
            for (k, v) in std::mem::take(b) {
                if v.is_null() {
                    merged.remove(&k);
                } else {
                    merged.insert(k, v);
                }
            }
            *b = merged;
        }
    }
    match props {
        Value::Object(p) => Ok(p),
        _ => Err("props must be an object".into()),
    }
}

fn replace(st: &mut State, a: &[Value]) -> R {
    let id = id_arg(a, 0)?;
    let mut node = match a.get(1) {
        Some(v @ Value::Object(_)) => v.clone(),
        _ => return Err("node must be an object with a \"type\"".into()),
    };
    query::normalize(&mut node, None, None);
    query::defaults(&mut node);
    let overrides = prepare(st, &mut node)?;
    let new = st.session.in_tx(|tx| tx.replace_json(id, node)).map_err(edit_err)?;
    let r = created(st, new);
    for (instance, map) in overrides {
        components::apply_overrides(st, instance, map)?;
    }
    r
}

/// Exports nodes (or everything on the current page) at full detail: the bytes, and the size
/// in pixels (points for PDF).
fn export_nodes(st: &mut State, ids: Option<Vec<String>>, format: export::Format, o: &ExportOpts) -> Result<(Vec<u8>, u32, u32), String> {
    sync(st)?;
    let page = st.session.page();
    let (scene, doc) = st.session.scene_and_doc();
    let ids: Vec<NodeId> = match ids {
        Some(ids) if !ids.is_empty() => {
            let mut v = Vec::new();
            for s in &ids {
                let k = key_of(doc, s)?;
                let id = doc.node(k).map(|n| n.id).ok_or_else(|| format!("no node with id {s}"))?;
                if editor::select::page_of(doc, id) != Some(page) {
                    return Err(format!("{s} isn't on the current page; call setPage first"));
                }
                v.push(id);
            }
            v
        }
        _ => {
            let pk = doc.key(page).ok_or("no current page")?;
            doc.node(pk).map(|p| p.children.iter().filter_map(|k| doc.node(*k)).map(|n| n.id).collect()).unwrap_or_default()
        }
    };
    let assets = scene.assets();
    let mut content = export::collect_with(doc, &ids, &mut scene.text, &mut scene.images, &*assets).map_err(|e| e.to_string())?;
    content.pad(o.padding.unwrap_or(0.0));
    let opts = export::Options { scale: o.scale.unwrap_or(1.0), quality: o.quality.unwrap_or(90) };
    let (w, h) = content.pixels(opts.scale);
    let bytes = export::write(&content, format, opts).map_err(|e| e.to_string())?;
    Ok((bytes, w, h))
}

/// Renders nodes (or the whole page) of the current page to PNG.
fn shoot(st: &mut State, ids: Option<Vec<String>>, o: &ShotOpts) -> Result<Image, String> {
    sync(st)?;
    let doc = st.session.doc();
    let page = st.session.page();
    let rect = match &ids {
        Some(ids) if !ids.is_empty() => {
            let mut r: Option<Rect> = None;
            for s in ids {
                let k = key_of(doc, s)?;
                let id = doc.node(k).map(|n| n.id).ok_or_else(|| format!("no node with id {s}"))?;
                if editor::select::page_of(doc, id) != Some(page) {
                    return Err(format!("{s} isn't on the current page; call setPage first"));
                }
                if let Some(b) = st.session.scene.node_bounds(doc, id) {
                    r = Some(r.map_or(b, |r| r.union(b)));
                }
            }
            r
        }
        _ => st.session.scene.page_bounds(doc),
    }
    .ok_or("nothing to capture: the nodes are empty or hidden")?;
    let pad = o.padding.unwrap_or(0.0).max(0.0);
    let rect = rect.inflate(pad, pad);
    let rect = Rect::new(rect.x0.floor(), rect.y0.floor(), rect.x1.ceil(), rect.y1.ceil());
    let max = st.opts.max_screenshot_px as f64;
    let mut scale = o.scale.unwrap_or(1.0);
    if !(scale > 0.0 && scale.is_finite()) {
        return Err("scale must be a positive number".into());
    }
    scale = scale.min(max / rect.width().max(rect.height()).max(1.0));
    let renderer = st.renderer.get_or_insert_with(render_cpu::CpuRenderer::default);
    let (scene, doc) = st.session.scene_and_doc();
    let px = render_cpu::render_rect(scene, doc, rect, scale, renderer).map_err(|e| e.to_string())?;
    Ok(Image { ids: ids.unwrap_or_default(), width: px.width() as u32, height: px.height() as u32, png: render_cpu::png(&px) })
}

/// Resolves `path` inside one of `roots`: relative paths are taken from the first root; `..`
/// components and links that leave every root are refused.
pub fn resolve_path(roots: &[PathBuf], path: &str) -> Result<PathBuf, String> {
    let first = roots.first().ok_or("file access is disabled (the host has no file roots)")?;
    let p = Path::new(path);
    if p.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(format!("{path}: \"..\" is not allowed"));
    }
    let full = if p.is_absolute() { p.to_path_buf() } else { first.join(p) };
    // The file may not exist yet: canonicalise its folder.
    let dir = full.parent().ok_or_else(|| format!("{path}: not a file path"))?;
    let name = full.file_name().ok_or_else(|| format!("{path}: not a file path"))?;
    let dir = dir.canonicalize().map_err(|e| format!("{}: {e}", dir.display()))?;
    let inside = roots.iter().filter_map(|r| r.canonicalize().ok()).any(|r| dir.starts_with(&r));
    if !inside {
        return Err(format!("{path} is outside the allowed folders"));
    }
    Ok(dir.join(name))
}
