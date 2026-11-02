//! Document validation for agents and the CLI: every structural problem against the published
//! JSON Schema (with its JSON pointer), strictly (unknown properties are errors, since serde would
//! silently drop a misspelt one), then the document rules (tree, references, components).

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::sync::OnceLock;

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Map, Value};

/// Problems reported at most (the rest are counted).
const MAX_PROBLEMS: usize = 200;

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Problem {
    /// JSON pointer to the offending value, e.g. `/pages/0/children/2/fills/0/color`.
    pub path: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub valid: bool,
    /// The file's format version (0 when missing).
    pub format_version: u32,
    /// Whether a migration to the current version was needed.
    pub migrated: bool,
    pub problems: Vec<Problem>,
    /// Problems found beyond those listed.
    pub more: usize,
    /// Node count, when the document is valid.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nodes: Option<usize>,
}

fn schema() -> &'static Value {
    static S: OnceLock<Value> = OnceLock::new();
    S.get_or_init(format::schema)
}

/// Validates `document.json` bytes.
pub fn validate_bytes(bytes: &[u8]) -> Report {
    match serde_json::from_slice::<Value>(bytes) {
        Ok(v) => validate(v, Some(bytes)),
        Err(e) => Report { valid: false, format_version: 0, migrated: false, problems: vec![Problem { path: String::new(), message: format!("invalid JSON at line {}, column {}: {e}", e.line(), e.column()) }], more: 0, nodes: None },
    }
}

/// Validates a parsed `document.json`.
pub fn validate_value(v: Value) -> Report {
    validate(v, None)
}

/// `bytes`: the text `v` was parsed from, reused for the document build when no migration ran.
fn validate(mut v: Value, bytes: Option<&[u8]>) -> Report {
    let version = v.get("formatVersion").and_then(Value::as_u64).unwrap_or(0) as u32;
    let mut report = Report { valid: false, format_version: version, migrated: false, problems: Vec::new(), more: 0, nodes: None };
    let problem = |message: String| Problem { path: String::new(), message };
    if version > format::FORMAT_VERSION {
        report.problems.push(problem(format!("format version {version} is newer than this build supports ({})", format::FORMAT_VERSION)));
        return report;
    }
    if version < format::FORMAT_VERSION {
        match format::migrate(v) {
            Ok(m) => {
                v = m;
                report.migrated = true;
            }
            Err(e) => {
                report.problems.push(problem(e.to_string()));
                return report;
            }
        }
    }
    let mut c = Checker { out: Vec::new(), more: 0 };
    c.check(schema(), &v, &Path::Root, &BTreeSet::new());
    report.problems = c.out;
    report.more = c.more;
    if !report.problems.is_empty() {
        return report;
    }
    // Structurally sound: build the document for the semantic rules.
    let built = match bytes.filter(|_| !report.migrated) {
        Some(b) => format::from_json(b),
        None => format::from_json(v.to_string().as_bytes()),
    };
    match built {
        Ok(doc) => {
            report.valid = true;
            report.nodes = Some(doc.pages().iter().map(|&p| doc.descendants(p).len()).sum());
        }
        Err(e) => report.problems.push(problem(e.to_string())),
    }
    report
}

/// A JSON pointer built up during the walk and only turned into text for a problem.
enum Path<'a> {
    Root,
    Key(&'a Path<'a>, &'a str),
    Index(&'a Path<'a>, usize),
}

impl Path<'_> {
    fn render(&self) -> String {
        match self {
            Path::Root => String::new(),
            Path::Key(parent, k) => format!("{}/{}", parent.render(), escape(k)),
            Path::Index(parent, i) => format!("{}/{i}", parent.render()),
        }
    }
}

