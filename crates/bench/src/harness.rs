//! Timing, host detection, the scenario interface and the baseline gate.

use std::time::Instant;

use serde_json::{Map, Value, json};

pub fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

pub fn rss_mb() -> f64 {
    memory_stats::memory_stats().map_or(0.0, |m| m.physical_mem as f64 / 1048576.0)
}

/// `{n, p50, p95, max}` of samples in ms.
pub fn stats(mut v: Vec<f64>) -> Value {
    if v.is_empty() {
        return json!({ "n": 0 });
    }
    v.sort_by(f64::total_cmp);
    let at = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
    json!({ "n": v.len(), "p50": at(0.5), "p95": at(0.95), "max": at(1.0) })
}

/// Times `f` `n` times.
pub fn repeat(n: usize, mut f: impl FnMut(usize)) -> Value {
    stats(
        (0..n)
            .map(|i| {
                let t = Instant::now();
                f(i);
                ms(t)
            })
            .collect(),
    )
}

/// What a scenario reports: timed metrics (gated against a baseline) and free-form info.
#[derive(Default)]
pub struct Report {
    pub metrics: Map<String, Value>,
    pub info: Map<String, Value>,
}

impl Report {
    /// A timed metric. `target` is the brief's limit for its p95, `gate` our tighter one.
    pub fn metric(&mut self, name: &str, mut stats: Value, target: Option<f64>, gate: Option<f64>) {
        if let Some(o) = stats.as_object_mut() {
            if let Some(t) = target {
                o.insert("target_ms".into(), t.into());
            }
            if let Some(g) = gate {
                o.insert("gate_ms".into(), g.into());
            }
        }
        eprintln!("  {name:<44} {}", summary(&stats));
        self.metrics.insert(name.into(), stats);
    }

    pub fn once(&mut self, name: &str, ms: f64, target: Option<f64>, gate: Option<f64>) {
        self.metric(name, stats(vec![ms]), target, gate);
    }

    pub fn info(&mut self, name: &str, v: impl Into<Value>) {
        self.info.insert(name.into(), v.into());
    }

    pub fn into_json(self) -> Value {
        json!({ "metrics": self.metrics, "info": self.info })
    }
}

fn summary(s: &Value) -> String {
    let f = |k: &str| s.get(k).and_then(Value::as_f64);
    let mut out = match (f("p50"), f("p95"), f("n")) {
        (Some(a), Some(b), Some(n)) if n > 1.0 => format!("p50 {a:>9.3}  p95 {b:>9.3} ms  (n={n})"),
        (Some(a), _, _) => format!("{a:>9.3} ms"),
        _ => String::new(),
    };
    for (key, label) in [("gate_ms", "gate"), ("target_ms", "target")] {
        if let (Some(limit), Some(p95)) = (f(key), f("p95")) {
            out += &format!("  [{label} {limit} ms: {}]", if p95 <= limit { "ok" } else { "MISS" });
        }
    }
    out
}

pub trait Scenario {
    fn name(&self) -> &'static str;
    fn describe(&self) -> &'static str;
    fn run(&self, r: &mut Report);
}

pub fn host() -> Value {
    let cpu = std::process::Command::new("reg")
        .args(["query", r"HKLM\HARDWARE\DESCRIPTION\System\CentralProcessor\0", "/v", "ProcessorNameString"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).split("REG_SZ").nth(1).map(|s| s.trim().to_string()))
        .unwrap_or_else(|| std::env::var("PROCESSOR_IDENTIFIER").unwrap_or_default());
    let git = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    json!({
        "cpu": cpu,
        "logical_cpus": std::thread::available_parallelism().map_or(0, |n| n.get()),
        "os": format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        "git": git,
        // `gpu` is added by main (render::gpu_info).
    })
}

/// Merges baseline runs, keeping the slowest p95 of each metric.
pub fn slowest(runs: Vec<Value>) -> Value {
    let mut it = runs.into_iter();
    let mut base = it.next().unwrap_or_default();
    for run in it {
        let Some(scenarios) = run.get("scenarios").and_then(Value::as_object) else { continue };
        for (sname, s) in scenarios {
            for (mname, m) in s.get("metrics").and_then(Value::as_object).into_iter().flatten() {
                let ptr = format!("/scenarios/{sname}/metrics/{mname}/p95");
                if let (Some(new), Some(slot)) = (m.get("p95").and_then(Value::as_f64), base.pointer_mut(&ptr))
                    && slot.as_f64().is_some_and(|old| new > old)
                {
                    *slot = new.into();
                }
            }
        }
    }
    base
}

/// Compares p95s against a baseline run. A metric regresses when it is more than `threshold`
/// percent slower and at least `floor_ms` slower (sub-floor jitter is noise). Returns the
/// regressions as text lines.
pub fn compare(base: &Value, run: &Value, threshold: f64, floor_ms: f64) -> Vec<String> {
    let mut bad = Vec::new();
    let Some(scenarios) = run.get("scenarios").and_then(Value::as_object) else { return bad };
    eprintln!("\n{:<20} {:<44} {:>10} {:>10} {:>8}", "scenario", "metric (p95 ms)", "baseline", "now", "change");
    for (sname, s) in scenarios {
        let Some(metrics) = s.get("metrics").and_then(Value::as_object) else { continue };
        for (mname, m) in metrics {
            let now = m.get("p95").and_then(Value::as_f64);
            let was = base.pointer(&format!("/scenarios/{sname}/metrics/{mname}/p95")).and_then(Value::as_f64);
            let (Some(now), Some(was)) = (now, was) else { continue };
            let change = if was > 0.0 { (now / was - 1.0) * 100.0 } else { 0.0 };
            let regressed = change > threshold && now - was >= floor_ms;
            eprintln!("{sname:<20} {mname:<44} {was:>10.3} {now:>10.3} {change:>+7.1}%{}", if regressed { "  REGRESSION" } else { "" });
            if regressed {
                bad.push(format!("{sname}/{mname}: p95 {was:.3} -> {now:.3} ms ({change:+.1}%)"));
            }
        }
    }
    bad
}
