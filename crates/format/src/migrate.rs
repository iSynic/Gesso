//! Format migrations. `STEPS[i]` turns version `i` into version `i + 1`; files are migrated as
//! untyped JSON and only then deserialised into the current types.

use serde_json::Value;

use crate::{FORMAT_VERSION, FormatError};

type Step = fn(Value) -> Result<Value, String>;

const STEPS: [Step; FORMAT_VERSION as usize] = [v0_to_v1, v1_to_v2];

/// Migrates a parsed file of any older version to the current one.
pub fn migrate(mut v: Value) -> Result<Value, FormatError> {
    let from = v.get("formatVersion").and_then(Value::as_u64).unwrap_or(0) as u32;
    if from > FORMAT_VERSION {
        return Err(FormatError::TooNew(from));
    }
    for (i, step) in STEPS.iter().enumerate().skip(from as usize) {
        v = step(v).map_err(|msg| FormatError::Migration { from: i as u32, msg })?;
    }
    Ok(v)
}

/// Version 0 was the pre-release layout: no `formatVersion`, top-level `nodes` instead of
/// `pages`, and `cornerRadius` instead of `radius`.
fn v0_to_v1(mut v: Value) -> Result<Value, String> {
    let obj = v.as_object_mut().ok_or("top level is not an object")?;
    if let Some(nodes) = obj.remove("nodes") {
        obj.insert("pages".into(), nodes);
    }
    fn rename(node: &mut Value) {
        if let Some(o) = node.as_object_mut() {
            if let Some(r) = o.remove("cornerRadius") {
                o.insert("radius".into(), r);
            }
            if let Some(children) = o.get_mut("children").and_then(Value::as_array_mut) {
                children.iter_mut().for_each(rename);
            }
        }
    }
    obj.get_mut("pages").and_then(Value::as_array_mut).ok_or("missing pages")?.iter_mut().for_each(rename);
    // Keep formatVersion as the first key.
    let mut out = serde_json::Map::new();
    out.insert("formatVersion".into(), 1.into());
    out.extend(std::mem::take(obj));
    Ok(Value::Object(out))
}

/// Version 2 addresses overrides by id path, so a nested node can be reached through the
/// instances above it. A v1 `target` id becomes a one-step path, and a missing one the empty
/// path (the instance itself), as does a target naming the component root.
///
/// In v2 an instance's fills, strokes and corner radius come from its component root plus its
/// `[]` overrides. An instance that set its own value different from the component's gets it as
/// a `[]` override (unless it already had one for that property), so it keeps its look.
fn v1_to_v2(mut v: Value) -> Result<Value, String> {
    const LOOK: [&str; 3] = ["fills", "strokes", "radius"];
    type Looks = std::collections::HashMap<String, serde_json::Map<String, Value>>;
    fn components(node: &Value, out: &mut Looks) {
        let Some(o) = node.as_object() else { return };
        if o.get("type").and_then(Value::as_str) == Some("component")
            && let Some(id) = o.get("id").and_then(Value::as_str)
        {
            let look = LOOK.iter().filter_map(|k| o.get(*k).map(|v| (k.to_string(), v.clone()))).collect();
            out.insert(id.to_string(), look);
        }
        for c in o.get("children").and_then(Value::as_array).into_iter().flatten() {
            components(c, out);
        }
    }
    fn walk(node: &mut Value, looks: &Looks) {
        let Some(o) = node.as_object_mut() else { return };
        let component = o.get("component").and_then(Value::as_str).map(str::to_string);
        if let Some(overrides) = o.get_mut("overrides").and_then(Value::as_array_mut) {
            for ov in overrides.iter_mut().filter_map(Value::as_object_mut) {
                let path = match ov.remove("target") {
                    Some(Value::String(id)) if component.as_deref() != Some(id.as_str()) => vec![Value::String(id)],
                    Some(Value::Array(path)) => path,
                    _ => Vec::new(),
                };
                ov.insert("target".into(), Value::Array(path));
            }
        }
        if o.get("type").and_then(Value::as_str) == Some("instance") {
            let comp = component.as_ref().and_then(|c| looks.get(c));
            for key in LOOK {
                let Some(own) = o.remove(key) else { continue };
                if comp.and_then(|c| c.get(key)) == Some(&own) {
                    continue;
                }
                let list = o.entry("overrides").or_insert_with(|| Value::Array(Vec::new()));
                let Some(list) = list.as_array_mut() else { continue };
                let has = list.iter().any(|ov| ov.get("prop").and_then(Value::as_str) == Some(key) && ov.get("target").and_then(Value::as_array).is_some_and(Vec::is_empty));
                if !has {
                    list.push(serde_json::json!({"target": [], "prop": key, "value": own}));
                }
            }
        }
        if let Some(children) = o.get_mut("children").and_then(Value::as_array_mut) {
            children.iter_mut().for_each(|c| walk(c, looks));
        }
    }
    let obj = v.as_object_mut().ok_or("top level is not an object")?;
    let pages = obj.get_mut("pages").and_then(Value::as_array_mut).ok_or("missing pages")?;
    let mut looks = Looks::new();
    pages.iter().for_each(|p| components(p, &mut looks));
    pages.iter_mut().for_each(|p| walk(p, &looks));
    obj.insert("formatVersion".into(), 2.into());
    Ok(v)
}