struct Checker {
    out: Vec<Problem>,
    more: usize,
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn is_type(v: &Value, t: &str) -> bool {
    match t {
        "integer" => v.as_f64().is_some_and(|n| n.fract() == 0.0),
        "number" => v.is_number(),
        other => type_name(v) == other,
    }
}

/// The schema's string patterns, checked without a regex engine. A test keeps this list in step
/// with the schema.
fn pattern_ok(pattern: &str, s: &str) -> Option<bool> {
    let hex = |s: &str| s.bytes().all(|b| b.is_ascii_hexdigit());
    Some(match pattern {
        "^#([0-9A-Fa-f]{6}|[0-9A-Fa-f]{8})$" => s.starts_with('#') && matches!(s.len(), 7 | 9) && hex(&s[1..]),
        "^[0-7][0-9A-Za-z]{21}$" => s.len() == 22 && s.bytes().all(|b| b.is_ascii_alphanumeric()) && matches!(s.as_bytes()[0], b'0'..=b'7'),
        "^[0-9a-f]{64}$" => s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        _ => return None,
    })
}

fn describe(pattern: &str) -> &str {
    match pattern {
        "^#([0-9A-Fa-f]{6}|[0-9A-Fa-f]{8})$" => "a colour (#rrggbb or #rrggbbaa)",
        "^[0-7][0-9A-Za-z]{21}$" => "a node id (22 base62 characters)",
        "^[0-9a-f]{64}$" => "an asset id (64 lowercase hex digits)",
        p => p,
    }
}

fn escape(k: &str) -> String {
    k.replace('~', "~0").replace('/', "~1")
}

impl Checker {
    fn push(&mut self, path: &Path<'_>, message: String) {
        self.push_rendered(|| path.render(), message);
    }

    fn push_rendered(&mut self, path: impl FnOnce() -> String, message: String) {
        if self.out.len() < MAX_PROBLEMS {
            self.out.push(Problem { path: path(), message });
        } else {
            self.more += 1;
        }
    }

    /// Follows `$ref`. Keywords beside a `$ref` apply too (draft 2020-12): their `properties` and
    /// `required` are merged into the target's.
    fn resolve(s: &Value) -> Cow<'_, Value> {
        let Some(r) = s.get("$ref").and_then(Value::as_str) else { return Cow::Borrowed(s) };
        let Some(target) = r.strip_prefix("#/$defs/").and_then(|name| schema().get("$defs").and_then(|d| d.get(name))) else { return Cow::Borrowed(s) };
        let target = Self::resolve(target);
        // Annotations beside a `$ref` (schemars adds a description to most) change nothing here,
        // and merging them would copy the target for every value checked.
        const NOTES: [&str; 6] = ["$ref", "description", "title", "default", "examples", "deprecated"];
        let Some(siblings) = s.as_object().filter(|o| o.keys().any(|k| !NOTES.contains(&k.as_str()))) else { return target };
        let mut m = target.into_owned();
        let Some(mo) = m.as_object_mut() else { return Cow::Borrowed(s) };
        for (k, v) in siblings {
            match (k.as_str(), v, mo.get_mut(k)) {
                (k, ..) if NOTES.contains(&k) => {}
                ("properties", Value::Object(p), Some(Value::Object(existing))) => existing.extend(p.clone()),
                ("required", Value::Array(r), Some(Value::Array(existing))) => existing.extend(r.iter().cloned()),
                _ => {
                    mo.insert(k.clone(), v.clone());
                }
            }
        }
        Cow::Owned(m)
    }

    /// Problems `v` would have against `s`, without recording them.
    fn trial(&self, s: &Value, v: &Value, path: &Path<'_>, extra: &BTreeSet<String>) -> Vec<Problem> {
        let mut c = Checker { out: Vec::new(), more: 0 };
        c.check(s, v, path, extra);
        c.out
    }

