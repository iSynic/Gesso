//! The headless host: one open document in a [`Session`], answering requests without a window.
//! The CLI and the MCP server (when no app is running) use it; the desktop app answers the same
//! requests through [`handle_with`] on its own session.

use std::path::PathBuf;

use editor::Session;
use script::{Engine, Options};
use serde_json::{Value, json};
use text::FontDb;

use crate::{Host, ImageOut, Request, Response, b64, guides, validate};

pub struct Workspace {
    pub session: Session,
    pub engine: Engine,
    pub opts: Options,
    /// Shown in `get_state` (`"headless"` here; the app says `"app"`).
    pub mode: &'static str,
}

impl Workspace {
    /// An untitled document. `roots` are the folders file paths must stay in.
    pub fn new(fonts: FontDb, roots: Vec<PathBuf>) -> Result<Self, String> {
        let session = Session::untitled(fonts).map_err(|e| e.to_string())?;
        Ok(Workspace { session, engine: Engine::new(), opts: Options { roots, ..Options::default() }, mode: "headless" })
    }

    /// Opens `path` (a `.canvas` folder), which must be inside the roots.
    pub fn open(fonts: FontDb, roots: Vec<PathBuf>, path: &str) -> Result<Self, String> {
        let mut w = Self::new(fonts, roots)?;
        let r = w.handle(Request::Open(crate::PathArg { path: path.into() }));
        match r.message() {
            Some(m) => Err(m.to_string()),
            None => Ok(w),
        }
    }
}

impl Host for Workspace {
    fn handle(&mut self, req: Request) -> Response {
        handle_with(&mut self.session, &mut self.engine, &self.opts, self.mode, req)
    }
}

fn images(imgs: &[script::Image]) -> Vec<ImageOut> {
    imgs.iter().map(|i| ImageOut { width: i.width, height: i.height, png: b64::encode(&i.png) }).collect()
}

/// An existing path inside the roots (for opening and validating).
fn existing(opts: &Options, path: &str) -> Result<PathBuf, String> {
    let p = script::resolve_path(&opts.roots, path)?;
    p.canonicalize().map_err(|e| format!("{path}: {e}"))
}

