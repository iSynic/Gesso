//! Scripting spike: rquickjs (QuickJS) running an agent-style `execute` program that builds
//! 500 nodes against a stub document API, plus sandbox checks (CPU time limit via interrupt
//! handler, memory limit, error line numbers).

use common::{percentiles, write_json, Args, Host, Timer};
use rquickjs::{CatchResultExt, CaughtError, Context, Function, Runtime};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Stub document: id -> (parent, node JSON). Stands in for the real transaction API.
#[derive(Default)]
struct Doc {
    nodes: HashMap<u64, (u64, Value)>,
    children: HashMap<u64, Vec<u64>>,
    next: u64,
    ops: usize,
}

/// JS prelude: the agent-facing API, marshalling through JSON strings to the native functions.
const PRELUDE: &str = r#"
const insert = (parent, node) => __insert(parent, JSON.stringify(node));
const update = (id, props) => __update(id, JSON.stringify(props));
const get = (id) => JSON.parse(__get(id));
const find = (type) => JSON.parse(__find(type));
const print = (...a) => __print(a.map(String).join(" "));
"#;

/// The 500-node program an agent would send: 50 cards × (frame + 9 children).
const PROGRAM: &str = r##"
const page = 0;
const root = insert(page, { type: "frame", name: "Grid", layout: { direction: "row", wrap: true, gap: 16, padding: 24 }, width: 1440 });
for (let i = 0; i < 55; i++) {
  const card = insert(root, { type: "frame", name: `Card ${i}`, layout: { direction: "column", gap: 8, padding: 16 },
    width: 220, fills: [{ type: "solid", color: "#ffffff" }], cornerRadius: 12,
    effects: [{ type: "dropShadow", color: "#0f172a28", offset: [0, 4], radius: 8 }] });
  insert(card, { type: "ellipse", width: 48, height: 48, fills: [{ type: "solid", color: i % 2 ? "#2563eb" : "#10b981" }] });
  insert(card, { type: "text", characters: `Project ${i}`, fontSize: 18, fontWeight: 600 });
  insert(card, { type: "text", characters: "Updated 3 minutes ago", fontSize: 13, fills: [{ type: "solid", color: "#64748b" }] });
  const row = insert(card, { type: "frame", layout: { direction: "row", gap: 8 } });
  insert(row, { type: "rectangle", width: 60, height: 6, cornerRadius: 3 });
  const btn = insert(row, { type: "frame", layout: { padding: [6, 12] }, cornerRadius: 6, fills: [{ type: "solid", color: "#2563eb" }] });
  insert(btn, { type: "text", characters: "Open", fontSize: 13 });
  const tag = insert(card, { type: "text", characters: "draft" });
  update(tag, { characters: i % 3 == 0 ? "published" : "draft", opacity: 0.8 });
}
const texts = find("text");
print("nodes created; texts:", texts.length);
texts.length
"##;

fn install(ctx: &rquickjs::Ctx<'_>, doc: &Rc<RefCell<Doc>>) {
    let g = ctx.globals();
    let d = doc.clone();
    g.set(
        "__insert",
        Function::new(ctx.clone(), move |parent: f64, node: String| -> rquickjs::Result<f64> {
            let mut d = d.borrow_mut();
            let v: Value = serde_json::from_str(&node).map_err(|_| rquickjs::Error::new_from_js("string", "node"))?;
            d.next += 1;
            let id = d.next;
            d.nodes.insert(id, (parent as u64, v));
            d.children.entry(parent as u64).or_default().push(id);
            d.ops += 1;
            Ok(id as f64)
        }),
    )
    .unwrap();
    let d = doc.clone();
    g.set(
        "__update",
        Function::new(ctx.clone(), move |id: f64, props: String| {
            let mut d = d.borrow_mut();
            let p: Value = serde_json::from_str(&props).unwrap_or(Value::Null);
            if let (Some((_, Value::Object(n))), Value::Object(p)) = (d.nodes.get_mut(&(id as u64)), p) {
                n.extend(p);
            }
            d.ops += 1;
        }),
    )
    .unwrap();
    let d = doc.clone();
    g.set("__get", Function::new(ctx.clone(), move |id: f64| d.borrow().nodes.get(&(id as u64)).map_or("null".to_string(), |n| n.1.to_string()))).unwrap();
    let d = doc.clone();
    g.set(
        "__find",
        Function::new(ctx.clone(), move |ty: String| {
            let d = d.borrow();
            let ids: Vec<u64> = d.nodes.iter().filter(|(_, n)| n.1.get("type").and_then(Value::as_str) == Some(ty.as_str())).map(|(k, _)| *k).collect();
            serde_json::to_string(&ids).unwrap()
        }),
    )
    .unwrap();
    g.set("__print", Function::new(ctx.clone(), |s: String| eprintln!("[script] {s}"))).unwrap();
    g.set("__noop", Function::new(ctx.clone(), |x: f64| x)).unwrap();
    ctx.eval::<(), _>(PRELUDE).unwrap();
}

