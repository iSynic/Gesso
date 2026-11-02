//! The agent guides: Markdown teaching an agent the tools, the document API and good design
//! practice. They are embedded in the binary and served by `read_guide`. The `nodes` guide's
//! property lists are generated from the schema, so they can't drift from the format.

use std::collections::BTreeMap;

use serde_json::{Value, json};

pub const NAMES: [&str; 7] = ["start", "api", "nodes", "layout", "design", "components", "recipes"];

const START: &str = include_str!("../guides/start.md");
const API: &str = include_str!("../guides/api.md");
const NODES: &str = include_str!("../guides/nodes.md");
const LAYOUT: &str = include_str!("../guides/layout.md");
const DESIGN: &str = include_str!("../guides/design.md");
const COMPONENTS: &str = include_str!("../guides/components.md");
const RECIPES: &str = include_str!("../guides/recipes.md");

/// Marker in `nodes.md` replaced by the generated property reference.
const GENERATED: &str = "<!-- generated: properties -->";

pub fn get(name: &str) -> Option<String> {
    Some(match name {
        "start" => START.to_string(),
        "api" => API.to_string(),
        "nodes" => NODES.replace(GENERATED, &properties()),
        "layout" => LAYOUT.to_string(),
        "design" => DESIGN.to_string(),
        "components" => COMPONENTS.to_string(),
        "recipes" => RECIPES.to_string(),
        _ => return None,
    })
}

/// The first heading's text and the first paragraph of a guide.
fn summary(text: &str) -> String {
    text.lines().skip_while(|l| l.starts_with('#') || l.trim().is_empty()).take_while(|l| !l.trim().is_empty()).collect::<Vec<_>>().join(" ")
}

pub fn list() -> Vec<Value> {
    NAMES.iter().map(|n| json!({ "name": n, "summary": get(n).map(|t| summary(&t)).unwrap_or_default() })).collect()
}

pub fn index() -> String {
    let mut s = String::from("# Guides\n\nRead `start` first. Each guide is `read_guide({\"name\": ...})`.\n\n");
    for n in NAMES {
        s.push_str(&format!("- **{n}**: {}\n", get(n).map(|t| summary(&t)).unwrap_or_default()));
    }
    s
}

/// A schema type as short text: `number`, `string | null`, `Paint[]`, …
fn type_text(s: &Value) -> String {
    if let Some(r) = s.get("$ref").and_then(Value::as_str) {
        return r.trim_start_matches("#/$defs/").to_string();
    }
    if let Some(Value::Array(any)) = s.get("anyOf").or_else(|| s.get("oneOf")) {
        return any.iter().map(type_text).collect::<Vec<_>>().join(" | ");
    }
    if let Some(c) = s.get("const") {
        return c.to_string();
    }
    if let Some(Value::Array(e)) = s.get("enum") {
        return e.iter().map(Value::to_string).collect::<Vec<_>>().join(" | ");
    }
    match s.get("type") {
        Some(Value::String(t)) if t == "array" => format!("{}[]", s.get("items").map(type_text).unwrap_or_else(|| "any".into())),
        Some(Value::String(t)) => t.clone(),
        Some(Value::Array(ts)) => ts.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" | "),
        _ => "any".into(),
    }
}

fn describe_props(props: &serde_json::Map<String, Value>, required: &[Value], out: &mut String) {
    for (k, v) in props {
        if k == "type" || k == "children" || k == "id" {
            continue;
        }
        let req = if required.iter().any(|r| r == k) { " (required)" } else { "" };
        let desc = v.get("description").and_then(Value::as_str).map(|d| format!(": {}", d.replace('\n', " "))).unwrap_or_default();
        out.push_str(&format!("- `{k}`: {}{req}{desc}\n", type_text(v)));
    }
}

