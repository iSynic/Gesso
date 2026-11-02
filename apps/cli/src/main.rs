//! `canvas`: the headless command line. Every command is an `agent::Request` answered by an
//! `agent::Workspace`, the same code path the MCP server uses when no app is running.
//!
//! ```text
//! canvas render <doc> [--page N|NAME] [--node ID]... [--scale S] [--padding P] -o OUT.png
//! canvas export <doc> [--page N|NAME] [--node ID]... [--format png|jpeg|webp|svg|pdf] [--scale S] [--quality Q] [--padding P] -o OUT
//! canvas validate <doc|document.json> [--json]
//! canvas run [doc] <script.js> [--seed N] [--timeout MS] [--save | -o OUT.canvas] [--screenshot OUT.png]
//! canvas state <doc>
//! canvas schema
//! canvas guide [NAME]
//! ```
//!
//! Common flags: `--root DIR` (repeatable) adds a folder that scripts may export into; the
//! current folder and the folders of the paths given on the command line are always allowed.
//! `--deterministic` uses the bundled fonts only, so output is the same on every machine.
//! Exit codes: 0 success, 1 the command failed (invalid document, script error), 2 bad usage.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use agent::{Host, Request, Response, Workspace};
use serde_json::{Value, json};

const USAGE: &str = "usage:
  canvas render <doc> [--page N|NAME] [--node ID]... [--scale S] [--padding P] -o OUT.png
  canvas export <doc> [--page N|NAME] [--node ID]... [--format png|jpeg|webp|svg|pdf] [--scale S] [--quality Q] [--padding P] -o OUT
  canvas validate <doc|document.json> [--json]
  canvas run [doc] <script.js> [--seed N] [--timeout MS] [--save | -o OUT.canvas] [--screenshot OUT.png]
  canvas state <doc>
  canvas schema
  canvas guide [NAME]
flags: --root DIR (repeatable), --deterministic";

/// Flags that take a value; everything else starting with `-` is a switch.
const VALUED: &[&str] = &["--page", "--node", "--scale", "--padding", "--format", "--quality", "-o", "--out", "--seed", "--timeout", "--screenshot", "--root"];

struct Args {
    positional: Vec<String>,
    flags: Vec<(String, Option<String>)>,
}

impl Args {
    fn parse(raw: Vec<String>) -> Result<Args, String> {
        let mut a = Args { positional: Vec::new(), flags: Vec::new() };
        let mut it = raw.into_iter();
        while let Some(s) = it.next() {
            if s.starts_with('-') && s.len() > 1 {
                if VALUED.contains(&s.as_str()) {
                    let v = it.next().ok_or_else(|| format!("{s} needs a value"))?;
                    a.flags.push((s, Some(v)));
                } else {
                    a.flags.push((s, None));
                }
            } else {
                a.positional.push(s);
            }
        }
        Ok(a)
    }

    fn get(&self, name: &str) -> Option<&str> {
        self.flags.iter().rev().find(|(n, _)| n == name).and_then(|(_, v)| v.as_deref())
    }

    fn all(&self, name: &str) -> Vec<&str> {
        self.flags.iter().filter(|(n, _)| n == name).filter_map(|(_, v)| v.as_deref()).collect()
    }

    fn has(&self, name: &str) -> bool {
        self.flags.iter().any(|(n, _)| n == name)
    }

    fn num(&self, name: &str) -> Result<Option<f64>, Fail> {
        self.get(name).map(|v| v.parse::<f64>().map_err(|_| usage(format!("{name}: not a number: {v}")))).transpose()
    }

    /// Fails on flags the command doesn't know, so typos aren't silently ignored.
    fn only(&self, allowed: &[&str]) -> Result<(), Fail> {
        let common = ["--root", "--deterministic"];
        match self.flags.iter().find(|(n, _)| !allowed.contains(&n.as_str()) && !common.contains(&n.as_str())) {
            Some((n, _)) => Err(usage(format!("unknown flag {n} for this command"))),
            None => Ok(()),
        }
    }
}

enum Fail {
    Usage(String),
    Failed(String),
}

impl From<String> for Fail {
    fn from(s: String) -> Self {
        Fail::Failed(s)
    }
}

