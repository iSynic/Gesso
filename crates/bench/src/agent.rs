//! Stage 6 agent scenarios: the script engine and agent requests against the 100k-node
//! document on one page, and the IPC round trip.

use std::sync::Arc;
use std::time::Instant;

use agent::{Request, Response};
use doc::testing::GenConfig;
use editor::Session;
use script::{Engine, Execute, Options};
use serde_json::json;
use text::FontDb;

use crate::editor::one_page;
use crate::harness::{Report, Scenario, ms, repeat, stats};

pub fn all() -> Vec<Box<dyn Scenario>> {
    vec![Box::new(Agent)]
}

struct Agent;

/// Runs `code`, failing the benchmark on a script error.
fn exec(e: &mut Engine, s: &mut Session, code: &str) -> script::Outcome {
    let opts = Options { timeout: std::time::Duration::from_secs(10), ..Options::default() };
    match e.execute(s, &Execute { code: Some(code.into()), ..Default::default() }, &opts) {
        Ok(o) => o,
        Err(err) => panic!("script failed: {err:?}"),
    }
}

const INSERT_500: &str = r##"insert(null, { type: "frame", name: "Bench", x: -5000, y: 0, width: 400, fills: ["#ffffff"],
  autoLayout: { direction: "vertical", gap: 4, padding: 8 },
  children: Array.from({ length: 499 }, (_, i) => i % 2
    ? { type: "rectangle", name: "Bar " + i, sizingH: "fill", height: 8, fills: ["#3b82f6"] }
    : { type: "text", name: "Row " + i, text: "Row " + i, sizingH: "fill" }) })"##;

impl Scenario for Agent {
    fn name(&self) -> &'static str {
        "agent"
    }
    fn describe(&self) -> &'static str {
        "Agent requests at 100k nodes on one page: script start-up, a 500-node insert, get_state, find, findEmptySpace, screenshot, IPC round trip"
    }
    fn run(&self, r: &mut Report) {
        let mut s = Session::new(one_page(&GenConfig { target_nodes: 100_000, ..Default::default() }), None, FontDb::new(true)).expect("session");
        let page = s.doc().pages().iter().filter_map(|&k| s.doc().node(k)).max_by_key(|p| p.children.len()).map(|p| p.id).expect("page");
        s.set_page(page);
        r.info("page_nodes", s.doc().key(page).map_or(0, |k| s.doc().descendants(k).len()));
        let mut e = Engine::new();

        // A fresh context with the prelude, running nothing.
        r.metric("script_context", repeat(200, |_| drop(exec(&mut e, &mut s, "null"))), None, Some(1.0));

        // 500 nodes in one transaction, laid out and synced into the scene; undone untimed.
        let mut times = Vec::new();
        for _ in 0..30 {
            let t = Instant::now();
            exec(&mut e, &mut s, INSERT_500);
            times.push(ms(t));
            s.undo().expect("undo");
        }
        r.metric("execute_insert_500", stats(times), None, Some(10.0));
        r.metric("get_state", repeat(200, |_| drop(agent::state(&s, "headless"))), None, Some(1.0));
        let mut n = 0;
        r.metric("find_query_100k", repeat(50, |_| n = exec(&mut e, &mut s, r#"find({ name: "Card *", limit: 1000000 }).length"#).value.as_u64().unwrap_or(0)), None, Some(5.0));
        r.info("find_query_matches", n);
        r.metric("find_query_selective", repeat(50, |_| n = exec(&mut e, &mut s, r#"find({ name: "Screen 1*", type: "frame" }).length"#).value.as_u64().unwrap_or(0)), None, None);
        r.info("find_query_selective_matches", n);
        r.metric("find_empty_space", repeat(50, |_| drop(exec(&mut e, &mut s, "findEmptySpace(1440, 900, { near: { x: 0, y: 0 } })"))), None, Some(5.0));

        // A 1080p screenshot of a screen frame (about 100 nodes or more).
        let doc = s.doc();
        let screen = doc.iter().find(|(_, n)| n.name == "Screen 1").map(|(k, n)| (n.id, doc.descendants(k).len(), n.props.width)).expect("screen");
        r.info("screenshot_nodes", screen.1);
        let code = format!("screenshot({}, {{ scale: {} }})", json!(screen.0.to_string()), 1920.0 / screen.2);
        r.metric("screenshot_1080p_cpu", repeat(10, |_| drop(exec(&mut e, &mut s, &code))), None, None);

        // A no-op request over the local socket (framing, JSON, thread hand-off).
        let name = format!("ai-design-engine-bench-{}.sock", std::process::id());
        let handler: agent::ipc::Handler = Arc::new(|_req: Request| Response::ok(json!(null)));
        if agent::ipc::serve(Some(&name), json!({}), handler).is_ok() {
            match agent::ipc::Client::connect(Some(&name), "bench") {
                Ok(mut c) => {
                    let req = Request::GetState(agent::Empty {});
                    r.metric("ipc_round_trip", repeat(500, |_| drop(c.call(&req))), None, Some(2.0));
                }
                Err(err) => eprintln!("  ipc: cannot connect: {err}"),
            }
        }
    }
}
