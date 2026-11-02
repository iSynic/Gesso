//! Shared spike harness: a deterministic synthetic design document, R-tree culling with
//! simple level-of-detail, a scripted pan/zoom/edit scenario, and result reporting.
//!
//! Everything here is renderer-agnostic so the renderer spikes measure only the renderer.

pub mod scene;
pub mod scenario;
pub mod stats;
pub mod text;

pub use scene::*;
pub use scenario::*;
pub use stats::*;

/// Small deterministic PRNG (xorshift64*) so scenes are identical across runs and crates.
#[derive(Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    pub fn f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    pub fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.f64()
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
    pub fn chance(&mut self, p: f64) -> bool {
        self.f64() < p
    }
}

/// Parse `--key value` style flags without pulling in a CLI crate.
pub struct Args(Vec<String>);

impl Args {
    pub fn from_env() -> Self {
        Self(std::env::args().skip(1).collect())
    }
    pub fn value(&self, key: &str) -> Option<&str> {
        let i = self.0.iter().position(|a| a == key)?;
        self.0.get(i + 1).map(String::as_str)
    }
    pub fn flag(&self, key: &str) -> bool {
        self.0.iter().any(|a| a == key)
    }
    pub fn usize_or(&self, key: &str, default: usize) -> usize {
        self.value(key).and_then(|v| v.parse().ok()).unwrap_or(default)
    }
}
