//! A scripted session through the real app: window, egui, Vello, the editor controller. It
//! needs a GPU and a desktop, so it runs only with `GPU_TESTS=1` (like the renderer's GPU
//! parity tests).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use doc::testing::{GenConfig, generate};
use serde_json::{Value, json};

#[test]
fn edits_then_undo_all_restore_the_saved_document() {
    if std::env::var("GPU_TESTS").is_err() {
        eprintln!("skipped: set GPU_TESTS=1 to run");
        return;
    }
    let dir = std::env::temp_dir().join(format!("desktop-script-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let doc = doc::Document::from_data(generate(&GenConfig { target_nodes: 3000, ..Default::default() })).unwrap();
    format::Bundle::create(dir.join("in.canvas")).and_then(|b| b.save(&doc)).unwrap();
    let script = json!({
        "document": "in.canvas",
        "window": [1280, 800],
        "steps": [
            {"page": "Page 1"},
            {"save": "before.canvas"},
            {"reveal": "Screen 1", "at": 0.5},
            {"select": "Screen 1"},
            {"drag": {"node": "Screen 1", "by": [120, 60], "frames": 10}},
            {"drag": {"handle": "se", "by": [-200, 80], "frames": 10, "mods": "shift"}},
            {"select": "Card 1"},
            {"drag": {"node": "Card 1", "by": [-260, 0], "frames": 10}},
            {"select": "Card 0"},
            {"panel": {"path": "width", "values": [300, 250]}},
            {"panel": {"path": "rotation", "values": [15]}},
            {"fill": ["#ff4060"]},
            {"command": "duplicate"},
            {"command": "group"},
            {"command": "delete"},
            {"undoall": true},
            {"save": "after.canvas"}
        ]
    });
    std::fs::write(dir.join("script.json"), serde_json::to_vec_pretty(&script).unwrap()).unwrap();
    let out = dir.join("result.json");
    let status = Command::new(env!("CARGO_BIN_EXE_gesso")).arg("--script").arg(dir.join("script.json")).arg("--out").arg(&out).status().unwrap();
    assert!(status.success());
    let result: Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
    assert_eq!(result["log"], json!([]), "{result}");
    assert_eq!(result["undo_label"], Value::Null);
    let read = |name: &str| std::fs::read_to_string(dir.join(name).join("document.json")).unwrap();
    assert_eq!(read("before.canvas"), read("after.canvas"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn component_override_session_then_undo_all() {
    if std::env::var("GPU_TESTS").is_err() {
        eprintln!("skipped: set GPU_TESTS=1 to run");
        return;
    }
    let dir = std::env::temp_dir().join(format!("desktop-components-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let doc = doc::Document::from_data(generate(&GenConfig { target_nodes: 3000, ..Default::default() })).unwrap();
    format::Bundle::create(dir.join("in.canvas")).and_then(|b| b.save(&doc)).unwrap();
    let script = json!({
        "document": "in.canvas",
        "window": [1280, 800],
        "steps": [
            {"page": "Page 1"},
            {"save": "before.canvas"},
            {"reveal": "Card 0", "at": 1.0},
            {"select": "Card 0"},
            {"command": "createcomponent"},
            {"instance": "Card 0", "at": [3000, 3000]},
            {"reveal": "Card 0", "at": 1.0},
            {"fill": ["#16a34a"]},
            {"enter": true},
            {"fill": ["#ff4060"]},
            {"panel": {"path": "opacity", "values": [0.5]}},
            {"screenshot": "instance.png"},
            {"save": "mid.canvas"},
            {"command": "resetoverrides"},
            {"command": "detach"},
            {"undoall": true},
            {"save": "after.canvas"}
        ]
    });
    std::fs::write(dir.join("script.json"), serde_json::to_vec_pretty(&script).unwrap()).unwrap();
    let out = dir.join("result.json");
    let status = Command::new(env!("CARGO_BIN_EXE_gesso")).arg("--script").arg(dir.join("script.json")).arg("--out").arg(&out).status().unwrap();
    assert!(status.success());
    let result: Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
    assert_eq!(result["log"], json!([]), "{result}");
    let read = |name: &str| std::fs::read_to_string(dir.join(name).join("document.json")).unwrap();
    // Mid-session the instance carries overrides: its look, and edits made inside it.
    let mid: Value = serde_json::from_str(&read("mid.canvas")).unwrap();
    let mut overrides = 0;
    let mut stack = vec![&mid];
    while let Some(v) = stack.pop() {
        if v["type"] == "instance" {
            overrides += v["overrides"].as_array().map_or(0, Vec::len);
        }
        match v {
            Value::Array(a) => stack.extend(a),
            Value::Object(o) => stack.extend(o.values()),
            _ => {}
        }
    }
    assert!(overrides >= 2, "expected overrides on the instance, found {overrides}");
    assert_eq!(read("before.canvas"), read("after.canvas"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn killed_mid_session_the_journal_recovers_the_work() {
    if std::env::var("GPU_TESTS").is_err() {
        eprintln!("skipped: set GPU_TESTS=1 to run");
        return;
    }
    let dir = std::env::temp_dir().join(format!("desktop-crash-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let doc = doc::Document::from_data(generate(&GenConfig { target_nodes: 3000, ..Default::default() })).unwrap();
    format::Bundle::create(dir.join("in.canvas")).and_then(|b| b.save(&doc)).unwrap();
    // Edit, save the expected result elsewhere (the journal stays on in.canvas), then idle until
    // killed.
    let script = json!({
        "document": "in.canvas",
        "window": [1280, 800],
        "steps": [
            {"page": "Page 1"},
            {"reveal": "Screen 1", "at": 0.5},
            {"select": "Screen 1"},
            {"drag": {"node": "Screen 1", "by": [120, 60], "frames": 30}},
            {"select": "Card 0"},
            {"panel": {"path": "width", "values": [300, 250]}},
            {"command": "duplicate"},
            {"save": "expected.canvas"},
            {"wait": 10_000_000}
        ]
    });
    std::fs::write(dir.join("script.json"), serde_json::to_vec_pretty(&script).unwrap()).unwrap();
    let appdata = dir.join("appdata");
    let mut child = Command::new(env!("CARGO_BIN_EXE_gesso"))
        .arg("--script")
        .arg(dir.join("script.json"))
        .arg("--autosave")
        .env("LOCALAPPDATA", &appdata)
        .env("XDG_DATA_HOME", &appdata)
        .spawn()
        .unwrap();
    let recovery = appdata.join("ai-design-engine").join("recovery");
    let t = std::time::Instant::now();
    let found = loop {
        let found = editor::autosave::find(&recovery);
        if !found.is_empty() && dir.join("expected.canvas").join("document.json").exists() {
            break found;
        }
        assert!(t.elapsed().as_secs() < 30, "no journal after 30 s");
        std::thread::sleep(std::time::Duration::from_millis(200));
    };
    // Let the last batch land (it is written 2 s after the last edit), then crash.
    std::thread::sleep(std::time::Duration::from_millis(3000));
    child.kill().unwrap();
    let _ = child.wait();
    let found = editor::autosave::find(&recovery).into_iter().next().unwrap_or_else(|| found[0].clone());
    let recovered = editor::autosave::load(&found).unwrap();
    let expected = std::fs::read_to_string(dir.join("expected.canvas").join("document.json")).unwrap();
    assert_eq!(format::to_json(&recovered), expected);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn agent_requests_over_the_pipe_edit_the_live_document() {
    if std::env::var("GPU_TESTS").is_err() {
        eprintln!("skipped: set GPU_TESTS=1 to run");
        return;
    }
    let dir = std::env::temp_dir().join(format!("desktop-agent-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let doc = doc::Document::from_data(generate(&GenConfig { target_nodes: 3000, ..Default::default() })).unwrap();
    format::Bundle::create(dir.join("in.canvas")).and_then(|b| b.save(&doc)).unwrap();
    let (edits, pings) = (10, 200);
    let total = 1 + edits + 2 + pings;
    let script = json!({
        "document": "in.canvas",
        "window": [1280, 800],
        "steps": [{"page": "Page 1"}, {"fit": true}, {"agent": total, "measure": "agent_edit"}, {"save": "after.canvas"}]
    });
    std::fs::write(dir.join("script.json"), serde_json::to_vec_pretty(&script).unwrap()).unwrap();
    let socket = format!("ai-design-engine-test-{}.sock", std::process::id());
    let out = dir.join("result.json");
    let child = Command::new(env!("CARGO_BIN_EXE_gesso")).arg("--script").arg(dir.join("script.json")).arg("--out").arg(&out).args(["--agent-socket", &socket]).spawn().unwrap();
    // The app idles until it has had every request, so a failed assertion must not leave it running.
    struct Kill(std::process::Child);
    impl Drop for Kill {
        fn drop(&mut self) {
            let _ = self.0.kill();
        }
    }
    let mut child = Kill(child);
    let t = std::time::Instant::now();
    let mut c = loop {
        if let Ok(c) = agent::ipc::Client::connect(Some(&socket), "test") {
            break c;
        }
        assert!(t.elapsed().as_secs() < 30, "the app didn't start listening");
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    let mut call = |req: agent::Request| {
        let r = c.call(&req).unwrap();
        assert!(r.is_ok(), "{}: {:?}", req.name(), r.error);
        r
    };
    let st = call(agent::Request::GetState(agent::Empty {})).result.unwrap();
    assert_eq!(st["mode"], "app");
    for i in 0..edits {
        let code = format!(
            "// label: Agent card {i}\ninsert(null, {{type: 'frame', name: 'Agent {i}', x: {x}, y: -2000, width: 360, height: 600, fills: ['#ffffff'], autoLayout: {{direction: 'vertical', gap: 4, padding: 8}}, children: Array.from({{length: 49}}, (_, j) => ({{type: 'rectangle', name: 'Row ' + j, width: 300, height: 8, fills: ['#88aacc']}}))}})",
            x = i * 400
        );
        let r = call(agent::Request::Execute(script::Execute { code: Some(code), ..Default::default() }));
        // One tree insert: the frame and its 49 rows.
        assert_eq!(r.result.unwrap()["changes"]["inserted"], 1);
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let r = call(agent::Request::Screenshot(agent::Screenshot { scale: Some(0.25), ..Default::default() }));
    assert_eq!(r.images.len(), 1);
    let r = call(agent::Request::Undo(agent::Empty {}));
    assert_eq!(r.result.unwrap()["undone"], format!("Agent card {}", edits - 1));
    let mut rt = Vec::new();
    for _ in 0..pings {
        let t = std::time::Instant::now();
        call(agent::Request::GetState(agent::Empty {}));
        rt.push(t.elapsed().as_secs_f64() * 1e3);
    }
    rt.sort_by(f64::total_cmp);
    eprintln!("get_state round trip over the pipe: p50 {:.3} ms, p95 {:.3} ms", rt[pings / 2], rt[pings * 95 / 100]);
    assert!(child.0.wait().unwrap().success());
    let result: Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
    assert_eq!(result["log"], json!([]), "{result}");
    eprintln!("agent edit to GPU done: {}", result["metrics"]["agent_edit"]["input_to_gpu_done"]);
    assert_eq!(result["undo_label"], format!("Agent card {}", edits - 2));
    let after = std::fs::read_to_string(dir.join("after.canvas").join("document.json")).unwrap();
    assert_eq!(after.matches("\"Agent ").count(), edits - 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn text_editing_ime_and_pen_session_then_undo_all() {
    if std::env::var("GPU_TESTS").is_err() {
        eprintln!("skipped: set GPU_TESTS=1 to run");
        return;
    }
    let dir = std::env::temp_dir().join(format!("desktop-canvas-edit-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let doc = doc::Document::from_data(generate(&GenConfig { target_nodes: 3000, ..Default::default() })).unwrap();
    format::Bundle::create(dir.join("in.canvas")).and_then(|b| b.save(&doc)).unwrap();
    let script = json!({
        "document": "in.canvas",
        "window": [1280, 800],
        "steps": [
            {"page": "Page 1"},
            {"save": "before.canvas"},
            {"reveal": "Card 0", "at": 2.0},
            {"edit": "Title"},
            {"type": "Typed on the canvas", "measure": "keystroke"},
            {"key": "backspace", "mods": "ctrl"},
            {"ime": {"preedit": "にほ"}},
            {"ime": {"preedit": "日本"}},
            {"screenshot": "composing.png"},
            {"ime": {"commit": "日本語"}},
            {"key": "left", "mods": "ctrl+shift"},
            {"key": "b", "mods": "ctrl"},
            {"screenshot": "text.png"},
            {"save": "mid.canvas"},
            {"key": "escape"},
            {"pen": [[-400, -400], [-300, -400, 30, 40], [-300, -300]], "close": true},
            {"edit": "Path"},
            {"screenshot": "path.png"},
            {"key": "escape"},
            {"undoall": true},
            {"save": "after.canvas"},
            {"uiscale": 1.5},
            {"wait": 3},
            {"zoommenu": true},
            {"wait": 3},
            {"screenshot": "scaled.png"}
        ]
    });
    std::fs::write(dir.join("script.json"), serde_json::to_vec_pretty(&script).unwrap()).unwrap();
    let out = dir.join("result.json");
    let status = Command::new(env!("CARGO_BIN_EXE_gesso")).arg("--script").arg(dir.join("script.json")).arg("--out").arg(&out).status().unwrap();
    assert!(status.success());
    let result: Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
    assert_eq!(result["log"], json!([]), "{result}");
    let read = |name: &str| std::fs::read_to_string(dir.join(name).join("document.json")).unwrap();
    let mid = read("mid.canvas");
    let found = mid.find("Typed on").map(|i| &mid[i..(i + 60).min(mid.len())]);
    // The composed word was then selected with Ctrl+Shift+Left and toggled with Ctrl+B (the
    // title is bold, so it became regular), so it is a span of its own.
    assert!(mid.contains("\"Typed on the \"") && mid.contains("\"日本語\""), "the typed and composed text is in the document: {found:?}");
    assert!(mid.contains("\"fontWeight\": 400"), "the composed word was restyled");
    assert!(!mid.contains("日本\""), "composition text alone never reaches the file");
    assert_eq!(read("before.canvas"), read("after.canvas"));
    // Keystroke to presented frame in the real app.
    eprintln!("keystroke: {}", result["metrics"]["keystroke"]["input_to_gpu_done"]);
    if let Ok(keep) = std::env::var("KEEP_SHOTS") {
        for f in ["composing.png", "text.png", "path.png"] {
            let _ = std::fs::copy(dir.join(f), std::path::Path::new(&keep).join(f));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
