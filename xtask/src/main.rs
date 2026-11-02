//! Repository checks.
//!
//! `cargo xtask layers`: crates form layers, and a crate may only depend (normal and build
//! dependencies) on crates in lower layers. This keeps the core free of UI, file and app code
//! so it can also build for the CLI and WASM. Every workspace crate must be listed in `LAYERS`.

use std::process::{Command, ExitCode};

use serde_json::Value;

/// Lowest first.
const LAYERS: &[&[&str]] = &[
    &["doc"],
    &["text"],
    &["layout"],
    &["index"],
    &["format"],
    &["render"],
    &["render-cpu"],
    &["render-vello"],
    &["export"],
    &["editor"],
    &["script"],
    &["agent"],
    // Apps and tools.
    &["bench", "desktop", "cli", "mcp", "xtask"],
];

fn layer(name: &str) -> Option<usize> {
    LAYERS.iter().position(|l| l.contains(&name))
}

fn layers() -> Result<Vec<String>, String> {
    let out = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .output()
        .map_err(|e| format!("cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    let meta: Value = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    let packages = meta["packages"].as_array().cloned().unwrap_or_default();
    let members: Vec<&str> = packages.iter().filter_map(|p| p["name"].as_str()).collect();
    let mut errors = Vec::new();
    for p in &packages {
        let name = p["name"].as_str().unwrap_or("?");
        let Some(mine) = layer(name) else {
            errors.push(format!("{name}: not assigned to a layer in xtask/src/main.rs"));
            continue;
        };
        for d in p["dependencies"].as_array().into_iter().flatten() {
            let dep = d["name"].as_str().unwrap_or("?");
            if d["kind"].as_str() == Some("dev") || !members.contains(&dep) {
                continue;
            }
            match layer(dep) {
                Some(theirs) if theirs < mine => {}
                Some(theirs) => errors.push(format!("{name} (layer {mine}) depends on {dep} (layer {theirs}); only lower layers are allowed")),
                None => errors.push(format!("{name} depends on {dep}, which has no layer")),
            }
        }
    }
    if errors.is_empty() { Ok(members.iter().map(|m| m.to_string()).collect()) } else { Err(errors.join("\n")) }
}

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("layers") => match layers() {
            Ok(members) => {
                println!("layers ok ({} crates): {}", members.len(), LAYERS.iter().map(|l| l.join(", ")).collect::<Vec<_>>().join(" < "));
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!("usage: cargo xtask layers");
            ExitCode::from(2)
        }
    }
}
