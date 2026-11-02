//! `canvas-mcp --headless` driven over stdio with raw JSON-RPC, as an MCP client would.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next: u64,
}

impl Mcp {
    fn start(dir: &std::path::Path) -> Mcp {
        let mut child = Command::new(env!("CARGO_BIN_EXE_canvas-mcp"))
            .args(["--headless", "--deterministic", "--root"])
            .arg(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Mcp { child, stdin, stdout, next: 1 }
    }

    fn send(&mut self, v: &Value) {
        writeln!(self.stdin, "{v}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let mut line = String::new();
            assert!(self.stdout.read_line(&mut line).unwrap() > 0, "server closed stdout");
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == id {
                return v;
            }
        }
    }

    fn tool(&mut self, name: &str, args: Value) -> Value {
        self.request("tools/call", json!({"name": name, "arguments": args}))["result"].clone()
    }
}

fn text(r: &Value) -> Value {
    serde_json::from_str(r["content"][0]["text"].as_str().unwrap()).unwrap()
}

#[test]
fn headless_session() {
    let dir = std::env::temp_dir().join(format!("canvas-mcp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut m = Mcp::start(&dir);

    let init = m.request("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}));
    assert_eq!(init["result"]["serverInfo"]["name"], "canvas-mcp");
    assert!(init["result"]["instructions"].as_str().unwrap().contains("read_guide"));
    m.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    let tools = m.request("tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    for n in ["execute", "get_state", "screenshot", "read_guide", "validate", "open", "save"] {
        assert!(names.contains(&n), "{n} missing from {names:?}");
    }
    assert_eq!(tools["result"]["tools"][0]["inputSchema"]["type"], "object");

    let st = text(&m.tool("get_state", json!({})));
    assert_eq!(st["mode"], "headless");

    let r = m.tool("execute", json!({"code": "// label: Box\nconst b = insert(null, {type: 'rectangle', width: 100, height: 50, fills: ['#336699']}); screenshot([b]); b"}));
    assert_ne!(r["isError"], true, "{r}");
    assert_eq!(text(&r)["undoLabel"], "Box");
    assert_eq!(r["content"][1]["type"], "image");
    assert_eq!(r["content"][1]["mimeType"], "image/png");

    let r = m.tool("screenshot", json!({"scale": 2}));
    assert_eq!(text(&r)["width"], 200);
    assert_eq!(r["content"][1]["type"], "image");

    // A script error comes back as a tool error with the line.
    let r = m.tool("execute", json!({"code": "1;\nupdate('nope', {})"}));
    assert_eq!(r["isError"], true);
    assert_eq!(text(&r)["error"]["line"], 2);

    // Bad arguments are a readable tool error; an unknown tool is a protocol error.
    let r = m.tool("execute", json!({"cod": "1"}));
    assert_eq!(r["isError"], true);
    assert!(r["content"][0]["text"].as_str().unwrap().contains("cod"));
    assert!(m.request("tools/call", json!({"name": "nope", "arguments": {}}))["error"].is_object());

    let r = m.tool("save", json!({"path": "box.canvas"}));
    assert_ne!(r["isError"], true, "{r}");
    assert_eq!(text(&m.tool("validate", json!({"path": "box.canvas"})))["valid"], true);
    assert!(text(&m.tool("read_guide", json!({"name": "start"})))["text"].is_string());

    drop(m.stdin);
    let _ = m.child.wait();
    let _ = std::fs::remove_dir_all(dir);
}