    /// `extra`: object keys allowed besides the schema's own (from an enclosing `properties` when
    /// this schema is a `oneOf` branch).
    fn check(&mut self, s: &Value, v: &Value, path: &Path<'_>, extra: &BTreeSet<String>) {
        let s = Self::resolve(s);
        let Some(so) = s.as_object() else { return };
        // Type.
        if let Some(t) = so.get("type") {
            let ok = match t {
                Value::String(t) => is_type(v, t),
                Value::Array(ts) => ts.iter().filter_map(Value::as_str).any(|t| is_type(v, t)),
                _ => true,
            };
            if !ok {
                let want = match t {
                    Value::Array(ts) => ts.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" or "),
                    t => t.as_str().unwrap_or("?").to_string(),
                };
                self.push(path, format!("expected {want}, got {}", type_name(v)));
                return;
            }
        }
        if let Some(c) = so.get("const")
            && c != v
        {
            self.push(path, format!("must be {c}"));
        }
        if let Some(Value::Array(e)) = so.get("enum")
            && !e.contains(v)
        {
            let opts: Vec<String> = e.iter().map(Value::to_string).collect();
            self.push(path, format!("{v} is not one of {}", opts.join(", ")));
        }
        if let (Some(p), Value::String(sv)) = (so.get("pattern").and_then(Value::as_str), v)
            && pattern_ok(p, sv) == Some(false)
        {
            self.push(path, format!("{sv:?} is not {}", describe(p)));
        }
        if let Some(n) = v.as_f64() {
            if let Some(min) = so.get("minimum").and_then(Value::as_f64)
                && n < min
            {
                self.push(path, format!("{n} is below the minimum {min}"));
            }
            if let Some(max) = so.get("maximum").and_then(Value::as_f64)
                && n > max
            {
                self.push(path, format!("{n} is above the maximum {max}"));
            }
        }
        if let Value::Array(items) = v {
            if let Some(min) = so.get("minItems").and_then(Value::as_u64)
                && (items.len() as u64) < min
            {
                self.push(path, format!("needs at least {min} items, has {}", items.len()));
            }
            if let Some(max) = so.get("maxItems").and_then(Value::as_u64)
                && (items.len() as u64) > max
            {
                self.push(path, format!("allows at most {max} items, has {}", items.len()));
            }
            if let Some(item) = so.get("items").filter(|i| i.is_object()) {
                for (i, x) in items.iter().enumerate() {
                    self.check(item, x, &Path::Index(path, i), &BTreeSet::new());
                }
            }
        }
        // A `oneOf` over objects discriminated by `type` (node kinds, paints, effects).
        let branch = match (so.get("oneOf"), v) {
            (Some(Value::Array(branches)), Value::Object(o)) => Self::discriminate(branches, o),
            _ => None,
        };
        let discriminated = matches!(so.get("oneOf"), Some(Value::Array(b)) if Self::has_discriminator(b));
        // Keys a `oneOf`/`anyOf` branch may use besides its own: the enclosing ones and these.
        // Built only when there are branches to pass it to.
        let mine: Cow<'_, BTreeSet<String>> = match so.get("properties").and_then(Value::as_object) {
            Some(p) if so.contains_key("oneOf") || so.contains_key("anyOf") => Cow::Owned(extra.iter().cloned().chain(p.keys().cloned()).collect()),
            _ => Cow::Borrowed(extra),
        };
        if let Value::Object(o) = v {
            self.check_object(so, v, o, path, &mine, branch, discriminated);
        }
        match (so.get("oneOf"), branch) {
            (Some(Value::Array(branches)), None) if !matches!(v, Value::Object(o) if o.contains_key("type") && Self::has_discriminator(branches)) => {
                self.alternatives(branches, v, path, &mine);
            }
            (Some(Value::Array(branches)), None) => {
                let types: Vec<String> = branches.iter().filter_map(|b| Self::resolve(b).pointer("/properties/type/const").and_then(Value::as_str).map(str::to_string)).collect();
                let got = v.get("type").map(Value::to_string).unwrap_or_default();
                self.push(&Path::Key(path, "type"), format!("unknown type {got}; expected one of {}", types.join(", ")));
            }
            _ => {}
        }
        if let Some(Value::Array(branches)) = so.get("anyOf") {
            self.alternatives(branches, v, path, &mine);
        }
    }

    fn has_discriminator(branches: &[Value]) -> bool {
        branches.iter().all(|b| Self::resolve(b).pointer("/properties/type/const").is_some())
    }

    fn discriminate<'a>(branches: &'a [Value], o: &Map<String, Value>) -> Option<&'a Value> {
        if !Self::has_discriminator(branches) {
            return None;
        }
        let t = o.get("type")?;
        branches.iter().find(|b| Self::resolve(b).pointer("/properties/type/const") == Some(t))
    }

    /// `anyOf`/undiscriminated `oneOf`: fine if any branch fits; otherwise the closest branch's
    /// problems.
    fn alternatives(&mut self, branches: &[Value], v: &Value, path: &Path<'_>, extra: &BTreeSet<String>) {
        let mut best: Option<Vec<Problem>> = None;
        for b in branches {
            let p = self.trial(b, v, path, extra);
            if p.is_empty() {
                return;
            }
            if best.as_ref().is_none_or(|bp| p.len() < bp.len()) {
                best = Some(p);
            }
        }
        for p in best.unwrap_or_default() {
            self.push_rendered(|| p.path, p.message);
        }
    }

    /// `discriminated`: the schema picks a `oneOf` branch by `type`; unknown keys are then left to
    /// the branch, which knows its own properties as well as these. Likewise any schema with
    /// `oneOf`/`anyOf` branches leaves unknown keys to them. `mine`: this schema's keys plus the
    /// enclosing ones.
    #[allow(clippy::too_many_arguments)]
    fn check_object(&mut self, so: &Map<String, Value>, v: &Value, o: &Map<String, Value>, path: &Path<'_>, mine: &BTreeSet<String>, branch: Option<&Value>, discriminated: bool) {
        let props = so.get("properties").and_then(Value::as_object);
        let branch = branch.map(Self::resolve);
        let branch_props = branch.as_ref().and_then(|b| b.get("properties")).and_then(Value::as_object);
        let deferred = discriminated || so.contains_key("oneOf") || so.contains_key("anyOf");
        if let Some(Value::Array(req)) = so.get("required") {
            for r in req.iter().filter_map(Value::as_str) {
                if !o.contains_key(r) {
                    self.push(path, format!("missing required property {r:?}"));
                }
            }
        }
        let patterns = so.get("patternProperties").and_then(Value::as_object);
        let additional = so.get("additionalProperties");
        for (k, val) in o {
            let kp = Path::Key(path, k);
            if let Some(sub) = props.and_then(|p| p.get(k)) {
                self.check(sub, val, &kp, &BTreeSet::new());
                continue;
            }
            if deferred || mine.contains(k) || branch_props.is_some_and(|b| b.contains_key(k)) {
                continue;
            }
            if let Some(pp) = patterns {
                let mut matched = false;
                for (pat, sub) in pp {
                    if pattern_ok(pat, k) != Some(false) {
                        matched = true;
                        self.check(sub, val, &kp, &BTreeSet::new());
                    }
                }
                if matched {
                    continue;
                }
                if additional == Some(&Value::Bool(false)) {
                    self.push(&kp, format!("key {k:?} is not {}", pp.keys().next().map_or("allowed", |p| describe(p))));
                    continue;
                }
            }
            match additional {
                Some(sub @ Value::Object(_)) => self.check(sub, val, &kp, &BTreeSet::new()),
                Some(Value::Bool(true)) => {}
                // Strict: serde would silently ignore it.
                _ if props.is_some() => self.push(&kp, format!("unknown property {k:?}")),
                _ => {}
            }
        }
        if let Some(b) = &branch {
            self.check(b, v, path, mine);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn doc(pages: Value) -> Value {
        json!({"formatVersion": 2, "pages": pages})
    }

    #[test]
    fn every_schema_pattern_is_known() {
        fn walk(v: &Value, out: &mut Vec<String>) {
            match v {
                Value::Object(o) => {
                    if let Some(Value::String(p)) = o.get("pattern") {
                        out.push(p.clone());
                    }
                    if let Some(Value::Object(pp)) = o.get("patternProperties") {
                        out.extend(pp.keys().cloned());
                    }
                    o.values().for_each(|x| walk(x, out));
                }
                Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
                _ => {}
            }
        }
        let mut pats = Vec::new();
        walk(schema(), &mut pats);
        assert!(!pats.is_empty());
        for p in pats {
            assert!(pattern_ok(&p, "").is_some(), "validator doesn't know pattern {p}");
        }
    }

    #[test]
    fn generated_documents_pass() {
        // Components, instances with overrides, variables, paths and text, as the benchmarks use.
        let data = doc::testing::generate(&doc::testing::GenConfig { target_nodes: 3000, ..Default::default() });
        let json = format::data_to_json(&data);
        assert!(json.contains("\"prop\""), "the generator should make overrides");
        let r = validate_bytes(json.as_bytes());
        assert!(r.valid, "{:?}", r.problems);
    }

    #[test]
    fn valid_documents_pass() {
        let r = validate_value(doc(json!([{"id": "0000000000000000000001", "type": "page", "name": "P", "children": [
            {"id": "0000000000000000000002", "type": "text", "spans": [{"text": "Hi"}], "fills": [{"type": "solid", "color": "#112233"}]},
            {"id": "0000000000000000000003", "type": "frame", "autoLayout": {"direction": "vertical", "padding": [1, 2, 3, 4]}}
        ]}])));
        assert!(r.valid, "{:?}", r.problems);
        assert_eq!(r.nodes, Some(3));
    }

    #[test]
    fn lists_every_problem_with_its_path() {
        let r = validate_value(doc(json!([{"id": "0000000000000000000001", "type": "page", "children": [
            {"id": "0000000000000000000002", "type": "rectangle", "fil": [], "fills": [{"type": "solid", "color": "red"}]},
            {"id": "0000000000000000000003", "type": "blob"},
            {"id": "0000000000000000000004", "type": "text"},
            {"id": "0000000000000000000005", "type": "frame", "autoLayout": {"padding": [1, 2]}, "opacity": "x"}
        ]}])));
        assert!(!r.valid);
        let got: Vec<(String, String)> = r.problems.iter().map(|p| (p.path.clone(), p.message.clone())).collect();
        let has = |path: &str, msg: &str| got.iter().any(|(p, m)| p == path && m.contains(msg));
        assert!(has("/pages/0/children/0/fil", "unknown property"), "{got:?}");
        assert!(has("/pages/0/children/0/fills/0/color", "is not a colour"), "{got:?}");
        assert!(has("/pages/0/children/1/type", "unknown type"), "{got:?}");
        assert!(has("/pages/0/children/2", "missing required property \"spans\""), "{got:?}");
        assert!(has("/pages/0/children/3/autoLayout/padding", "at least 4"), "{got:?}");
        assert!(has("/pages/0/children/3/opacity", "expected number"), "{got:?}");
        assert_eq!(got.len(), 6, "{got:?}");
    }

    #[test]
    fn document_rules_after_the_schema() {
        let r = validate_value(doc(json!([{"id": "0000000000000000000001", "type": "page", "children": [
            {"id": "0000000000000000000002", "type": "rectangle", "children": [{"id": "0000000000000000000003", "type": "ellipse"}]}
        ]}])));
        assert!(!r.valid);
        assert!(r.problems[0].message.contains("cannot have children"), "{:?}", r.problems);
        let r = validate_bytes(b"{ not json");
        assert!(r.problems[0].message.starts_with("invalid JSON"));
        let r = validate_value(json!({"formatVersion": 99, "pages": []}));
        assert!(r.problems[0].message.contains("newer"));
    }
}
