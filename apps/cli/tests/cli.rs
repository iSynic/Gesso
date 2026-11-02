//! The `canvas` binary end to end: run a script into a new document, validate it, render it,
//! and the exit codes for script errors, invalid documents and bad usage.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("canvas-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn canvas(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_canvas")).current_dir(dir).args(args).arg("--deterministic").output().unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn run_validate_render() {
    let dir = temp("e2e");
    std::fs::write(
        dir.join("card.js"),
        "// label: Card\nconst c = insert(null, {type: 'frame', name: 'Card', width: 320, height: 200, fills: ['#ffffff']});\ninsert(c, {type: 'text', text: 'Hi'});\nc\n",
    )
    .unwrap();
    let o = canvas(&dir, &["run", "card.js", "--seed", "7", "-o", "card.canvas", "--screenshot", "page.png"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let r: serde_json::Value = serde_json::from_str(&stdout(&o)).unwrap();
    assert_eq!(r["undoLabel"], "Card");
    assert_eq!(r["created"].as_array().unwrap().len(), 2);
    assert!(std::fs::read(dir.join("page.png")).unwrap().starts_with(b"\x89PNG"));

    // Seeded ids are the same on a second run.
    let again = canvas(&dir, &["run", "card.js", "--seed", "7"]);
    let r2: serde_json::Value = serde_json::from_str(&stdout(&again)).unwrap();
    assert_eq!(r["created"], r2["created"]);

    let o = canvas(&dir, &["validate", "card.canvas"]);
    assert!(o.status.success());
    assert!(stdout(&o).contains("valid"));

    let o = canvas(&dir, &["render", "card.canvas", "--page", "Page 1", "--scale", "0.5", "-o", "r.png"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let png = std::fs::read(dir.join("r.png")).unwrap();
    // PNG width at bytes 16..20: a 320-unit frame at half scale.
    assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 160);

    let o = canvas(&dir, &["state", "card.canvas"]);
    let st: serde_json::Value = serde_json::from_str(&stdout(&o)).unwrap();
    assert_eq!(st["document"]["nodes"], 2);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn failures_have_exit_codes() {
    let dir = temp("fail");
    std::fs::write(dir.join("bad.js"), "const a = 1;\nupdate('x', {});\n").unwrap();
    let o = canvas(&dir, &["run", "bad.js"]);
    assert_eq!(o.status.code(), Some(1));
    let e: serde_json::Value = serde_json::from_str(&stdout(&o)).unwrap();
    assert_eq!(e["error"]["line"], 2);

    std::fs::write(dir.join("doc.json"), r#"{"formatVersion": 1, "nodes": 3}"#).unwrap();
    let o = canvas(&dir, &["validate", "doc.json", "--json"]);
    assert_eq!(o.status.code(), Some(1));
    let rep: serde_json::Value = serde_json::from_str(&stdout(&o)).unwrap();
    assert_eq!(rep["valid"], false);
    assert!(!rep["problems"].as_array().unwrap().is_empty());

    assert_eq!(canvas(&dir, &["render", "x.canvas"]).status.code(), Some(2));
    assert_eq!(canvas(&dir, &["run", "bad.js", "--sed", "1"]).status.code(), Some(2));
    assert_eq!(canvas(&dir, &["frobnicate"]).status.code(), Some(2));
    assert!(canvas(&dir, &["guide", "api"]).status.success());
    assert!(stdout(&canvas(&dir, &["schema"])).contains("$defs"));
    let _ = std::fs::remove_dir_all(dir);
}
