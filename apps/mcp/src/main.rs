//! `canvas-mcp`: the MCP server, over stdio.
//!
//! ```text
//! canvas-mcp [--root DIR]... [--doc PATH] [--headless] [--deterministic] [--socket NAME]
//! ```
//!
//! Each tool call becomes an `agent::Request`. Unless `--headless`, it goes to a running desktop
//! app over local IPC (so edits appear live and undo there); with no app running it is answered
//! by an in-process headless `agent::Workspace`, opened from `--doc` if given. File paths
//! (open, save, export, validate) must stay inside the `--root` folders (default: the current
//! folder). Logs go to stderr; stdout carries only the protocol.

use std::path::PathBuf;
use std::sync::mpsc;

use agent::ipc::Client;
use agent::{Host, Request, Response, Workspace};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation, ListToolsResult, PaginatedRequestParams, ServerCapabilities,
    ServerConfig, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData, ServerHandler, ServiceExt};
use serde_json::Value;
use tokio::sync::oneshot;

const INSTRUCTIONS: &str = "Design screens on an infinite canvas. Before your first edit, call read_guide with {\"name\": \"start\"}, then get_state. \
Make changes with execute (a JavaScript program; each call is one undoable step), and check them with screenshot.";

const USAGE: &str = "usage: canvas-mcp [--root DIR]... [--doc PATH] [--headless] [--deterministic] [--socket NAME]";

struct Config {
    roots: Vec<PathBuf>,
    doc: Option<String>,
    headless: bool,
    deterministic: bool,
    socket: Option<String>,
}

impl Config {
    fn parse(args: Vec<String>) -> Result<Config, String> {
        let mut c = Config { roots: Vec::new(), doc: None, headless: false, deterministic: false, socket: None };
        let mut it = args.into_iter();
        while let Some(a) = it.next() {
            let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
            match a.as_str() {
                "--root" => c.roots.push(PathBuf::from(value("--root")?)),
                "--doc" => c.doc = Some(value("--doc")?),
                "--socket" => c.socket = Some(value("--socket")?),
                "--headless" => c.headless = true,
                "--deterministic" => c.deterministic = true,
                other => return Err(format!("unknown argument {other}")),
            }
        }
        if c.roots.is_empty() {
            c.roots.extend(std::env::current_dir());
        }
        for r in &c.roots {
            if !r.is_dir() {
                return Err(format!("--root {}: not a folder", r.display()));
            }
        }
        Ok(c)
    }
}

/// Answers requests on its own thread: the app connection or the headless workspace aren't
/// shared across threads.
struct Router {
    cfg: Config,
    app: Option<Client>,
    headless: Option<Workspace>,
}

impl Router {
    fn handle(&mut self, req: Request) -> Response {
        if !self.cfg.headless {
            if self.app.is_none() {
                self.app = Client::connect(self.cfg.socket.as_deref(), "canvas-mcp").ok();
                if let Some(c) = &self.app {
                    eprintln!("canvas-mcp: connected to the app ({})", c.server);
                }
            }
            if let Some(c) = &mut self.app {
                match c.call(&req) {
                    Ok(r) => return r,
                    Err(e) => {
                        // The request may or may not have been applied; don't replay it headless.
                        self.app = None;
                        return Response::err(format!("lost the connection to the app ({e}); call get_state to see where things stand"));
                    }
                }
            }
        }
        if self.headless.is_none() {
            let fonts = if self.cfg.deterministic { text::FontDb::bundled_only() } else { text::FontDb::new(true) };
            let roots = self.cfg.roots.clone();
            let w = match &self.cfg.doc {
                Some(d) => Workspace::open(fonts, roots, d),
                None => Workspace::new(fonts, roots),
            };
            match w {
                Ok(w) => self.headless = Some(w),
                Err(e) => return Response::err(format!("cannot start the headless workspace: {e}")),
            }
        }
        match &mut self.headless {
            Some(w) => w.handle(req),
            None => Response::err("no workspace"),
        }
    }
}

type Job = (Request, oneshot::Sender<Response>);

struct Server {
    jobs: mpsc::Sender<Job>,
    tools: Vec<Tool>,
}

impl Server {
    fn new(cfg: Config) -> Result<Server, String> {
        let (jobs, rx) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("canvas-mcp-router".into())
            .spawn(move || {
                let mut router = Router { cfg, app: None, headless: None };
                for (req, reply) in rx {
                    let _ = reply.send(router.handle(req));
                }
            })
            .map_err(|e| e.to_string())?;
        let tools = agent::tools()
            .into_iter()
            .map(|(name, description, schema)| {
                let schema = match schema {
                    Value::Object(o) => o,
                    _ => serde_json::Map::new(),
                };
                Tool::new(name, description, schema)
            })
            .collect();
        Ok(Server { jobs, tools })
    }

    async fn call(&self, req: Request) -> Response {
        let (tx, rx) = oneshot::channel();
        if self.jobs.send((req, tx)).is_err() {
            return Response::err("the request router stopped");
        }
        rx.await.unwrap_or_else(|_| Response::err("the request router stopped"))
    }
}

/// A response as MCP content: the result (or error) as compact JSON text, then any images.
fn to_result(r: Response) -> CallToolResult {
    let ok = r.is_ok();
    let body = if ok { r.result.unwrap_or(Value::Null) } else { serde_json::json!({ "error": r.error }) };
    let mut content = vec![ContentBlock::text(serde_json::to_string(&body).unwrap_or_default())];
    content.extend(r.images.into_iter().map(|i| ContentBlock::image(i.png, "image/png")));
    if ok { CallToolResult::success(content) } else { CallToolResult::error(content) }
}

impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("canvas-mcp", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(&self, _: Option<PaginatedRequestParams>, _: RequestContext<RoleServer>) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(self.tools.clone()))
    }

    async fn call_tool(&self, p: CallToolRequestParams, _: RequestContext<RoleServer>) -> Result<CallToolResponse, ErrorData> {
        let args = p.arguments.map_or(Value::Null, Value::Object);
        let result = match agent::request(&p.name, args) {
            Ok(req) => to_result(self.call(req).await),
            Err(_) if !self.tools.iter().any(|t| t.name == p.name) => return Err(ErrorData::invalid_params(format!("unknown tool {}", p.name), None)),
            // Bad arguments go back to the agent as a tool error it can read and fix.
            Err(e) => CallToolResult::error(vec![ContentBlock::text(e)]),
        };
        Ok(result.into())
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    let cfg = match Config::parse(std::env::args().skip(1).collect()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("canvas-mcp: {e}\n{USAGE}");
            return std::process::ExitCode::from(2);
        }
    };
    let server = match Server::new(cfg) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("canvas-mcp: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let running = match server.serve(rmcp::transport::stdio()).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("canvas-mcp: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let _ = running.waiting().await;
    std::process::ExitCode::SUCCESS
}
