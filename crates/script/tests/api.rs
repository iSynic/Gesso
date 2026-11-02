//! The script API against a real session: edits, reads, errors, limits, undo and retries.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::result_large_err)]

use std::time::Duration;

use editor::Session;
use script::{Engine, Execute, LineEdit, Options};
use serde_json::{Value, json};

fn session() -> Session {
    Session::untitled(text::FontDb::bundled_only()).unwrap()
}

fn run(e: &mut Engine, s: &mut Session, code: &str) -> Result<script::Outcome, script::ScriptError> {
    e.execute(s, &Execute { code: Some(code.into()), ..Default::default() }, &Options::default())
}

fn ok(e: &mut Engine, s: &mut Session, code: &str) -> script::Outcome {
    match run(e, s, code) {
        Ok(o) => o,
        Err(err) => panic!("script failed: {err} ({err:?})"),
    }
}

fn doc_json(s: &Session) -> String {
    format::to_json(s.doc())
}

const CARD: &str = r##"
// label: Add card
const card = insert(null, { type: "frame", name: "Card", x: 100, y: 50, width: 320, sizingV: "hug",
  fills: ["#ffffff"], autoLayout: { direction: "vertical", gap: 12, padding: 16 } });
const title = insert(card, { type: "text", name: "Title", text: "Hello", style: { fontSize: 24, fontWeight: 700 }, sizingH: "fill" });
insert(card, { type: "rectangle", name: "Bar", height: 8, sizingH: "fill", fills: ["#2563eb"] });
const t = get(title);
({ card, title, titleBounds: t.bounds, cardHeight: get(card).bounds.height, text: t.text })
"##;

#[test]
fn builds_with_layout_visible_to_reads() {
    let (mut e, mut s) = (Engine::new(), session());
    let o = ok(&mut e, &mut s, CARD);
    let v = &o.value;
    assert_eq!(v["text"], "Hello");
    // Laid out inside the script: padding 16 from the card's origin.
    assert_eq!(v["titleBounds"]["x"].as_f64(), Some(116.0));
    assert_eq!(v["titleBounds"]["y"].as_f64(), Some(66.0));
    let h = v["cardHeight"].as_f64().unwrap();
    assert!(h > 16.0 + 24.0 + 12.0 + 8.0 + 16.0 - 1.0, "card hugs its content: {h}");
    assert_eq!(o.created.len(), 3);
    assert_eq!(o.undo_label.as_deref(), Some("Add card"));
    assert_eq!(o.changes.inserted, 3);
}

#[test]
fn a_script_is_one_undo_step() {
    let (mut e, mut s) = (Engine::new(), session());
    let before = doc_json(&s);
    ok(&mut e, &mut s, CARD);
    assert_ne!(doc_json(&s), before);
    assert_eq!(s.undo().unwrap().as_deref(), Some("Add card"));
    assert_eq!(doc_json(&s), before);
}

#[test]
fn an_error_rolls_everything_back_and_reports_the_line() {
    let (mut e, mut s) = (Engine::new(), session());
    ok(&mut e, &mut s, CARD);
    let before = doc_json(&s);
    let label = s.store().undo_label().map(str::to_string);
    let err = run(&mut e, &mut s, "const f = insert(null, { type: \"frame\", width: 10 });\nprint(\"made\", f);\nupdate(f, { fil: 1 });\n").unwrap_err();
    assert_eq!(err.line, Some(3), "{err:?}");
    assert!(err.message.contains("update") && err.message.contains("unknown property \"fil\""), "{}", err.message);
    assert_eq!(err.source_line.as_deref(), Some("update(f, { fil: 1 });"));
    assert_eq!(err.logs.len(), 1);
    assert_eq!(doc_json(&s), before, "rolled back");
    assert_eq!(s.store().undo_label().map(str::to_string), label, "undo stack unchanged");
    // The scene was synced mid-script by no read here, but a later script still sees a clean doc.
    let n = ok(&mut e, &mut s, "find({ type: 'frame' }).length").value;
    assert_eq!(n, json!(1));
}

#[test]
fn rollback_after_reads_resyncs_the_scene() {
    let (mut e, mut s) = (Engine::new(), session());
    let err = run(&mut e, &mut s, "const f = insert(null, { type: 'frame', x: 5000, width: 100, height: 100 });\nget(f).bounds;\nthrow new Error('stop');").unwrap_err();
    assert_eq!(err.line, Some(3));
    assert!(s.scene.page_bounds(s.doc()).is_none(), "scene no longer has the frame");
    assert!(s.scene.index.page_bounds(s.page()).is_none());
}

