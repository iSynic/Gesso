//! Components for scripts: instances name their component by id or name, and take overrides as
//! a map from name paths inside the component to properties, e.g.
//! `{"Label": {text: "Buy"}, "Icon": {visible: false}}` or `{"Label/text": "Buy"}`.

use doc::{Document, NodeId, NodeKey, NodeKind};
use serde_json::{Map, Value};

use crate::State;
use crate::query::parse_id;

/// Replaces component names in `component` (here and in children) by ids; instances without a
/// size take the component's.
pub(crate) fn resolve_components(node: &mut Value, doc: &Document) -> Result<(), String> {
    let Some(o) = node.as_object_mut() else { return Ok(()) };
    if o.get("type").and_then(Value::as_str) == Some("instance")
        && let Some(Value::String(c)) = o.get_mut("component")
    {
        let id = component_id(doc, c)?;
        *c = id.to_string();
        if let Some(comp) = doc.get(id) {
            for (k, v) in [("width", comp.props.width), ("height", comp.props.height)] {
                o.entry(k).or_insert_with(|| v.into());
            }
        }
    }
    if let Some(Value::Array(children)) = o.get_mut("children") {
        for c in children {
            resolve_components(c, doc)?;
        }
    }
    Ok(())
}

/// Whether `node` or a descendant is an instance.
pub(crate) fn has_instance(node: &Value) -> bool {
    node.get("type").and_then(Value::as_str) == Some("instance") || node.get("children").and_then(Value::as_array).is_some_and(|c| c.iter().any(has_instance))
}

/// A component by id or by name (unique in the document).
pub(crate) fn component_id(doc: &Document, s: &str) -> Result<NodeId, String> {
    let is_component = |n: &doc::Node| n.virt.is_none() && matches!(n.kind, NodeKind::Component { .. });
    if let Ok(id) = parse_id(s)
        && doc.get(id).is_some_and(is_component)
    {
        return Ok(id);
    }
    let found: Vec<NodeId> = doc.iter().filter(|(_, n)| is_component(n) && n.name == s).map(|(_, n)| n.id).collect();
    match found.as_slice() {
        [id] => Ok(*id),
        [] => {
            let mut all: Vec<&str> = doc.iter().filter(|(_, n)| is_component(n)).map(|(_, n)| n.name.as_str()).collect();
            all.sort_unstable();
            Err(format!("no component {s:?}; the components are: {}", if all.is_empty() { "(none; insert a \"component\" or call createComponent)".into() } else { all.join(", ") }))
        }
        _ => Err(format!("{s:?} names more than one component; pass its id")),
    }
}

/// An instance and its by-name override map.
pub(crate) type Named = (NodeId, Map<String, Value>);

/// Takes by-name override maps out of instances in `node` (they are applied once the instances
/// exist and are expanded), giving those instances ids so they can be found again.
pub(crate) fn take_overrides(node: &mut Value, new_id: &mut dyn FnMut() -> NodeId, out: &mut Vec<Named>) -> Result<(), String> {
    let Some(o) = node.as_object_mut() else { return Ok(()) };
    if o.get("type").and_then(Value::as_str) == Some("instance")
        && matches!(o.get("overrides"), Some(Value::Object(_)))
        && let Some(Value::Object(map)) = o.remove("overrides")
    {
        let id = match o.get("id").and_then(Value::as_str) {
            Some(s) => parse_id(s)?,
            None => {
                let id = new_id();
                o.insert("id".into(), Value::String(id.to_string()));
                id
            }
        };
        out.push((id, map));
    }
    if let Some(Value::Array(children)) = o.get_mut("children") {
        for c in children {
            take_overrides(c, new_id, out)?;
        }
    }
    Ok(())
}

/// The first node named `name` below `from`, nearest first.
fn named_below(doc: &Document, from: NodeKey, name: &str) -> Option<NodeKey> {
    let mut queue = std::collections::VecDeque::from([from]);
    while let Some(k) = queue.pop_front() {
        let n = doc.node(k)?;
        if k != from && n.name == name {
            return Some(k);
        }
        queue.extend(n.children.iter().copied());
    }
    None
}

