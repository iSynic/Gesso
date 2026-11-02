//! Design tokens for scripts, by name: `setVariables` takes collections whose modes, variables
//! and aliases are named rather than given ids, `getVariables` returns them in the same form,
//! and node `bindings` may name a variable (`"Theme/bg"` or a unique `"bg"`) instead of its id.

use std::collections::BTreeMap;

use doc::variables::{Collection, Mode, Variable, VariableType, VariableValue};
use doc::{Id, Variables};
use serde::Deserialize;
use serde_json::{Map, Value, json};

/// A mode: its name, or `{id?, name}`.
#[derive(Deserialize)]
#[serde(untagged)]
enum ModeIn {
    Name(String),
    Full { id: Option<String>, name: String },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VariableIn {
    id: Option<String>,
    name: String,
    #[serde(rename = "type")]
    ty: VariableType,
    /// Mode name (or id) → value; `{alias: "name"}` refers to another variable.
    #[serde(default)]
    values: Map<String, Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CollectionIn {
    id: Option<String>,
    name: String,
    modes: Vec<ModeIn>,
    /// Mode name or id; default: the first mode.
    default_mode: Option<String>,
    #[serde(default)]
    variables: Vec<VariableIn>,
}

fn parse(s: &str) -> Option<Id> {
    s.parse::<Id>().ok()
}

/// The collections in by-name form (what `getVariables` returns).
pub(crate) fn friendly(v: &Variables) -> Value {
    let cols: Vec<Value> = v
        .collections
        .iter()
        .map(|c| {
            let mode_name = |id: &Id| c.modes.iter().find(|m| m.id == *id).map_or_else(|| id.to_string(), |m| m.name.clone());
            let vars: Vec<Value> = c
                .variables
                .iter()
                .map(|var| {
                    let values: Map<String, Value> = var
                        .values
                        .iter()
                        .map(|(m, val)| {
                            let val = match val {
                                VariableValue::Alias { alias } => json!({ "alias": name_of(v, *alias).unwrap_or_else(|| alias.to_string()) }),
                                other => serde_json::to_value(other).unwrap_or_default(),
                            };
                            (mode_name(m), val)
                        })
                        .collect();
                    json!({ "id": var.id.to_string(), "name": var.name, "type": var.ty, "values": values })
                })
                .collect();
            json!({
                "id": c.id.to_string(),
                "name": c.name,
                "modes": c.modes.iter().map(|m| json!({ "id": m.id.to_string(), "name": m.name })).collect::<Vec<_>>(),
                "defaultMode": mode_name(&c.default_mode),
                "variables": vars,
            })
        })
        .collect();
    Value::Array(cols)
}

/// `Collection/variable` for an id.
fn name_of(v: &Variables, id: Id) -> Option<String> {
    v.collections.iter().find_map(|c| c.variables.iter().find(|x| x.id == id).map(|x| format!("{}/{}", c.name, x.name)))
}

/// A variable's id from its id, `Collection/name`, or a name unique across collections.
pub(crate) fn resolve(v: &Variables, s: &str) -> Result<Id, String> {
    if let Some(id) = parse(s)
        && v.collections.iter().any(|c| c.variables.iter().any(|x| x.id == id))
    {
        return Ok(id);
    }
    let (coll, name) = match s.split_once('/') {
        Some((c, n)) => (Some(c), n),
        None => (None, s),
    };
    let found: Vec<Id> = v.collections.iter().filter(|c| coll.is_none_or(|n| c.name == n)).flat_map(|c| c.variables.iter().filter(|x| x.name == name).map(|x| x.id)).collect();
    match found.as_slice() {
        [id] => Ok(*id),
        [] => {
            let all: Vec<String> = v.collections.iter().flat_map(|c| c.variables.iter().map(move |x| format!("{}/{}", c.name, x.name))).collect();
            Err(format!("no variable {s:?}; the variables are: {}", if all.is_empty() { "(none; call setVariables first)".into() } else { all.join(", ") }))
        }
        _ => Err(format!("{s:?} names more than one variable; write it as \"Collection/{name}\"")),
    }
}

/// Replaces variable names in `bindings`, and collection and mode names in `modes`, by ids (here
/// and in children). The engine then writes each bound property's value for the node's modes.
pub(crate) fn resolve_names(node: &mut Value, v: &Variables) -> Result<(), String> {
    let Some(o) = node.as_object_mut() else { return Ok(()) };
    if let Some(Value::Object(b)) = o.get_mut("bindings") {
        for (path, val) in b.iter_mut() {
            if let Value::String(s) = val {
                let id = resolve(v, s).map_err(|e| format!("bindings.{path}: {e}"))?;
                *val = Value::String(id.to_string());
            }
        }
    }
    if let Some(Value::Object(m)) = o.get_mut("modes") {
        let mut out = Map::new();
        for (coll, mode) in std::mem::take(m) {
            let c = v.collections.iter().find(|c| c.name == coll || parse(&coll) == Some(c.id)).ok_or_else(|| {
                let names: Vec<&str> = v.collections.iter().map(|c| c.name.as_str()).collect();
                format!("modes: no collection {coll:?} (collections: {})", if names.is_empty() { "(none)".into() } else { names.join(", ") })
            })?;
            // `null` clears the choice (`update` merges `modes` into the node's own).
            let id = match mode.as_str() {
                None if mode.is_null() => Value::Null,
                None => return Err(format!("modes.{coll}: give a mode name")),
                Some(s) => {
                    let m = c.modes.iter().find(|m| m.name == s || parse(s) == Some(m.id)).ok_or_else(|| {
                        let names: Vec<&str> = c.modes.iter().map(|m| m.name.as_str()).collect();
                        format!("modes.{coll}: no mode {s:?} (modes: {})", names.join(", "))
                    })?;
                    Value::String(m.id.to_string())
                }
            };
            out.insert(c.id.to_string(), id);
        }
        *m = out;
    }
    if let Some(Value::Array(children)) = o.get_mut("children") {
        for c in children {
            resolve_names(c, v)?;
        }
    }
    Ok(())
}

/// Builds the document's variables from by-name collections. Collections not mentioned are
/// kept; a mentioned collection is replaced, keeping the ids of modes and variables whose names
/// are unchanged so existing bindings stay valid.
pub(crate) fn build(input: Value, current: &Variables, new_id: &mut dyn FnMut() -> Id) -> Result<Variables, String> {
    let input = match input {
        Value::Object(mut o) if o.contains_key("collections") => o.remove("collections").unwrap_or_default(),
        Value::Object(o) => Value::Array(vec![Value::Object(o)]),
        v => v,
    };
    let cols: Vec<CollectionIn> = serde_json::from_value(input).map_err(|e| format!("collections: {e}"))?;
    let mut out = current.clone();
    // First give every collection, mode and variable its id, so aliases can point anywhere.
    let mut built: Vec<(Collection, Vec<Map<String, Value>>)> = Vec::new();
    for c in cols {
        let old = current.collections.iter().find(|x| c.id.as_deref().and_then(parse).is_some_and(|id| id == x.id) || x.name == c.name);
        let id = c.id.as_deref().and_then(parse).or(old.map(|o| o.id)).unwrap_or_else(&mut *new_id);
        if c.modes.is_empty() {
            return Err(format!("collection {:?}: needs at least one mode", c.name));
        }
        let mut modes = Vec::new();
        for m in c.modes {
            let (mid, name) = match m {
                ModeIn::Name(n) => (None, n),
                ModeIn::Full { id, name } => (id, name),
            };
            let mid = mid.as_deref().and_then(parse).or_else(|| old.and_then(|o| o.modes.iter().find(|x| x.name == name)).map(|x| x.id)).unwrap_or_else(&mut *new_id);
            modes.push(Mode { id: mid, name });
        }
        let find_mode = |s: &str| modes.iter().find(|m| m.name == s || parse(s) == Some(m.id)).map(|m| m.id);
        let default_mode = match &c.default_mode {
            Some(d) => find_mode(d).ok_or_else(|| format!("collection {:?}: defaultMode {d:?} is not one of its modes", c.name))?,
            None => modes[0].id,
        };
        let mut vars = Vec::new();
        let mut raw = Vec::new();
        for v in c.variables {
            let vid = v.id.as_deref().and_then(parse).or_else(|| old.and_then(|o| o.variables.iter().find(|x| x.name == v.name)).map(|x| x.id)).unwrap_or_else(&mut *new_id);
            vars.push(Variable { id: vid, name: v.name, ty: v.ty, values: BTreeMap::new() });
            raw.push(v.values);
        }
        built.push((Collection { id, name: c.name, modes, default_mode, variables: vars }, raw));
    }
    // Swap the new collections in, so names resolve against the result.
    for (c, _) in &built {
        match out.collections.iter_mut().find(|x| x.id == c.id) {
            Some(slot) => *slot = c.clone(),
            None => out.collections.push(c.clone()),
        }
    }
    let lookup = out.clone();
    for (c, raw) in built {
        let Some(slot) = out.collections.iter_mut().find(|x| x.id == c.id) else { continue };
        for (var, values) in slot.variables.iter_mut().zip(raw) {
            for (mode, val) in values {
                let mid = c.modes.iter().find(|m| m.name == mode || parse(&mode) == Some(m.id)).map(|m| m.id).ok_or_else(|| {
                    let names: Vec<&str> = c.modes.iter().map(|m| m.name.as_str()).collect();
                    format!("{}/{}: no mode {mode:?} (modes: {})", c.name, var.name, names.join(", "))
                })?;
                let val = match val {
                    Value::Object(o) if o.contains_key("alias") => {
                        let a = o.get("alias").and_then(Value::as_str).ok_or_else(|| format!("{}/{}: alias must be a variable name or id", c.name, var.name))?;
                        // A bare name is looked up in this collection first.
                        let id = resolve(&lookup, &format!("{}/{a}", c.name)).or_else(|_| resolve(&lookup, a)).map_err(|e| format!("{}/{}: {e}", c.name, var.name))?;
                        VariableValue::Alias { alias: id }
                    }
                    Value::String(h) if var.ty == VariableType::Color => VariableValue::String(crate::query::expand_hex(&h)),
                    v => serde_json::from_value(v).map_err(|e| format!("{}/{}: {e}", c.name, var.name))?,
                };
                var.values.insert(mid, val);
            }
        }
    }
    Ok(out)
}
