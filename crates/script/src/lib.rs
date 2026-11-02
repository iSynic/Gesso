//! The `execute` sandbox: JavaScript (QuickJS via rquickjs) running against the document API as
//! one undoable transaction.
//!
//! - A fresh runtime and context per call, with CPU-time and memory limits; no filesystem or
//!   network access except through the API (`exportImage` writes only under configured roots).
//! - The script's edits apply immediately (reads see earlier writes) inside an open transaction
//!   on the [`Session`]; it commits as one undo step, or rolls back completely on any error.
//! - Errors carry the failing line and column; failed scripts are kept so a retry can send just
//!   the edited lines.

// `ScriptError` is returned once per script, never in a loop; boxing it would only make the
// public API clumsier.
#![allow(clippy::result_large_err)]

mod api;
mod components;
mod query;
mod vars;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use doc::{Origin, TxMeta};
use editor::Session;
use rquickjs::context::EvalOptions;
use rquickjs::{CatchResultExt, CaughtError, Context, Ctx, Exception, Function, Runtime};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use api::resolve_path;
pub use query::{node_json, Query};

const PRELUDE: &str = include_str!("prelude.js");
const SCRIPT_FILE: &str = "script";
/// Failed scripts kept for retries.
const KEEP_FAILED: usize = 16;

/// An `execute` request.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Execute {
    /// The JavaScript program. The value of its last expression (or of a top-level `return`) is
    /// the result. Optional when `retry` is given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// Re-run a failed script (its `scriptId` from the error) with `edits` applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<String>,
    /// Line edits for `retry`, applied bottom-up so line numbers refer to the failed script.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edits: Vec<LineEdit>,
    /// Make generated ids reproducible: the same seed and script give the same ids.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    /// CPU-time limit in ms (default 100, at most 10,000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

/// Replace lines `from..=to` (1-based) of a failed script with `text` (which may span several
/// lines, or be empty to delete). `to` defaults to `from`; `from = to + 1` inserts before `from`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LineEdit {
    pub from: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<usize>,
    pub text: String,
}

/// Host settings for scripts.
#[derive(Debug, Clone)]
pub struct Options {
    pub origin: Origin,
    pub timeout: Duration,
    pub memory_limit: usize,
    /// Directories `exportImage` may write under; empty disables it.
    pub roots: Vec<PathBuf>,
    /// Longest side of a screenshot, in pixels.
    pub max_screenshot_px: u32,
}

impl Default for Options {
    fn default() -> Self {
        Options { origin: Origin::Agent, timeout: Duration::from_millis(100), memory_limit: 64 << 20, roots: Vec::new(), max_screenshot_px: 2048 }
    }
}

/// A PNG made by `screenshot`.
#[derive(Debug, Clone)]
pub struct Image {
    pub ids: Vec<String>,
    pub width: u32,
    pub height: u32,
    pub png: Vec<u8>,
}

/// What a successful script did.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    /// The script's value as JSON (`null` when it has none).
    pub value: Value,
    /// Lines from `print`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub logs: Vec<String>,
    /// Ids returned by `insert`, `replace` and `copy`, in call order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub created: Vec<String>,
    pub changes: Changes,
    /// The undo step's label, or `None` when nothing changed.
    pub undo_label: Option<String>,
    pub ms: f64,
    /// Files written by `exportImage`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub exports: Vec<String>,
    #[serde(skip)]
    pub images: Vec<Image>,
}

/// Counts of changed nodes.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct Changes {
    pub inserted: usize,
    pub updated: usize,
    pub moved: usize,
    pub deleted: usize,
}

/// Why a script failed. Nothing it did was kept.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScriptError {
    pub message: String,
    /// 1-based position in the script, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<usize>,
    /// The failing line's text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_line: Option<String>,
    /// Pass as `retry` (with `edits`) to re-run this script with a fix.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script_id: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub logs: Vec<String>,
}

impl ScriptError {
    fn plain(message: impl Into<String>) -> Self {
        ScriptError { message: message.into(), line: None, column: None, source_line: None, script_id: None, logs: Vec::new() }
    }
}

impl std::fmt::Display for ScriptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.line, self.column) {
            (Some(l), Some(c)) => write!(f, "line {l}:{c}: {}", self.message),
            (Some(l), None) => write!(f, "line {l}: {}", self.message),
            _ => f.write_str(&self.message),
        }
    }
}

/// Runs scripts for one host. Keeps failed scripts for retries, and a spare session: the script's
/// host functions must own what they touch (rquickjs closures are `'static`), so the real session
/// is swapped into shared ownership for the run and swapped back afterwards.
#[derive(Default)]
pub struct Engine {
    spare: Option<Session>,
    renderer: Option<render_cpu::CpuRenderer>,
    failed: VecDeque<(String, String)>,
    next_id: u64,
}