/// The node a `/`-separated name path reaches inside `instance` (`""` is the instance).
fn resolve_path(doc: &Document, instance: NodeKey, path: &str) -> Option<NodeKey> {
    if path.is_empty() {
        return Some(instance);
    }
    path.split('/').try_fold(instance, |k, step| named_below(doc, k, step))
}

/// The names of the nodes inside `instance`, for error messages.
fn names_inside(doc: &Document, instance: NodeKey) -> String {
    let mut names: Vec<&str> = doc.descendants(instance).into_iter().skip(2).filter_map(|k| doc.node(k)).map(|n| n.name.as_str()).filter(|s| !s.is_empty()).collect();
    names.dedup();
    names.truncate(40);
    if names.is_empty() { "(nothing named)".into() } else { names.join(", ") }
}

/// Whether a binding path is into an instance's look, which its content root carries.
fn is_look(path: &str) -> bool {
    matches!(path.split('/').next(), Some("fills" | "strokes" | "radius"))
}

/// Applies a by-name override map to `instance`: each entry edits the node it names inside the
/// instance, which the engine records as an override.
pub(crate) fn apply_overrides(st: &mut State, instance: NodeId, map: Map<String, Value>) -> Result<(), String> {
    crate::api::sync(st)?;
    for (key, value) in map {
        let doc = st.session.doc();
        let ik = doc.key(instance).ok_or_else(|| format!("no node with id {instance}"))?;
        if !matches!(doc.node(ik).map(|n| &n.kind), Some(NodeKind::Instance { .. })) {
            return Err(format!("overrides: {instance} is not an instance"));
        }
        // `{"Label": {…}}`, or `{"Label/text": "Buy"}` with the property as the last step.
        let (target, props) = match (resolve_path(doc, ik, &key), value) {
            (Some(k), Value::Object(p)) => (k, p),
            (found, value) => {
                let Some((path, prop)) = key.rsplit_once('/').or(found.is_none().then_some(("", key.as_str()))) else {
                    return Err(format!("overrides.{key}: give the properties as an object, e.g. {{\"{key}\": {{\"text\": \"…\"}}}}"));
                };
                let k = resolve_path(doc, ik, path).ok_or_else(|| format!("overrides.{key}: nothing named {path:?} in this instance; it contains: {}", names_inside(doc, ik)))?;
                let mut p = Map::new();
                p.insert(prop.to_string(), value);
                (k, p)
            }
        };
        let id = doc.node(target).map(|n| n.id).ok_or_else(|| format!("overrides.{key}: not found"))?;
        // Bindings of the instance's look (fills, strokes, radius) belong to its content root,
        // like the look itself; the instance's own bindings are for its layer properties.
        let (id, props) = match (target == ik, props.get("bindings")) {
            (true, Some(Value::Object(b))) if b.keys().any(|p| is_look(p)) => {
                let root = doc.node(ik).and_then(|n| n.children.first()).and_then(|k| doc.node(*k)).map(|n| n.id).ok_or_else(|| format!("overrides.{key}: the instance shows nothing"))?;
                let (look, own): (Map<String, Value>, Map<String, Value>) = b.clone().into_iter().partition(|(p, _)| is_look(p));
                let mut rest = props.clone();
                rest.insert("bindings".into(), Value::Object(own));
                let mut m = Map::new();
                m.insert("bindings".into(), Value::Object(look));
                let props = crate::api::props_for(st, root, m).map_err(|e| format!("overrides.{key}: {e}"))?;
                st.session.in_tx(|tx| tx.update_json(root, &props)).map_err(|e| format!("overrides.{key}: {}", crate::api::edit_err(e)))?;
                (id, rest)
            }
            _ => (id, props),
        };
        let props = crate::api::props_for(st, id, props).map_err(|e| format!("overrides.{key}: {e}"))?;
        st.session.in_tx(|tx| tx.update_json(id, &props)).map_err(|e| format!("overrides.{key}: {}", crate::api::edit_err(e)))?;
        // Later entries may name nodes inside a nested instance this one just changed.
        st.dirty = true;
        crate::api::sync(st)?;
    }
    Ok(())
}