fn err_text(e: CaughtError<'_>) -> String {
    match e {
        CaughtError::Exception(ex) => format!("{} | stack: {}", ex.message().unwrap_or_default(), ex.stack().unwrap_or_default().replace('\n', " / ")),
        other => other.to_string(),
    }
}

fn main() {
    let args = Args::from_env();
    // Runtime + context creation (cold and warm).
    let t = Timer::start();
    let rt = Runtime::new().unwrap();
    let rt_ms = t.ms();
    let t = Timer::start();
    let ctx = Context::full(&rt).unwrap();
    let ctx_ms = t.ms();
    let mut ctx_warm = Vec::new();
    for _ in 0..20 {
        let t = Timer::start();
        let _c = Context::full(&rt).unwrap();
        ctx_warm.push(t.ms());
    }

    // The 500-node program: fresh context + doc each run, as one `execute` call would be.
    let mut runs = Vec::new();
    let mut nodes = 0;
    for _ in 0..20 {
        let doc = Rc::new(RefCell::new(Doc::default()));
        let c = Context::full(&rt).unwrap();
        let t = Timer::start();
        let n: i32 = c.with(|ctx| {
            install(&ctx, &doc);
            ctx.eval::<i32, _>(PROGRAM).catch(&ctx).map_err(err_text).unwrap()
        });
        runs.push(t.ms());
        nodes = doc.borrow().nodes.len();
        let _ = n;
    }
    // Per-call FFI cost.
    let ffi_us = ctx.with(|ctx| {
        install(&ctx, &Rc::new(RefCell::new(Doc::default())));
        let t = Timer::start();
        ctx.eval::<f64, _>("let s = 0; for (let i = 0; i < 100000; i++) s += __noop(i); s").unwrap();
        t.ms() * 1000.0 / 100000.0
    });
    let insert_us = ctx.with(|ctx| {
        let t = Timer::start();
        ctx.eval::<(), _>("for (let i = 0; i < 10000; i++) insert(0, { type: 'rectangle', width: 10, height: 10, fills: [{ type: 'solid', color: '#ff0000' }] });").unwrap();
        t.ms() * 1000.0 / 10000.0
    });

    // Sandbox: CPU limit via interrupt handler (100 ms budget) on an infinite loop.
    let rt2 = Runtime::new().unwrap();
    let deadline = Rc::new(RefCell::new(Instant::now() + Duration::from_secs(3600)));
    let dl = deadline.clone();
    rt2.set_interrupt_handler(Some(Box::new(move || Instant::now() > *dl.borrow())));
    rt2.set_memory_limit(64 * 1024 * 1024);
    let c2 = Context::full(&rt2).unwrap();
    *deadline.borrow_mut() = Instant::now() + Duration::from_millis(100);
    let t = Timer::start();
    let cpu = c2.with(|ctx| ctx.eval::<(), _>("while (true) {}").catch(&ctx).map_err(err_text).err());
    let cpu_ms = t.ms();
    *deadline.borrow_mut() = Instant::now() + Duration::from_secs(3600);
    let t = Timer::start();
    let mem = c2.with(|ctx| ctx.eval::<(), _>("const a = []; while (true) a.push(new Array(1e5).fill(1.5));").catch(&ctx).map_err(err_text).err());
    let mem_ms = t.ms();
    let line = c2.with(|ctx| ctx.eval::<(), _>("const x = 1;\nconst y = 2;\nnull.prop; // line 3").catch(&ctx).map_err(err_text).err());

    eprintln!("runtime new {rt_ms:.2} ms, first context {ctx_ms:.2} ms, warm context p50 {:.3} ms", percentiles(ctx_warm.clone()).p50);
    let rp = percentiles(runs.clone());
    eprintln!("500-node program ({nodes} nodes): p50 {:.2} ms p95 {:.2} ms", rp.p50, rp.p95);
    eprintln!("ffi noop {ffi_us:.3} µs/call, insert via JSON {insert_us:.2} µs/call");
    eprintln!("cpu limit: stopped after {cpu_ms:.0} ms: {cpu:?}");
    eprintln!("memory limit (64 MB): {mem_ms:.0} ms: {mem:?}");
    eprintln!("error line: {line:?}");
    write_json(
        args.value("--out"),
        &json!({
            "rquickjs": "0.14.0", "host": Host::detect(String::new(), "cpu".into()),
            "runtime_new_ms": rt_ms, "first_context_ms": ctx_ms, "warm_context": percentiles(ctx_warm),
            "program_500_nodes": rp, "nodes": nodes, "ffi_noop_us": ffi_us, "insert_json_us": insert_us,
            "cpu_limit": { "budget_ms": 100, "stopped_after_ms": cpu_ms, "error": cpu },
            "memory_limit": { "limit_mb": 64, "ms": mem_ms, "error": mem },
            "error_with_line": line,
        }),
    );
}