fn usage(s: impl Into<String>) -> Fail {
    Fail::Usage(s.into())
}

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let r = match Args::parse(raw) {
        Ok(a) => run(a),
        Err(e) => Err(usage(e)),
    };
    match r {
        Ok(code) => code,
        Err(Fail::Usage(m)) => {
            eprintln!("canvas: {m}\n{USAGE}");
            ExitCode::from(2)
        }
        Err(Fail::Failed(m)) => {
            eprintln!("canvas: {m}");
            ExitCode::FAILURE
        }
    }
}

fn run(a: Args) -> Result<ExitCode, Fail> {
    let Some(cmd) = a.positional.first().cloned() else { return Err(usage("no command")) };
    let rest: Vec<String> = a.positional[1..].to_vec();
    match cmd.as_str() {
        "render" => render(&a, &rest),
        "export" => export(&a, &rest),
        "validate" => validate(&a, &rest),
        "run" => run_script(&a, &rest),
        "state" => {
            a.only(&[])?;
            let [doc] = rest.as_slice() else { return Err(usage("state takes one document")) };
            let mut w = workspace(&a, Some(doc), &[])?;
            print_json(&ok(w.handle(Request::GetState(agent::Empty {})))?);
            Ok(ExitCode::SUCCESS)
        }
        "schema" => {
            a.only(&[])?;
            print_json(&format::schema());
            Ok(ExitCode::SUCCESS)
        }
        "guide" => {
            a.only(&[])?;
            let name = match rest.as_slice() {
                [] => None,
                [n] => Some(n.clone()),
                _ => return Err(usage("guide takes at most one name")),
            };
            let none = name.is_none();
            let r = ok(handle_new(Request::ReadGuide(agent::ReadGuide { name }))?)?;
            if none {
                for g in r["guides"].as_array().into_iter().flatten() {
                    println!("{:<9} {}", g["name"].as_str().unwrap_or(""), g["summary"].as_str().unwrap_or(""));
                }
            } else {
                print!("{}", r["text"].as_str().unwrap_or(""));
            }
            Ok(ExitCode::SUCCESS)
        }
        "help" | "--help" | "-h" => {
            println!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        other => Err(usage(format!("unknown command {other}"))),
    }
}

/// A request that needs no document (guides).
fn handle_new(req: Request) -> Result<Response, Fail> {
    let mut w = Workspace::new(text::FontDb::bundled_only(), Vec::new())?;
    Ok(w.handle(req))
}

/// The allowed folders: the current one, `--root`s, and the folders of `paths`.
fn roots(a: &Args, paths: &[&str]) -> Vec<PathBuf> {
    let mut r: Vec<PathBuf> = std::env::current_dir().into_iter().collect();
    r.extend(a.all("--root").into_iter().map(PathBuf::from));
    for p in paths {
        let p = absolute(p);
        // A folder argument (a .canvas bundle) is allowed through its parent.
        if let Some(parent) = p.parent() {
            r.push(parent.to_path_buf());
        }
    }
    r.retain(|p| p.is_dir());
    r
}

fn absolute(p: &str) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| PathBuf::from(p))
}

/// A workspace with `doc` open (or untitled), allowing `extra` output paths.
fn workspace(a: &Args, doc: Option<&String>, extra: &[&str]) -> Result<Workspace, Fail> {
    let fonts = if a.has("--deterministic") { text::FontDb::bundled_only() } else { text::FontDb::new(true) };
    let mut paths: Vec<&str> = extra.to_vec();
    paths.extend(doc.map(String::as_str));
    let roots = roots(a, &paths);
    match doc {
        Some(d) => Ok(Workspace::open(fonts, roots, &absolute(d).display().to_string())?),
        None => Ok(Workspace::new(fonts, roots)?),
    }
}

/// The result of a successful response, or its error as a failure.
fn ok(r: Response) -> Result<Value, Fail> {
    if r.is_ok() {
        return Ok(r.result.unwrap_or_default());
    }
    let e = r.error.unwrap_or_default();
    let mut m = e["message"].as_str().unwrap_or("failed").to_string();
    if let Some(line) = e["line"].as_u64() {
        m = format!("line {line}: {m}");
        if let Some(src) = e["sourceLine"].as_str() {
            m.push_str(&format!("\n  {src}"));
        }
    }
    Err(Fail::Failed(m))
}