/// Everything the host functions reach, shared with them for one run.
pub(crate) struct State {
    pub session: Session,
    pub renderer: Option<render_cpu::CpuRenderer>,
    pub opts: Options,
    pub out: Outcome,
    /// Edits since the scene last saw the transaction (reads that need layout sync first).
    pub dirty: bool,
}

impl Engine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Runs one `execute` request against `session`.
    pub fn execute(&mut self, session: &mut Session, req: &Execute, opts: &Options) -> Result<Outcome, ScriptError> {
        let code = self.resolve_code(req)?;
        let mut opts = opts.clone();
        if let Some(ms) = req.timeout_ms {
            opts.timeout = Duration::from_millis(ms.clamp(1, 10_000));
        }
        let label = label_of(&code);
        let spare = match self.spare.take() {
            Some(s) => s,
            None => Session::untitled(session.fonts().clone()).map_err(|e| ScriptError::plain(format!("cannot start the script host: {e}")))?,
        };
        if let Err(e) = session.begin(TxMeta::new(opts.origin, label.clone())) {
            self.spare = Some(spare);
            return Err(ScriptError::plain(e.to_string()));
        }
        if let Some(seed) = req.seed {
            session.seed_ids(seed);
        }
        let real = std::mem::replace(session, spare);
        let state = Rc::new(RefCell::new(State { session: real, renderer: self.renderer.take(), opts: opts.clone(), out: Outcome::default(), dirty: false }));
        let t0 = Instant::now();
        let result = run_js(&state, &code, &opts);
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        // The runtime is gone, so its host functions (the other owners) are too.
        let state = match Rc::try_unwrap(state) {
            Ok(cell) => cell.into_inner(),
            Err(_) => return Err(ScriptError::plain("internal error: script host still referenced")),
        };
        self.spare = Some(std::mem::replace(session, state.session));
        self.renderer = state.renderer;
        if req.seed.is_some() {
            session.random_ids();
        }
        let mut out = state.out;
        out.ms = ms;
        match result {
            Ok(value) => match session.commit() {
                Ok(changes) => {
                    out.value = value;
                    out.changes = Changes { inserted: changes.inserted.len(), updated: changes.updated.len(), moved: changes.moved.len(), deleted: changes.deleted.len() };
                    let changed = !changes.is_empty();
                    out.undo_label = changed.then_some(label);
                    Ok(out)
                }
                Err(e) => Err(self.fail(code, ScriptError { logs: out.logs, ..ScriptError::plain(format!("the edits could not be laid out: {e}")) })),
            },
            Err(mut e) => {
                session.rollback();
                e.logs = out.logs;
                Err(self.fail(code, e))
            }
        }
    }

    fn fail(&mut self, code: String, mut e: ScriptError) -> ScriptError {
        self.next_id += 1;
        let id = format!("s{}", self.next_id);
        if let Some(l) = e.line {
            e.source_line = code.lines().nth(l.saturating_sub(1)).map(|s| clip(s.trim_end(), e.column.unwrap_or(1)));
        }
        e.script_id = Some(id.clone());
        self.failed.push_back((id, code));
        while self.failed.len() > KEEP_FAILED {
            self.failed.pop_front();
        }
        e
    }

    /// The program to run: `code`, or a failed script with line edits applied.
    fn resolve_code(&self, req: &Execute) -> Result<String, ScriptError> {
        match (&req.code, &req.retry) {
            (Some(code), None) => Ok(code.clone()),
            (None, Some(id)) => {
                let base = self.failed.iter().find(|(k, _)| k == id).map(|(_, c)| c.clone()).ok_or_else(|| {
                    ScriptError::plain(format!("unknown scriptId {id:?}: only the last {KEEP_FAILED} failed scripts are kept; send the whole script as `code`"))
                })?;
                apply_edits(&base, &req.edits).map_err(ScriptError::plain)
            }
            (Some(_), Some(_)) => Err(ScriptError::plain("give either `code` or `retry`, not both")),
            (None, None) => Err(ScriptError::plain("`code` is required")),
        }
    }
}

/// At most 200 characters of a line, around `column` (1-based) when the line is longer.
fn clip(line: &str, column: usize) -> String {
    const MAX: usize = 200;
    let chars: Vec<char> = line.chars().collect();
    if chars.len() <= MAX {
        return line.to_string();
    }
    let start = column.saturating_sub(MAX / 2).min(chars.len() - MAX);
    let mut s: String = chars[start..start + MAX].iter().collect();
    if start > 0 {
        s.insert(0, '…');
    }
    if start + MAX < chars.len() {
        s.push('…');
    }
    s
}