#[test]
fn caught_errors_keep_the_script_running() {
    let (mut e, mut s) = (Engine::new(), session());
    let o = ok(&mut e, &mut s, "let msg; try { update('nope', {}) } catch (err) { msg = err.message }\ninsert(null, {type: 'ellipse', width: 4, height: 4}); msg");
    assert!(o.value.as_str().unwrap().contains("not a node id"), "{}", o.value);
    assert_eq!(o.changes.inserted, 1);
}

#[test]
fn top_level_return_works() {
    let (mut e, mut s) = (Engine::new(), session());
    assert_eq!(ok(&mut e, &mut s, "const a = 2;\nreturn a * 21;").value, json!(42));
    let err = run(&mut e, &mut s, "const a = 2;\nnull.x;\nreturn a;").unwrap_err();
    assert_eq!(err.line, Some(2), "{err:?}");
}

#[test]
fn cpu_and_memory_limits() {
    let (mut e, mut s) = (Engine::new(), session());
    let t = std::time::Instant::now();
    let err = run(&mut e, &mut s, "insert(null, {type: 'frame'});\nwhile (true) {}").unwrap_err();
    assert!(err.message.contains("CPU limit"), "{}", err.message);
    assert!(t.elapsed() < Duration::from_secs(2));
    assert_eq!(s.doc().pages().len(), 1);
    assert_eq!(ok(&mut e, &mut s, "find({}).length").value, json!(0), "rolled back");
    let req = Execute { code: Some("const a = []; while (true) a.push(new Array(1e5).fill(1.5));".into()), timeout_ms: Some(10_000), ..Default::default() };
    let err = e.execute(&mut s, &req, &Options::default()).unwrap_err();
    assert!(err.message.contains("memory"), "{}", err.message);
    let req = Execute { code: Some("let i = 0; while (i < 2e6) i++; i".into()), timeout_ms: Some(5000), ..Default::default() };
    assert_eq!(e.execute(&mut s, &req, &Options::default()).unwrap().value, json!(2000000));
}

#[test]
fn seeded_ids_are_reproducible() {
    let ids = |seed| {
        let (mut e, mut s) = (Engine::new(), session());
        let req = Execute { code: Some("[insert(null, {type: 'frame'}), insert(null, {type: 'text', text: 'a'})]".into()), seed: Some(seed), ..Default::default() };
        e.execute(&mut s, &req, &Options::default()).unwrap().value
    };
    assert_eq!(ids(7), ids(7));
    assert_ne!(ids(7), ids(8));
}

#[test]
fn retry_with_line_edits() {
    let (mut e, mut s) = (Engine::new(), session());
    let err = run(&mut e, &mut s, "const f = insert(null, {type: 'frame', width: 10});\nupdate(f, {opacityy: 0.5});\nf").unwrap_err();
    let id = err.script_id.clone().unwrap();
    let req = Execute { retry: Some(id), edits: vec![LineEdit { from: 2, to: None, text: "update(f, {opacity: 0.5});".into() }], ..Default::default() };
    let o = e.execute(&mut s, &req, &Options::default()).unwrap();
    let f = o.value.as_str().unwrap().to_string();
    let v = ok(&mut e, &mut s, &format!("get('{f}').opacity")).value;
    assert_eq!(v, json!(0.5));
    let bad = Execute { retry: Some("s999".into()), ..Default::default() };
    assert!(e.execute(&mut s, &bad, &Options::default()).unwrap_err().message.contains("unknown scriptId"));
}

#[test]
fn queries() {
    let (mut e, mut s) = (Engine::new(), session());
    ok(&mut e, &mut s, CARD);
    ok(&mut e, &mut s, "insert(null, {type: 'text', name: 'Caption', text: 'Small print', visible: false})");
    let v = ok(&mut e, &mut s, r##"({
        texts: find({ type: "text" }).length,
        named: find({ name: "Ti*" }).length,
        byText: find({ text: "small" }).length,
        hidden: find({ visible: false }).length,
        within: find({ within: find({ name: "Card" })[0] }).length,
        fn: find((n) => n.type === "rectangle" && n.bounds.width > 100).length,
        all: find({ page: "all", type: ["text", "rectangle"] }).length,
        nested: get(find({ name: "Card" })[0], { depth: 1 }).children.map((c) => c.name),
        fields: Object.keys(get({ name: "Bar" }, { fields: ["fills"] })[0]).sort(),
    })"##).value;
    assert_eq!(v["texts"], 2);
    assert_eq!(v["named"], 1);
    assert_eq!(v["byText"], 1);
    assert_eq!(v["hidden"], 1);
    assert_eq!(v["within"], 2);
    assert_eq!(v["fn"], 1);
    assert_eq!(v["all"], 3);
    assert_eq!(v["nested"], json!(["Title", "Bar"]));
    assert_eq!(v["fields"], json!(["fills", "id", "type"]));
}

