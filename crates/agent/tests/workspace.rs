//! The headless host end to end: every request, file roots, and validation of real documents.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use agent::{Host, Request, Workspace};
use serde_json::{Value, json};

fn temp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("agent-ws-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn call(w: &mut Workspace, tool: &str, args: Value) -> agent::Response {
    w.handle(agent::request(tool, args).unwrap())
}

fn ok(w: &mut Workspace, tool: &str, args: Value) -> Value {
    let r = call(w, tool, args);
    assert!(r.is_ok(), "{tool} failed: {:?}", r.error);
    r.result.unwrap_or_default()
}

#[test]
fn requests_end_to_end() {
    let dir = temp("e2e");
    let mut w = Workspace::new(text::FontDb::bundled_only(), vec![dir.clone()]).unwrap();
    let st = ok(&mut w, "get_state", json!({}));
    assert_eq!(st["mode"], "headless");
    assert_eq!(st["document"]["pages"].as_array().unwrap().len(), 1);

    let r = ok(&mut w, "execute", json!({"code": "// label: Card\nconst c = insert(null, {type: 'frame', name: 'Card', width: 200, height: 100, fills: ['#ff0000']});\nselect(c); c"}));
    assert_eq!(r["undoLabel"], "Card");
    let st = ok(&mut w, "get_state", json!({}));
    assert_eq!(st["selection"][0]["name"], "Card");
    assert_eq!(st["undo"], "Card");

    let r = call(&mut w, "screenshot", json!({"scale": 0.5}));
    assert_eq!(r.images.len(), 1);
    assert_eq!(r.images[0].width, 100);
    assert!(agent::b64::decode(&r.images[0].png).unwrap().starts_with(b"\x89PNG"));

    let r = call(&mut w, "execute", json!({"code": "update('x', {})"}));
    let e = r.error.unwrap();
    assert_eq!(e["line"], 1);
    assert!(e["scriptId"].is_string());

    assert!(ok(&mut w, "validate", json!({}))["valid"].as_bool().unwrap());
    assert!(ok(&mut w, "read_guide", json!({}))["guides"].as_array().unwrap().len() >= 6);
    assert!(ok(&mut w, "read_guide", json!({"name": "api"}))["text"].as_str().unwrap().starts_with("# "));
    assert!(call(&mut w, "read_guide", json!({"name": "nope"})).message().unwrap().contains("available"));

    // Save, then open the copy and validate it from disk.
    assert!(call(&mut w, "save", json!({})).message().unwrap().contains("never been saved"));
    ok(&mut w, "save", json!({"path": "card.canvas"}));
    assert!(dir.join("card.canvas").join("document.json").exists());
    let report = ok(&mut w, "validate", json!({"path": "card.canvas"}));
    assert_eq!(report["valid"], true, "{report}");
    ok(&mut w, "new", json!({}));
    let st = ok(&mut w, "open", json!({"path": "card.canvas"}));
    assert_eq!(st["document"]["nodes"], 1);
    assert_eq!(st["document"]["pages"][0]["topLevel"], 1);

    // Undo and redo through requests.
    let mut w2 = Workspace::new(text::FontDb::bundled_only(), vec![]).unwrap();
    ok(&mut w2, "execute", json!({"code": "insert(null, {type: 'ellipse'})"}));
    assert_eq!(w2.handle(Request::Undo(agent::Empty {})).result.unwrap()["undone"], "Script");
    assert_eq!(w2.handle(Request::Redo(agent::Empty {})).result.unwrap()["redone"], "Script");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn paths_stay_inside_the_roots() {
    let dir = temp("roots");
    let outside = temp("outside");
    let mut w = Workspace::new(text::FontDb::bundled_only(), vec![dir.clone()]).unwrap();
    for (tool, args) in [
        ("save", json!({"path": "../x.canvas"})),
        ("save", json!({"path": outside.join("x.canvas").display().to_string()})),
        ("open", json!({"path": outside.display().to_string()})),
        ("validate", json!({"path": "../agent-ws"})),
    ] {
        let r = call(&mut w, tool, args.clone());
        assert!(!r.is_ok(), "{tool} {args} should be refused");
    }
    let mut none = Workspace::new(text::FontDb::bundled_only(), vec![]).unwrap();
    assert!(call(&mut none, "save", json!({"path": "a.canvas"})).message().unwrap().contains("disabled"));
    assert!(agent::request("execute", json!({"cod": "1"})).unwrap_err().contains("cod"));
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(outside);
}

#[test]
fn render_fixtures_validate() {
    // Every golden fixture, built into a document the way the tests do, passes strict validation.
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../render-cpu/tests/fixtures");
    let mut w = Workspace::new(text::FontDb::bundled_only(), vec![]).unwrap();
    let mut n = 0;
    for entry in std::fs::read_dir(fixtures).unwrap() {
        let path = entry.unwrap().path();
        let raw = std::fs::read_to_string(&path).unwrap();
        // Image assets and component references need their files and ids; skip those fixtures.
        if raw.contains("\"asset\"") || raw.contains("\"component\"") {
            continue;
        }
        let f: Value = serde_json::from_str(&raw).unwrap();
        let nodes = f["nodes"].clone();
        if nodes.as_array().is_none_or(|a| a.is_empty()) {
            continue;
        }
        // Fixtures omit ids and text style; insert them through the API, then validate the result.
        let code = format!("for (const n of {nodes}) insert(null, n); find({{}}).length");
        let r = ok(&mut w, "execute", json!({"code": code}));
        assert!(r["value"].as_u64().unwrap() > 0);
        let report = ok(&mut w, "validate", json!({}));
        assert_eq!(report["valid"], true, "{}: {report}", path.display());
        ok(&mut w, "new", json!({}));
        n += 1;
    }
    assert!(n > 20, "only {n} fixtures");
}