/// Applies line edits (bottom-up, so every edit's numbers refer to the original lines).
pub fn apply_edits(code: &str, edits: &[LineEdit]) -> Result<String, String> {
    let mut lines: Vec<String> = code.lines().map(str::to_string).collect();
    let mut edits: Vec<&LineEdit> = edits.iter().collect();
    edits.sort_by_key(|e| std::cmp::Reverse(e.from));
    for e in edits {
        let to = e.to.unwrap_or(e.from);
        if e.from == 0 || e.from > lines.len() + 1 || to + 1 < e.from || to > lines.len() {
            return Err(format!("edit lines {}..={to} are outside the script's {} lines", e.from, lines.len()));
        }
        let new: Vec<String> = if e.text.is_empty() { Vec::new() } else { e.text.lines().map(str::to_string).collect() };
        lines.splice(e.from - 1..to, new);
    }
    Ok(lines.join("\n"))
}

/// The undo label: a `// label: …` first line, else "Script".
fn label_of(code: &str) -> String {
    code.lines()
        .find(|l| !l.trim().is_empty())
        .and_then(|l| l.trim().strip_prefix("//"))
        .and_then(|l| l.trim().strip_prefix("label:"))
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| "Script".into())
}

fn run_js(state: &Rc<RefCell<State>>, code: &str, opts: &Options) -> Result<Value, ScriptError> {
    let rt = Runtime::new().map_err(|e| ScriptError::plain(e.to_string()))?;
    let deadline = Instant::now() + opts.timeout;
    rt.set_interrupt_handler(Some(Box::new(move || Instant::now() > deadline)));
    rt.set_memory_limit(opts.memory_limit);
    rt.set_max_stack_size(1 << 20);
    let ctx = Context::full(&rt).map_err(|e| ScriptError::plain(e.to_string()))?;
    let limit_error = |e: ScriptError| -> ScriptError {
        if Instant::now() > deadline {
            ScriptError { message: format!("the script ran past its CPU limit of {} ms (pass timeoutMs to raise it, up to 10000)", opts.timeout.as_millis()), ..e }
        } else if e.message.contains("out of memory") {
            ScriptError { message: format!("the script ran out of memory (limit {} MB)", opts.memory_limit >> 20), ..e }
        } else {
            e
        }
    };
    ctx.with(|ctx| {
        install(&ctx, state).map_err(|e| ScriptError::plain(format!("cannot start the script host: {e}")))?;
        let mut o = EvalOptions::default();
        o.filename = Some(SCRIPT_FILE.into());
        o.strict = false;
        let mut value = ctx.eval_with_options::<rquickjs::Value, _>(code, o).catch(&ctx).map_err(|e| caught(e, 0));
        // A top-level `return` is a syntax error in a script; run it as a function body instead,
        // on the same line so positions stay right (only line 1's column shifts).
        if let Err(e) = &value
            && e.message.contains("return not in a function")
        {
            const OPEN: &str = "(function () {";
            let mut o = EvalOptions::default();
            o.filename = Some(SCRIPT_FILE.into());
            o.strict = false;
            value = ctx.eval_with_options::<rquickjs::Value, _>(format!("{OPEN}{code}\n}})()"), o).catch(&ctx).map_err(|e| caught(e, OPEN.len()));
        }
        let value = value.map_err(limit_error)?;
        match ctx.json_stringify(value) {
            Ok(Some(s)) => serde_json::from_str(&s.to_string().unwrap_or_default()).map_err(|e| ScriptError::plain(e.to_string())),
            Ok(None) => Ok(Value::Null),
            Err(e) => Err(ScriptError::plain(format!("the script's value can't be converted to JSON: {e}"))),
        }
    })
}

/// Installs `__call` and the prelude.
fn install<'js>(ctx: &Ctx<'js>, state: &Rc<RefCell<State>>) -> rquickjs::Result<()> {
    let st = state.clone();
    let call = Function::new(ctx.clone(), move |ctx: Ctx<'js>, name: String, args: String| -> rquickjs::Result<rquickjs::Value<'js>> {
        let args: Vec<Value> = serde_json::from_str(&args).unwrap_or_default();
        let r = match st.try_borrow_mut() {
            Ok(mut s) => api::call(&mut s, &name, &args),
            Err(_) => Err("the document API can't be called re-entrantly".into()),
        };
        match r {
            Ok(Value::Null) if name != "get" => Ok(rquickjs::Value::new_undefined(ctx)),
            Ok(v) => to_js(&ctx, &v),
            Err(msg) => Err(Exception::throw_message(&ctx, &format!("{}: {msg}", api::public_name(&name)))),
        }
    })?;
    ctx.globals().set("__call", call)?;
    let mut o = EvalOptions::default();
    o.filename = Some("prelude.js".into());
    ctx.eval_with_options::<(), _>(PRELUDE, o)?;
    Ok(())
}