/// One `oneOf`/`anyOf` alternative as a line: a discriminated object (`type "solid":
/// `color` Color`), a small object (`{fr: number}`), or a plain type.
fn alternative(a: &Value, defs: &Value) -> String {
    let desc = a.get("description").and_then(Value::as_str).map(|d| format!(": {}", d.replace('\n', " "))).unwrap_or_default();
    let Some(props) = a.get("properties").and_then(Value::as_object) else { return format!("{}{desc}", type_text(a)) };
    // Properties of a `$ref` sibling (e.g. a shadow's fields) belong to this branch too.
    let mut all = props.clone();
    if let Some(r) = a.get("$ref").and_then(Value::as_str)
        && let Some(Value::Object(rp)) = defs.pointer(&format!("/{}/properties", r.trim_start_matches("#/$defs/")))
    {
        for (k, v) in rp {
            all.entry(k.clone()).or_insert_with(|| v.clone());
        }
    }
    let tag = ["type", "prop"].into_iter().find_map(|t| all.get(t).and_then(|v| v.get("const")).map(|c| (t, c.to_string())));
    let fields: Vec<String> = all.iter().filter(|(k, _)| tag.as_ref().is_none_or(|(t, _)| k != t)).map(|(k, s)| format!("`{k}` {}", type_text(s))).collect();
    match tag {
        Some((t, c)) => format!("{t} {c}: {}{desc}", fields.join(", ")),
        None => format!("{{{}}}{desc}", fields.join(", ")),
    }
}

/// Every node type's properties and the shared value types, from the schema.
pub fn properties() -> String {
    let schema = format::schema();
    let defs = schema.get("$defs").cloned().unwrap_or_default();
    let mut out = String::new();
    let node = defs.get("NodeTree").cloned().unwrap_or_default();
    out.push_str("### Properties every node has\n\n");
    if let Some(p) = node.get("properties").and_then(Value::as_object) {
        describe_props(p, &[], &mut out);
    }
    out.push_str("\n### Properties by type\n\n");
    for v in node.get("oneOf").and_then(Value::as_array).into_iter().flatten() {
        let Some(ty) = v.pointer("/properties/type/const").and_then(Value::as_str) else { continue };
        out.push_str(&format!("**{ty}**"));
        if let Some(d) = v.get("description").and_then(Value::as_str) {
            out.push_str(&format!(": {}", d.replace('\n', " ")));
        }
        out.push('\n');
        let props = v.get("properties").and_then(Value::as_object).cloned().unwrap_or_default();
        if props.len() <= 1 {
            out.push_str("- (no extra properties)\n");
        }
        let req = v.get("required").and_then(Value::as_array).cloned().unwrap_or_default();
        describe_props(&props, &req, &mut out);
        out.push('\n');
    }
    out.push_str("### Value types\n\n");
    let mut names: BTreeMap<String, Value> = BTreeMap::new();
    if let Some(d) = defs.as_object() {
        for (k, v) in d {
            if k != "NodeTree" && k != "NodeKind" {
                names.insert(k.clone(), v.clone());
            }
        }
    }
    for (k, v) in names {
        out.push_str(&format!("**{k}**"));
        if let Some(d) = v.get("description").and_then(Value::as_str) {
            out.push_str(&format!(": {}", d.replace('\n', " ")));
        }
        out.push('\n');
        if let Some(p) = v.get("properties").and_then(Value::as_object) {
            let req = v.get("required").and_then(Value::as_array).cloned().unwrap_or_default();
            describe_props(p, &req, &mut out);
        }
        if let Some(Value::Array(alts)) = v.get("oneOf").or_else(|| v.get("anyOf")) {
            if v.get("properties").is_some() {
                out.push_str("- and one of:\n");
            }
            for a in alts {
                out.push_str(&format!("- {}\n", alternative(a, &defs)));
            }
        } else if v.get("properties").is_none() {
            out.push_str(&format!("- {}\n", type_text(&v)));
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_guide_loads_and_has_a_summary() {
        for n in NAMES {
            let t = get(n).unwrap_or_default();
            assert!(t.starts_with("# "), "{n} must start with a heading");
            assert!(!summary(&t).is_empty(), "{n} has no summary paragraph");
        }
        assert!(get("nope").is_none());
    }

    #[test]
    fn generated_properties_cover_every_node_type() {
        let p = get("nodes").unwrap_or_default();
        assert!(!p.contains(GENERATED));
        for ty in ["page", "frame", "group", "rectangle", "ellipse", "polygon", "line", "path", "text", "image", "component", "instance", "note"] {
            assert!(p.contains(&format!("**{ty}**")), "missing {ty}");
        }
        assert!(p.contains("`autoLayout`") && p.contains("`spans`") && p.contains("**Paint**"));
    }
}