fn print_json(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

fn write_png(path: &str, r: &Response) -> Result<(), Fail> {
    let img = r.images.first().ok_or_else(|| "no image was produced (is the page empty?)".to_string())?;
    let bytes = agent::b64::decode(&img.png).map_err(|e| format!("bad image data: {e}"))?;
    std::fs::write(path, bytes).map_err(|e| format!("{path}: {e}"))?;
    eprintln!("wrote {path} ({}x{})", img.width, img.height);
    Ok(())
}

fn screenshot(w: &mut Workspace, ids: Option<Vec<String>>, scale: Option<f64>, padding: Option<f64>) -> Response {
    w.handle(Request::Screenshot(agent::Screenshot { ids, scale, padding }))
}

fn render(a: &Args, rest: &[String]) -> Result<ExitCode, Fail> {
    a.only(&["--page", "--node", "--scale", "--padding", "-o", "--out"])?;
    let [doc] = rest else { return Err(usage("render takes one document")) };
    let out = a.get("-o").or(a.get("--out")).ok_or_else(|| usage("render needs -o OUT.png"))?;
    let mut w = workspace(a, Some(doc), &[out])?;
    set_page(a, &mut w)?;
    let nodes: Vec<String> = a.all("--node").into_iter().map(String::from).collect();
    let ids = (!nodes.is_empty()).then_some(nodes);
    let r = screenshot(&mut w, ids, a.num("--scale")?, a.num("--padding")?);
    ok(r.clone())?;
    write_png(out, &r)?;
    Ok(ExitCode::SUCCESS)
}

/// `--page`: a 1-based number, a page name, or a page id.
fn set_page(a: &Args, w: &mut Workspace) -> Result<(), Fail> {
    if let Some(p) = a.get("--page") {
        let st = ok(w.handle(Request::GetState(agent::Empty {})))?;
        let pages = st["document"]["pages"].as_array().cloned().unwrap_or_default();
        // A 1-based number, a page name, or a page id.
        let found = p
            .parse::<usize>()
            .ok()
            .and_then(|n| n.checked_sub(1))
            .and_then(|i| pages.get(i))
            .or_else(|| pages.iter().find(|pg| pg["name"] == p || pg["id"] == p));
        let Some(id) = found.and_then(|pg| pg["id"].as_str()) else {
            let names: Vec<&str> = pages.iter().filter_map(|pg| pg["name"].as_str()).collect();
            return Err(Fail::Failed(format!("no page {p:?}; pages: {}", names.join(", "))));
        };
        let code = format!("setPage({})", json!(id));
        ok(w.handle(Request::Execute(script::Execute { code: Some(code), ..Default::default() })))?;
    }
    Ok(())
}

/// Exports at full detail through the script's `exportImage`, so the CLI, agents and scripts
/// share one path.
fn export(a: &Args, rest: &[String]) -> Result<ExitCode, Fail> {
    a.only(&["--page", "--node", "--format", "--scale", "--quality", "--padding", "-o", "--out"])?;
    let [doc] = rest else { return Err(usage("export takes one document")) };
    let out = a.get("-o").or(a.get("--out")).ok_or_else(|| usage("export needs -o OUT"))?;
    let format = match a.get("--format") {
        Some(f) => f.to_string(),
        None => Path::new(out).extension().and_then(|e| e.to_str()).map(str::to_string).ok_or_else(|| usage("no --format and OUT has no extension"))?,
    };
    let mut w = workspace(a, Some(doc), &[out])?;
    set_page(a, &mut w)?;
    let nodes: Vec<String> = a.all("--node").into_iter().map(String::from).collect();
    let ids = if nodes.is_empty() { Value::Null } else { json!(nodes) };
    let mut opts = serde_json::Map::new();
    for (flag, key) in [("--scale", "scale"), ("--quality", "quality"), ("--padding", "padding")] {
        if let Some(v) = a.num(flag)? {
            opts.insert(key.into(), json!(v));
        }
    }
    let code = format!("exportImage({ids}, {}, {}, {})", json!(format), json!(absolute(out).display().to_string()), Value::Object(opts));
    let r = ok(w.handle(Request::Execute(script::Execute { code: Some(code), ..Default::default() })))?;
    eprintln!("wrote {out} ({}x{}, {} bytes)", r["value"]["width"], r["value"]["height"], r["value"]["bytes"]);
    Ok(ExitCode::SUCCESS)
}

fn validate(a: &Args, rest: &[String]) -> Result<ExitCode, Fail> {
    a.only(&["--json"])?;
    let [path] = rest else { return Err(usage("validate takes one document or document.json")) };
    let abs = absolute(path);
    let mut w = Workspace::new(text::FontDb::bundled_only(), roots(a, &[path]))?;
    let report = ok(w.handle(Request::Validate(agent::Validate { path: Some(abs.display().to_string()), json: None })))?;
    let valid = report["valid"].as_bool().unwrap_or(false);
    if a.has("--json") {
        print_json(&report);
    } else if valid {
        let migrated = if report["migrated"].as_bool().unwrap_or(false) { " (migrated from an older version)" } else { "" };
        println!("{path}: valid, {} nodes{migrated}", report["nodes"]);
    } else {
        for p in report["problems"].as_array().into_iter().flatten() {
            println!("{}: {}", p["path"].as_str().unwrap_or(""), p["message"].as_str().unwrap_or(""));
        }
        if let Some(more) = report["more"].as_u64().filter(|&n| n > 0) {
            println!("... and {more} more");
        }
        eprintln!("{path}: invalid");
    }
    Ok(if valid { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}

fn run_script(a: &Args, rest: &[String]) -> Result<ExitCode, Fail> {
    a.only(&["--seed", "--timeout", "--save", "-o", "--out", "--screenshot"])?;
    let (doc, js) = match rest {
        [js] => (None, js),
        [doc, js] => (Some(doc), js),
        _ => return Err(usage("run takes [doc] script.js")),
    };
    let out = a.get("-o").or(a.get("--out"));
    if a.has("--save") && (out.is_some() || doc.is_none()) {
        return Err(usage("--save writes back to the opened document; use -o for a new one"));
    }
    let shot = a.get("--screenshot");
    let code = std::fs::read_to_string(js).map_err(|e| format!("{js}: {e}"))?;
    let extra: Vec<&str> = out.into_iter().chain(shot).collect();
    let mut w = workspace(a, doc, &extra)?;
    let seed = a.get("--seed").map(|s| s.parse::<u64>().map_err(|_| usage(format!("--seed: not a number: {s}")))).transpose()?;
    let timeout_ms = a.get("--timeout").map(|s| s.parse::<u64>().map_err(|_| usage(format!("--timeout: not a number: {s}")))).transpose()?;
    let x = script::Execute { code: Some(code), seed, timeout_ms, ..Default::default() };
    let r = w.handle(Request::Execute(x));
    if !r.is_ok() {
        // The structured error (line, column, source line, logs) on stdout for tools; a short
        // form on stderr.
        print_json(&json!({ "error": r.error }));
        ok(r)?;
        return Ok(ExitCode::FAILURE);
    }
    let shots = r.images.clone();
    let mut result = r.result.unwrap_or_default();
    // Images the script asked for are written next to the script (or --screenshot's folder).
    if !shots.is_empty() {
        let stem = Path::new(js).file_stem().and_then(|s| s.to_str()).unwrap_or("script");
        let dir = shot.and_then(|s| Path::new(s).parent()).filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        let mut files = Vec::new();
        for (i, img) in shots.iter().enumerate() {
            let p = dir.join(format!("{stem}-{}.png", i + 1));
            let bytes = agent::b64::decode(&img.png).map_err(|e| format!("bad image data: {e}"))?;
            std::fs::write(&p, bytes).map_err(|e| format!("{}: {e}", p.display()))?;
            files.push(p.display().to_string());
        }
        result["imageFiles"] = json!(files);
    }
    print_json(&result);
    if let Some(s) = shot {
        let r = screenshot(&mut w, None, None, Some(40.0));
        ok(r.clone())?;
        write_png(s, &r)?;
    }
    if a.has("--save") {
        ok(w.handle(Request::Save(agent::SaveArg { path: None })))?;
        eprintln!("saved {}", doc.map_or("", |d| d.as_str()));
    } else if let Some(o) = out {
        ok(w.handle(Request::Save(agent::SaveArg { path: Some(absolute(o).display().to_string()) })))?;
        eprintln!("saved {o}");
    }
    Ok(ExitCode::SUCCESS)
}