/// Builds the JS value for an API result directly (faster than JSON text and `JSON.parse` for
/// large results such as thousands of ids).
fn to_js<'js>(ctx: &Ctx<'js>, v: &Value) -> rquickjs::Result<rquickjs::Value<'js>> {
    Ok(match v {
        Value::Null => rquickjs::Value::new_null(ctx.clone()),
        Value::Bool(b) => rquickjs::Value::new_bool(ctx.clone(), *b),
        Value::Number(n) => match n.as_i64().and_then(|i| i32::try_from(i).ok()) {
            Some(i) => rquickjs::Value::new_int(ctx.clone(), i),
            None => rquickjs::Value::new_float(ctx.clone(), n.as_f64().unwrap_or(f64::NAN)),
        },
        Value::String(s) => rquickjs::String::from_str(ctx.clone(), s)?.into_value(),
        Value::Array(a) => {
            let arr = rquickjs::Array::new(ctx.clone())?;
            for (i, x) in a.iter().enumerate() {
                arr.set(i, to_js(ctx, x)?)?;
            }
            arr.into_value()
        }
        Value::Object(o) => {
            let obj = rquickjs::Object::new(ctx.clone())?;
            for (k, x) in o {
                obj.set(k.as_str(), to_js(ctx, x)?)?;
            }
            obj.into_value()
        }
    })
}

/// Turns a caught JS error into a [`ScriptError`], with the position of the innermost frame in the
/// script. `shift` is the column offset added to line 1 by wrapping.
fn caught(e: CaughtError<'_>, shift: usize) -> ScriptError {
    let (message, stack) = match e {
        CaughtError::Exception(ex) => {
            let msg = ex.message().unwrap_or_default();
            let name = ex.get::<_, String>("name").ok().filter(|n| n != "Error" && !n.is_empty());
            (name.map_or(msg.clone(), |n| format!("{n}: {msg}")), ex.stack().unwrap_or_default())
        }
        CaughtError::Value(v) => (format!("uncaught value: {}", v.as_string().and_then(|s| s.to_string().ok()).unwrap_or_else(|| format!("{v:?}"))), String::new()),
        CaughtError::Error(e) => (e.to_string(), String::new()),
    };
    let mut err = ScriptError::plain(message);
    if let Some((line, col)) = position(&stack, SCRIPT_FILE) {
        err.line = Some(line);
        err.column = col.map(|c| if line == 1 { c.saturating_sub(shift).max(1) } else { c });
    }
    err
}

/// The first `file:line[:col]` frame for `file` in a QuickJS stack trace (`at f (file:3:5)`, or
/// `at file:3:5` for syntax errors).
fn position(stack: &str, file: &str) -> Option<(usize, Option<usize>)> {
    let pat = format!("{file}:");
    let i = stack.match_indices(&pat).find(|(i, _)| *i == 0 || matches!(stack.as_bytes()[i - 1], b'(' | b' '))?.0 + pat.len();
    let rest = &stack[i..];
    let digits = |s: &str| s.chars().take_while(char::is_ascii_digit).collect::<String>();
    let line_s = digits(rest);
    let line = line_s.parse().ok()?;
    let col = rest[line_s.len()..].strip_prefix(':').and_then(|c| digits(c).parse().ok());
    Some((line, col))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_apply_bottom_up() {
        let code = "a\nb\nc\nd";
        let e = |from, to: Option<usize>, text: &str| LineEdit { from, to, text: text.into() };
        assert_eq!(apply_edits(code, &[e(2, None, "B"), e(4, None, "D1\nD2")]).unwrap_or_default(), "a\nB\nc\nD1\nD2");
        assert_eq!(apply_edits(code, &[e(2, Some(3), "")]).unwrap_or_default(), "a\nd");
        assert_eq!(apply_edits(code, &[e(5, Some(4), "e")]).unwrap_or_default(), "a\nb\nc\nd\ne");
        assert!(apply_edits(code, &[e(9, None, "x")]).is_err());
    }

    #[test]
    fn labels() {
        assert_eq!(label_of("// label: Add login form\ninsert()"), "Add login form");
        assert_eq!(label_of("insert()"), "Script");
    }

    #[test]
    fn stack_positions() {
        assert_eq!(position("    at <anonymous> (prelude.js:4:13)\n    at <eval> (script:3:5)\n", "script"), Some((3, Some(5))));
        assert_eq!(position("    at <eval> (script:7)\n", "script"), Some((7, None)));
        assert_eq!(position("    at script:1:8
", "script"), Some((1, Some(8))));
        assert_eq!(position("    at f (eval_script:2:1)
", "script"), None);
        assert_eq!(position("", "script"), None);
    }
}