/// Answers `req` against `session`: shared by the headless workspace and the desktop app.
pub fn handle_with(session: &mut Session, engine: &mut Engine, opts: &Options, mode: &str, req: Request) -> Response {
    match req {
        Request::Execute(x) => match engine.execute(session, &x, opts) {
            Ok(o) => {
                let imgs = images(&o.images);
                let mut r = Response::ok(serde_json::to_value(&o).unwrap_or_default());
                r.images = imgs;
                r
            }
            Err(e) => Response { error: Some(serde_json::to_value(&e).unwrap_or_default()), ..Default::default() },
        },
        Request::GetState(_) => Response::ok(state(session, mode)),
        Request::Screenshot(s) => {
            let mut code = String::from("screenshot(");
            code.push_str(&serde_json::to_string(&s.ids).unwrap_or_else(|_| "null".into()));
            code.push_str(&format!(", {})", json!({ "scale": s.scale, "padding": s.padding })));
            match engine.execute(session, &script::Execute { code: Some(code), ..Default::default() }, opts) {
                Ok(o) => {
                    let mut r = Response::ok(json!({ "width": o.images.first().map(|i| i.width), "height": o.images.first().map(|i| i.height) }));
                    r.images = images(&o.images);
                    r
                }
                Err(e) => Response::err(e.message),
            }
        }
        Request::Validate(v) => {
            let report = match (v.path, v.json) {
                (Some(_), Some(_)) => return Response::err("give either `path` or `json`, not both"),
                (None, Some(j)) => validate::validate_value(j),
                (Some(p), None) => {
                    let p = match existing(opts, &p) {
                        Ok(p) => p,
                        Err(e) => return Response::err(e),
                    };
                    let file = if p.is_dir() { p.join(format::DOCUMENT_FILE) } else { p };
                    match std::fs::read(&file) {
                        Ok(b) => validate::validate_bytes(&b),
                        Err(e) => return Response::err(format!("{}: {e}", file.display())),
                    }
                }
                (None, None) => validate::validate_value(serde_json::from_str(&format::to_json(session.doc())).unwrap_or_default()),
            };
            Response::ok(serde_json::to_value(report).unwrap_or_default())
        }
        Request::ReadGuide(g) => match g.name {
            None => Response::ok(json!({ "guides": guides::list(), "text": guides::index() })),
            Some(n) => match guides::get(&n) {
                Some(text) => Response::ok(json!({ "name": n, "text": text })),
                None => Response::err(format!("no guide named {n:?}; available: {}", guides::NAMES.join(", "))),
            },
        },
        Request::Open(p) => {
            if session.is_dirty() && mode == "app" {
                return Response::err("the open document has unsaved changes; save it first (save), then open");
            }
            let path = match existing(opts, &p.path) {
                Ok(p) => p,
                Err(e) => return Response::err(e),
            };
            match Session::open(&path, session.fonts().clone()) {
                Ok(s) => {
                    *session = s;
                    Response::ok(state(session, mode))
                }
                Err(e) => Response::err(e.to_string()),
            }
        }
        Request::Save(s) => {
            let r = match s.path {
                Some(p) => match script::resolve_path(&opts.roots, &p) {
                    Ok(path) => session.save_as(path),
                    Err(e) => return Response::err(e),
                },
                None if session.path().is_none() => return Response::err("this document has never been saved; pass a `path` (a .canvas folder)"),
                None => session.save(),
            };
            match r {
                Ok(()) => Response::ok(json!({ "path": session.path().map(|p| p.display().to_string()) })),
                Err(e) => Response::err(e.to_string()),
            }
        }
        Request::New(_) => {
            if session.is_dirty() && mode == "app" {
                return Response::err("the open document has unsaved changes; save it first");
            }
            match Session::untitled(session.fonts().clone()) {
                Ok(s) => {
                    *session = s;
                    Response::ok(state(session, mode))
                }
                Err(e) => Response::err(e.to_string()),
            }
        }
        Request::Undo(_) => match session.undo() {
            Ok(label) => Response::ok(json!({ "undone": label })),
            Err(e) => Response::err(e.to_string()),
        },
        Request::Redo(_) => match session.redo() {
            Ok(label) => Response::ok(json!({ "redone": label })),
            Err(e) => Response::err(e.to_string()),
        },
    }
}

/// `get_state`'s answer.
pub fn state(s: &Session, mode: &str) -> Value {
    let doc = s.doc();
    let pages: Vec<Value> = doc
        .pages()
        .iter()
        .filter_map(|&k| doc.node(k).map(|n| (k, n)))
        .map(|(_, n)| json!({ "id": n.id.to_string(), "name": n.name, "topLevel": n.children.len() }))
        .collect();
    let selection: Vec<Value> = s
        .selection()
        .iter()
        .filter_map(|&id| doc.key(id))
        .map(|k| script::node_json(s, k, 0, Some(&["name".into(), "bounds".into()])))
        .collect();
    let v = s.view();
    json!({
        "mode": mode,
        "document": {
            "path": s.path().map(|p| p.display().to_string()),
            "dirty": s.is_dirty(),
            // All pages; counting one page's nodes would walk it, too slow at 100k nodes.
            "nodes": doc.len().saturating_sub(doc.pages().len()),
            "pages": pages,
            "currentPage": s.page().to_string(),
        },
        "selection": selection,
        "viewport": { "x": v.x0, "y": v.y0, "width": v.width(), "height": v.height(), "zoom": s.zoom() },
        "undo": s.store().undo_label(),
        "redo": s.store().redo_label(),
        "formatVersion": format::FORMAT_VERSION,
        "protocolVersion": crate::PROTOCOL_VERSION,
        "generator": format::GENERATOR,
    })
}
