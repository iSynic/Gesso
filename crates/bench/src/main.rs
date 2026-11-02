//! Benchmark harness.
//!
//! ```text
//! cargo run -p bench --release -- list
//! cargo run -p bench --release -- docs [DIR]   (benchmark documents as .canvas bundles)
//! cargo run -p bench --release -- run [--scenario NAME[,NAME]|all] [--out FILE]
//!                                     [--baseline FILE] [--threshold PCT] [--floor MS]
//! ```
//!
//! `--baseline` takes one run or several (comma-separated; the slowest p95 per metric is used).
//! Any metric whose p95 is more than `--threshold` percent (default 10) and
//! at least `--floor` ms (default 0.05) slower than in the baseline fails the run (exit 1).
//! Run in a fresh process with nothing else heavy running; memory figures are process deltas.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod agent;
mod canvas_edit;
mod components;
mod editor;
mod harness;
mod render;
mod export;
mod scenarios;

use std::process::ExitCode;

use serde_json::{Map, Value, json};

fn arg<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut all = scenarios::all();
    all.extend(render::all());
    all.extend(editor::all());
    all.extend(agent::all());
    all.extend(components::all());
    all.extend(export::all());
    all.extend(canvas_edit::all());
    match args.first().map(String::as_str) {
        Some("list") => {
            for s in &all {
                println!("{:<8} {}", s.name(), s.describe());
            }
            ExitCode::SUCCESS
        }
        Some("run") => {
            let pick = arg(&args, "--scenario").unwrap_or("all");
            let wanted: Vec<&str> = pick.split(',').collect();
            let mut results = Map::new();
            for s in all.iter().filter(|s| pick == "all" || wanted.contains(&s.name())) {
                eprintln!("{}: {}", s.name(), s.describe());
                let mut r = harness::Report::default();
                s.run(&mut r);
                results.insert(s.name().into(), r.into_json());
            }
            if results.is_empty() {
                eprintln!("no scenario named {pick}; try `list`");
                return ExitCode::from(2);
            }
            let mut host = harness::host();
            host["gpu"] = render::gpu_info();
            let run = json!({ "host": host, "scenarios": results });
            if let Some(out) = arg(&args, "--out") {
                if let Some(dir) = std::path::Path::new(out).parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                match std::fs::write(out, serde_json::to_string_pretty(&run).unwrap_or_default()) {
                    Ok(()) => eprintln!("wrote {out}"),
                    Err(e) => eprintln!("cannot write {out}: {e}"),
                }
            }
            let Some(base) = arg(&args, "--baseline") else { return ExitCode::SUCCESS };
            // Several baselines (comma-separated) gate against the slowest p95 of each metric,
            // so ordinary run-to-run spread doesn't fail the gate.
            let mut runs = Vec::new();
            for path in base.split(',') {
                match std::fs::read(path).map(|b| serde_json::from_slice::<Value>(&b)) {
                    Ok(Ok(v)) => runs.push(v),
                    _ => {
                        eprintln!("cannot read baseline {path}");
                        return ExitCode::from(2);
                    }
                }
            }
            let base = harness::slowest(runs);
            let threshold = arg(&args, "--threshold").and_then(|v| v.parse().ok()).unwrap_or(10.0);
            let floor = arg(&args, "--floor").and_then(|v| v.parse().ok()).unwrap_or(0.05);
            let bad = harness::compare(&base, &run, threshold, floor);
            if bad.is_empty() {
                eprintln!("\nno p95 regressed by more than {threshold}%");
                ExitCode::SUCCESS
            } else {
                eprintln!("\n{} regression(s):", bad.len());
                for b in bad {
                    eprintln!("  {b}");
                }
                ExitCode::FAILURE
            }
        }
        Some("docs") => {
            // The benchmark documents as .canvas bundles, for the canvas app's scripted
            // sessions. Images are left as placeholders (no asset files).
            let dir = std::path::PathBuf::from(args.get(1).map_or("target/bench-docs", String::as_str));
            for (name, cfg) in [
                ("100k.canvas", doc::testing::GenConfig { target_nodes: 100_000, auto_layout_heavy: true, ..Default::default() }),
                ("50mb.canvas", doc::testing::GenConfig { target_nodes: 64_500, seed: 2, ..Default::default() }),
            ] {
                let store = render::laid_out(&cfg);
                let path = dir.join(name);
                let r = format::Bundle::create(&path).and_then(|b| b.save(store.doc()));
                match r {
                    Ok(()) => eprintln!("wrote {} ({} nodes)", path.display(), store.doc().len()),
                    Err(e) => {
                        eprintln!("cannot write {}: {e}", path.display());
                        return ExitCode::FAILURE;
                    }
                }
            }
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("usage: bench list | bench docs [DIR] | bench run [--scenario NAME|all] [--out FILE] [--baseline FILE] [--threshold PCT] [--floor MS]");
            ExitCode::from(2)
        }
    }
}
