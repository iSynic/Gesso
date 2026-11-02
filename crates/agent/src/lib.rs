//! The agent protocol shared by the CLI, the MCP server and the desktop app: the requests an
//! agent can make (the command registry, each with a JSON Schema), the [`Host`] that answers
//! them, the headless [`Workspace`] host, local IPC to a running app, document validation and the
//! agent guides.

pub mod b64;
pub mod guides;
pub mod ipc;
pub mod validate;
mod workspace;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use workspace::{Workspace, handle_with, state};

/// Bumped when requests or responses change incompatibly; checked in the IPC handshake.
pub const PROTOCOL_VERSION: u32 = 1;

/// One request. Serialised as `{"method": "...", "params": {...}}`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Request {
    /// Run a JavaScript program against the document as one undoable step.
    Execute(script::Execute),
    /// The open document, page, selection, viewport and versions.
    GetState(Empty),
    /// A PNG of nodes on the current page, or of the whole page.
    Screenshot(Screenshot),
    /// Check a document file (or JSON) against the schema and the document rules.
    Validate(Validate),
    /// An agent guide (Markdown); without a name, the list of guides.
    ReadGuide(ReadGuide),
    /// Open a `.canvas` document folder.
    Open(PathArg),
    /// Save the document (to `path`, or where it was opened from).
    Save(SaveArg),
    /// Start a new, empty document.
    New(Empty),
    Undo(Empty),
    Redo(Empty),
}

impl Request {
    pub fn name(&self) -> &'static str {
        match self {
            Request::Execute(_) => "execute",
            Request::GetState(_) => "get_state",
            Request::Screenshot(_) => "screenshot",
            Request::Validate(_) => "validate",
            Request::ReadGuide(_) => "read_guide",
            Request::Open(_) => "open",
            Request::Save(_) => "save",
            Request::New(_) => "new",
            Request::Undo(_) => "undo",
            Request::Redo(_) => "redo",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Empty {}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Screenshot {
    /// Node ids on the current page; omit for the whole page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ids: Option<Vec<String>>,
    /// Pixels per unit (default 1; reduced so the longest side stays within 2048 px).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
    /// Extra space around the content, in units.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub padding: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Validate {
    /// A `.canvas` folder or a `document.json` file. Omit to validate the open document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// A document as JSON (the `document.json` content), instead of a path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json: Option<Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadGuide {
    /// Guide name, e.g. `"start"` or `"api"`. Omit for the list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PathArg {
    /// A `.canvas` document folder.
    pub path: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveArg {
    /// Where to save (a `.canvas` folder). Omit to save in place.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// A PNG attached to a response.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ImageOut {
    pub width: u32,
    pub height: u32,
    /// Base64-encoded PNG.
    pub png: String,
}

/// The answer to a request: `result` on success, `error` (with a `message`, and for scripts the
/// failing `line`, `column`, `sourceLine` and `scriptId`) on failure.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Response {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImageOut>,
}

impl Response {
    pub fn ok(result: Value) -> Self {
        Response { result: Some(result), ..Default::default() }
    }

    pub fn err(message: impl Into<String>) -> Self {
        Response { error: Some(serde_json::json!({ "message": message.into() })), ..Default::default() }
    }

    pub fn is_ok(&self) -> bool {
        self.error.is_none()
    }

    /// The error's message, if this is an error.
    pub fn message(&self) -> Option<&str> {
        self.error.as_ref().and_then(|e| e.get("message")).and_then(Value::as_str)
    }
}

/// Answers requests: the headless [`Workspace`], or the desktop app.
pub trait Host {
    fn handle(&mut self, req: Request) -> Response;
}

/// JSON Schema of a request type's parameters, for tool listings.
pub fn params_schema<T: JsonSchema>() -> Value {
    let mut s = serde_json::to_value(schemars::schema_for!(T)).unwrap_or_default();
    if let Some(o) = s.as_object_mut() {
        o.remove("$schema");
        o.remove("title");
    }
    s
}

/// The tools an agent sees: name, description and parameter schema, in a stable order.
pub fn tools() -> Vec<(&'static str, &'static str, Value)> {
    vec![
        ("execute", "Run a JavaScript program against the open document as one undoable step. The document API (insert, update, replace, move, copy, remove, get, find, setVariables, getVariables, createComponent, detach, findEmptySpace, screenshot, exportImage, select, setPage, print) is described in read_guide(\"api\"). Returns the program's value, or an error with the failing line; fix it by resending, or with `retry` and line `edits`.", params_schema::<script::Execute>()),
        ("get_state", "The open document, its pages, the current page, selection, viewport, undo/redo labels and the schema version. Call this first.", params_schema::<Empty>()),
        ("screenshot", "A PNG of nodes on the current page (ids), or the whole page. Use it to check your work.", params_schema::<Screenshot>()),
        ("read_guide", "Guides for using this tool (Markdown): start, api, nodes, layout, design, components, recipes. Without a name, lists them. Read \"start\" before your first edit.", params_schema::<ReadGuide>()),
        ("validate", "Check a .canvas document (or document JSON) against the schema and the document rules; lists every problem with its JSON path.", params_schema::<Validate>()),
        ("open", "Open a .canvas document folder (within the allowed folders).", params_schema::<PathArg>()),
        ("save", "Save the document, in place or to a new .canvas folder path.", params_schema::<SaveArg>()),
    ]
}

/// Builds a request from a tool name and its JSON arguments.
pub fn request(tool: &str, args: Value) -> Result<Request, String> {
    let args = if args.is_null() { Value::Object(Default::default()) } else { args };
    let v = serde_json::json!({ "method": tool, "params": args });
    serde_json::from_value(v).map_err(|e| format!("{tool}: {e}"))
}
