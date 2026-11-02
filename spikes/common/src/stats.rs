//! Frame-time statistics and JSON result files.

use crate::scenario::Phase;
use serde::Serialize;
use web_time::Instant;

#[derive(Clone, Copy, Debug, Serialize)]
pub struct FrameSample {
    pub phase: Phase,
    pub visible: u32,
    pub cull_ms: f64,
    /// CPU time building the renderer's command stream.
    pub encode_ms: f64,
    /// Submit -> GPU finished (includes GPU execution).
    pub gpu_ms: f64,
    pub total_ms: f64,
}

#[derive(Debug, Serialize)]
pub struct Percentiles {
    pub n: usize,
    pub mean: f64,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub max: f64,
}

pub fn percentiles(mut v: Vec<f64>) -> Percentiles {
    if v.is_empty() {
        return Percentiles { n: 0, mean: 0.0, p50: 0.0, p95: 0.0, p99: 0.0, max: 0.0 };
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let at = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
    Percentiles {
        n: v.len(),
        mean: v.iter().sum::<f64>() / v.len() as f64,
        p50: at(0.5),
        p95: at(0.95),
        p99: at(0.99),
        max: *v.last().unwrap(),
    }
}

#[derive(Debug, Serialize)]
pub struct PhaseSummary {
    pub phase: Phase,
    pub total: Percentiles,
    pub encode: Percentiles,
    pub gpu: Percentiles,
    pub cull: Percentiles,
    pub visible_mean: f64,
}

pub fn summarize(samples: &[FrameSample]) -> Vec<PhaseSummary> {
    [Phase::PanZoom1, Phase::Zoom, Phase::PanOverview, Phase::Edit]
        .into_iter()
        .map(|ph| {
            let s: Vec<&FrameSample> = samples.iter().filter(|f| f.phase == ph).collect();
            PhaseSummary {
                phase: ph,
                total: percentiles(s.iter().map(|f| f.total_ms).collect()),
                encode: percentiles(s.iter().map(|f| f.encode_ms).collect()),
                gpu: percentiles(s.iter().map(|f| f.gpu_ms).collect()),
                cull: percentiles(s.iter().map(|f| f.cull_ms).collect()),
                visible_mean: s.iter().map(|f| f.visible as f64).sum::<f64>() / s.len().max(1) as f64,
            }
        })
        .collect()
}

#[derive(Debug, Serialize)]
pub struct Host {
    pub os: String,
    pub cpu: String,
    pub logical_cpus: usize,
    pub gpu: String,
    pub gpu_backend: String,
    pub git: String,
    pub profile: &'static str,
}

impl Host {
    pub fn detect(gpu: String, gpu_backend: String) -> Self {
        let cpu = std::env::var("PROCESSOR_IDENTIFIER").unwrap_or_default();
        #[cfg(target_arch = "wasm32")]
        let git = String::new();
        #[cfg(not(target_arch = "wasm32"))]
        let git = std::process::Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        Host {
            os: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
            cpu,
            logical_cpus: std::thread::available_parallelism().map_or(0, |n| n.get()),
            gpu,
            gpu_backend,
            git,
            profile: if cfg!(debug_assertions) { "debug" } else { "release" },
        }
    }
}

pub fn rss_mb() -> f64 {
    #[cfg(not(target_arch = "wasm32"))]
    return memory_stats::memory_stats().map_or(0.0, |m| m.physical_mem as f64 / 1048576.0);
    #[cfg(target_arch = "wasm32")]
    return 0.0;
}

pub struct Timer(Instant);
impl Timer {
    pub fn start() -> Self {
        Self(Instant::now())
    }
    pub fn ms(&self) -> f64 {
        self.0.elapsed().as_secs_f64() * 1e3
    }
}

pub fn write_json(path: Option<&str>, value: &impl Serialize) {
    let s = serde_json::to_string_pretty(value).unwrap();
    if let Some(p) = path {
        if let Some(dir) = std::path::Path::new(p).parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        std::fs::write(p, &s).unwrap();
        eprintln!("wrote {p}");
    } else {
        println!("{s}");
    }
}

/// Print a compact table of phase summaries to stderr.
pub fn print_summary(name: &str, phases: &[PhaseSummary]) {
    eprintln!("== {name}");
    eprintln!("{:<12} {:>8} {:>8} {:>8} {:>8} {:>9} {:>9} {:>8}", "phase", "p50", "p95", "p99", "max", "enc p50", "gpu p50", "visible");
    for p in phases {
        eprintln!(
            "{:<12} {:>8.2} {:>8.2} {:>8.2} {:>8.2} {:>9.2} {:>9.2} {:>8.0}",
            format!("{:?}", p.phase),
            p.total.p50,
            p.total.p95,
            p.total.p99,
            p.total.max,
            p.encode.p50,
            p.gpu.p50,
            p.visible_mean
        );
    }
}