#[test]
fn updates_merge_text_style_and_auto_layout() {
    let (mut e, mut s) = (Engine::new(), session());
    let v = ok(&mut e, &mut s, r##"
        const f = insert(null, { type: "frame", autoLayout: { direction: "vertical", gap: 4 } });
        const t = insert(f, { type: "text", text: "A", style: { fontSize: 30, color: "#ff0000" } });
        update(f, { autoLayout: { gap: 10 } });
        update(t, { style: { fontWeight: 700 }, text: "B" });
        ({ f: get(f).autoLayout, t: get(t) })"##).value;
    assert_eq!(v["f"]["direction"], "vertical");
    assert_eq!(v["f"]["gap"].as_f64(), Some(10.0));
    assert_eq!(v["t"]["style"]["fontSize"].as_f64(), Some(30.0));
    assert_eq!(v["t"]["style"]["color"], "#ff0000");
    assert_eq!(v["t"]["style"]["fontWeight"], 700);
    assert_eq!(v["t"]["text"], "B");
}

#[test]
fn move_copy_replace_remove() {
    let (mut e, mut s) = (Engine::new(), session());
    let v = ok(&mut e, &mut s, r##"
        const a = insert(null, { type: "frame", name: "A", width: 100, height: 100 });
        const b = insert(null, { type: "frame", name: "B", x: 200, width: 100, height: 100 });
        const r = insert(a, { type: "rectangle", name: "R", width: 10, height: 10 });
        move(r, b);
        const c = copy(b, null, { name: "B copy", x: 400 });
        const e = replace(a, { type: "ellipse", name: "E", width: 50, height: 50 });
        doc.delete(find({ name: "R" })[0]);
        ({ names: find({}).map((id) => get(id).name), copyKids: get(c).childIds.length })"##).value;
    assert_eq!(v["names"], json!(["E", "B", "B copy", "R"]));
    assert_eq!(v["copyKids"], 1);
    let err = run(&mut e, &mut s, "remove(page)").unwrap_err();
    assert!(err.message.contains("last page"), "{}", err.message);
}

#[test]
fn empty_space_never_overlaps() {
    let (mut e, mut s) = (Engine::new(), session());
    let v = ok(&mut e, &mut s, r##"
        for (let i = 0; i < 6; i++) insert(null, { type: "frame", x: (i % 3) * 500, y: Math.floor(i / 3) * 400, width: 400, height: 300 });
        const spots = [findEmptySpace(300, 200), findEmptySpace(300, 200, { near: { x: 450, y: 350 } }), findEmptySpace(800, 800, { near: { x: 0, y: 0 }, margin: 20 })];
        spots"##).value;
    let blocks: Vec<(f64, f64)> = (0..6).map(|i| ((i % 3) as f64 * 500.0, (i / 3) as f64 * 400.0)).collect();
    let sizes = [(300.0, 200.0, 100.0), (300.0, 200.0, 100.0), (800.0, 800.0, 20.0)];
    for (p, (w, h, m)) in v.as_array().unwrap().iter().zip(sizes) {
        let (x, y) = (p["x"].as_f64().unwrap(), p["y"].as_f64().unwrap());
        for (bx, by) in &blocks {
            let apart = x + w + m <= *bx || *bx + 400.0 + m <= x || y + h + m <= *by || *by + 300.0 + m <= y;
            assert!(apart, "spot {x},{y} ({w}×{h}) overlaps block at {bx},{by}");
        }
    }
    assert_eq!((v[0]["x"].as_f64(), v[0]["y"].as_f64()), (Some(1500.0), Some(0.0)), "default: right of the content");
}

#[test]
fn screenshots_and_exports() {
    let (mut e, mut s) = (Engine::new(), session());
    let o = ok(&mut e, &mut s, &format!("{CARD}\nscreenshot(card, {{ scale: 2 }})"));
    assert_eq!(o.images.len(), 1);
    let img = &o.images[0];
    assert_eq!(img.width, 640);
    assert!(img.png.starts_with(b"\x89PNG"));
    let dir = std::env::temp_dir().join(format!("script-export-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let opts = Options { roots: vec![dir.clone()], ..Options::default() };
    let req = |code: &str| Execute { code: Some(code.into()), ..Default::default() };
    let o = e.execute(&mut s, &req("exportImage(null, 'png', 'page.png')"), &opts).unwrap();
    assert_eq!(o.exports.len(), 1);
    assert!(dir.join("page.png").exists());
    let err = e.execute(&mut s, &req("exportImage(null, 'png', '../escape.png')"), &opts).unwrap_err();
    assert!(err.message.contains("not allowed"), "{}", err.message);
    for (f, magic) in [("svg", &b"<svg"[..]), ("pdf", b"%PDF"), ("jpeg", &[0xff, 0xd8][..]), ("webp", b"RIFF")] {
        let o = e.execute(&mut s, &req(&format!("{CARD}
exportImage(card, '{f}', 'card.{f}', {{ scale: 2, quality: 80 }})")), &opts).unwrap();
        assert_eq!(o.value["width"], 640, "{f}");
        assert!(std::fs::read(dir.join(format!("card.{f}"))).unwrap().starts_with(magic), "{f}");
    }
    let err = e.execute(&mut s, &req("exportImage(null, 'tiff', 'a.tiff')"), &opts).unwrap_err();
    assert!(err.message.contains("unknown format"), "{}", err.message);
    let err = run(&mut e, &mut s, "exportImage(null, 'png', 'a.png')").unwrap_err();
    assert!(err.message.contains("disabled"), "{}", err.message);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn svg_inserts_as_shapes() {
    let (mut e, mut s) = (Engine::new(), session());
    let o = ok(&mut e, &mut s, r##"
const id = insert(null, { type: "svg", name: "Logo", x: 40, y: 50,
  svg: '<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"><rect width="10" height="10" fill="#f00"/><circle cx="15" cy="5" r="5" fill="#00f" filter="url(#f)"/><filter id="f"><feGaussianBlur stdDeviation="1"/></filter></svg>' });
const n = get(id, { depth: 1 });
[n.type, n.name, n.x, n.y, n.width, n.children.length]"##);
    assert_eq!(o.value, serde_json::json!(["frame", "Logo", 40, 50, 20, 2]));
    assert!(o.logs.iter().any(|l| l.contains("filters")), "{:?}", o.logs);
    let err = run(&mut e, &mut s, "insert(null, { type: 'svg', svg: 'not svg' })").unwrap_err();
    assert!(err.message.contains("not a valid SVG"), "{}", err.message);
}

#[test]
fn state_and_selection() {
    let (mut e, mut s) = (Engine::new(), session());
    let v = ok(&mut e, &mut s, "const f = insert(null, {type: 'frame', width: 10, height: 10}); select(f); ({ page, sel: selection, z: viewport.zoom })").value;
    assert_eq!(v["page"], s.page().to_string());
    assert_eq!(v["sel"].as_array().unwrap().len(), 1);
    assert_eq!(s.selection().len(), 1);
    assert!(v["z"].as_f64().unwrap() > 0.0);
}

#[test]
fn values_and_unknown_input() {
    let (mut e, mut s) = (Engine::new(), session());
    assert_eq!(ok(&mut e, &mut s, "undefined").value, Value::Null);
    assert_eq!(ok(&mut e, &mut s, "get('0000000000000000000001')").value, Value::Null);
    let err = run(&mut e, &mut s, "insert(null, { type: 'blob' })").unwrap_err();
    assert!(err.message.contains("unknown node type"), "{}", err.message);
    let err = run(&mut e, &mut s, "find({ colour: 'red' })").unwrap_err();
    assert!(err.message.contains("colour"), "{}", err.message);
    let err = run(&mut e, &mut s, "syntax error here").unwrap_err();
    assert!(err.message.contains("SyntaxError"), "{}", err.message);
    assert_eq!(err.line, Some(1));
}

#[test]
fn variables_by_name() {
    let (mut e, mut s) = (Engine::new(), session());
    let o = ok(
        &mut e,
        &mut s,
        r##"
const v = setVariables([{ name: "Theme", modes: ["Light", "Dark"], variables: [
  { name: "bg", type: "color", values: { Light: "#ffffff", Dark: "#111827" } },
  { name: "surface", type: "color", values: { Light: { alias: "bg" } } },
  { name: "space", type: "number", values: { Light: 8 } } ] }]);
const r = insert(null, { type: "rectangle", width: 10, height: 10, fills: ["#000000"], bindings: { "fills/0/color": "Theme/bg" } });
update(r, { bindings: { opacity: "space" } });
({ v, r: get(r), all: getVariables() })
"##,
    );
    let v = &o.value["v"][0];
    assert_eq!(v["defaultMode"], "Light");
    assert_eq!(v["variables"][1]["values"]["Light"]["alias"], "Theme/bg");
    let bg = v["variables"][0]["id"].as_str().unwrap();
    assert_eq!(o.value["r"]["bindings"]["fills/0/color"], bg);
    // The bound fill given alongside takes the token's value.
    assert_eq!(o.value["r"]["fills"][0]["color"], "#ffffff");
    assert_eq!(o.value["all"], o.value["v"]);
    // Setting the collection again keeps ids by name, so bindings stay valid.
    let o = ok(&mut e, &mut s, r##"setVariables({ name: "Theme", modes: ["Light"], variables: [{ name: "bg", type: "color", values: { Light: "#fafafa" } }, { name: "space", type: "number", values: { Light: 4 } }] })"##);
    assert_eq!(o.value[0]["variables"][0]["id"], bg);
    // Dropping a variable that a node is bound to is refused.
    let err = run(&mut e, &mut s, r##"setVariables({ name: "Theme", modes: ["Light"], variables: [] })"##).unwrap_err();
    assert!(err.message.contains("unknown variable"), "{}", err.message);
    for (code, msg) in [
        (r#"insert(null, { type: "rectangle", bindings: { opacity: "nope" } })"#, "no variable \"nope\""),
        (r#"setVariables([{ name: "X", modes: ["A"], variables: [{ name: "v", type: "number", values: { B: 1 } }] }])"#, "no mode \"B\""),
        (r#"setVariables([{ name: "X", modes: [] }])"#, "at least one mode"),
    ] {
        let err = run(&mut e, &mut s, code).unwrap_err();
        assert!(err.message.contains(msg), "{code}: {}", err.message);
    }
}

#[test]
fn stroke_shorthand() {
    let (mut e, mut s) = (Engine::new(), session());
    let o = ok(&mut e, &mut s, r##"const r = insert(null, { type: "rectangle", strokes: ["#e5e7eb", { paint: "#111111", weight: 2, align: "inside" }] }); get(r).strokes"##);
    assert_eq!(o.value[0]["paint"]["color"], "#e5e7eb");
    assert_eq!(o.value[1]["paint"]["type"], "solid");
    assert_eq!(o.value[1]["weight"], 2.0);
}

#[test]
fn update_merges_bindings() {
    let (mut e, mut s) = (Engine::new(), session());
    let o = ok(
        &mut e,
        &mut s,
        r##"
setVariables([{ name: "T", modes: ["A"], variables: [{ name: "c", type: "color", values: { A: "#ff0000" } }, { name: "o", type: "number", values: { A: 0.5 } }] }]);
const r = insert(null, { type: "rectangle", fills: ["#000000"], bindings: { "fills/0/color": "c" } });
update(r, { bindings: { opacity: "o" } });
const both = Object.keys(get(r).bindings).length;
update(r, { bindings: { opacity: null } });
({ both, after: Object.keys(get(r).bindings) })
"##,
    );
    assert_eq!(o.value["both"], 2);
    assert_eq!(o.value["after"], json!(["fills/0/color"]));
}

#[test]
fn copy_overrides_take_shorthands() {
    let (mut e, mut s) = (Engine::new(), session());
    let o = ok(&mut e, &mut s, r##"const t = insert(null, { type: "text", text: "A", style: { fontSize: 20 } }); const c = copy(t, null, { x: 50, text: "B", style: { color: "#ff0000" }, fills: ["#00ff00"] }); get(c)"##);
    assert_eq!(o.value["text"], "B");
    assert_eq!(o.value["style"]["fontSize"], 20.0);
    assert_eq!(o.value["style"]["color"], "#ff0000");
    assert_eq!(o.value["fills"][0]["type"], "solid");
}

#[test]
fn components_by_name_with_overrides_and_modes() {
    let (mut e, mut s) = (Engine::new(), session());
    let o = ok(
        &mut e,
        &mut s,
        r##"
setVariables([{ name: "Theme", modes: ["Light", "Dark"], variables: [
  { name: "bg", type: "color", values: { Light: "#ffffff", Dark: "#111111" } }] }]);
const btn = insert(null, { type: "component", name: "Button", width: 120, height: 40, fills: ["#2563eb"],
  autoLayout: { padding: [10, 16] }, children: [
    { type: "text", name: "Label", text: "OK", style: { color: "#fff" } },
    { type: "rectangle", name: "Icon", width: 8, height: 8 }] });
const a = insert(null, { type: "instance", component: "Button", y: 100,
  overrides: { "Label": { text: "Buy" }, "Icon/visible": false, "fills": ["#16a34a"] } });
const label = get(a, { depth: 3 }).children[0].children[0];
const screen = insert(null, { type: "frame", name: "Screen", x: 300, modes: { Theme: "Dark" }, fills: ["#ffffff"],
  bindings: { "fills/0/color": "Theme/bg" } });
update(label.id, { text: "Buy now" });
const frame = insert(null, { type: "frame", name: "Card", x: 600, width: 50, height: 50 });
const comp = createComponent(frame, "Card component");
const inst = insert(null, { type: "instance", component: comp, x: 700 });
const detached = detach(inst);
({ a, label, labelText: get(label.id).text, inst: get(a), screen: get(screen), comp: get(comp, { fields: ["name"] }), detached: get(detached, { fields: ["type"] }) })
"##,
    );
    let v = &o.value;
    assert_eq!(v["label"]["name"], "Label");
    assert_eq!(v["label"]["instanceId"], v["a"]);
    assert_eq!(v["label"]["instancePath"].as_array().unwrap().len(), 1);
    assert_eq!(v["labelText"], "Buy now");
    let ovs = v["inst"]["overrides"].as_array().unwrap();
    assert_eq!(ovs.len(), 3, "{ovs:?}");
    assert!(ovs.iter().any(|o| o["target"].as_array().unwrap().is_empty()), "fills on the root: {ovs:?}");
    assert_eq!(v["screen"]["fills"][0]["color"], "#111111", "dark mode applied");
    assert_eq!(v["comp"]["name"], "Card component");
    assert_eq!(v["detached"]["type"], "frame");
    s.doc().check_invariants().unwrap();
    let err = run(&mut e, &mut s, r#"insert(null, { type: "instance", component: "Nope" })"#).unwrap_err().to_string();
    assert!(err.contains("Button"), "{err}");
    let err = run(&mut e, &mut s, r#"insert(null, { type: "instance", component: "Button", overrides: { "Missing/text": "x" } })"#).unwrap_err().to_string();
    assert!(err.contains("Label"), "{err}");
}

#[test]
fn overrides_beat_component_bindings() {
    let (mut e, mut s) = (Engine::new(), session());
    let o = ok(
        &mut e,
        &mut s,
        r##"
setVariables([{ name: "Theme", modes: ["Light", "Dark"], variables: [
  { name: "a", type: "color", values: { Light: "#2563eb", Dark: "#93c5fd" } },
  { name: "danger", type: "color", values: { Light: "#dc2626", Dark: "#f87171" } }] }]);
insert(null, { type: "component", name: "B", fills: ["#000"], bindings: { "fills/0/color": "a" }, autoLayout: { padding: 10 },
  children: [{ type: "text", name: "L", text: "x", style: { color: "#000" }, bindings: { "style/color": "a" } }] });
const plain = insert(null, { type: "instance", component: "B", x: 100, overrides: { "": { fills: ["#16a34a"] }, L: { style: { color: "#16a34a" } } } });
const themed = insert(null, { type: "instance", component: "B", x: 200, modes: { Theme: "Dark" }, overrides: { "": { bindings: { "fills/0/color": "danger" } } } });
const root = (id) => get(id, { depth: 2 }).children[0];
({ plain: root(plain), themed: root(themed), themedOwn: get(themed, { fields: ["bindings"] }) })
"##,
    );
    let v = &o.value;
    assert_eq!(v["plain"]["fills"][0]["color"], "#16a34a", "fill override shows");
    assert_eq!(v["plain"]["children"][0]["style"]["color"], "#16a34a", "text colour override shows");
    assert_eq!(v["themed"]["fills"][0]["color"], "#f87171", "rebinding the look reaches the root, in the instance's mode");
    assert!(v["themedOwn"].get("bindings").is_none(), "{}", v["themedOwn"]);
}
