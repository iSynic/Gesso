//! GPU parity: every golden fixture rendered on Vello must match vello_cpu: mean channel
//! difference ≤ 1.5/255, and at most 1 % of pixels off by more than 24/255. The two rasterisers
//! differ in anti-aliased coverage along curved edges and Vello's GPU gradients use a 512-texel
//! ramp (about 1/255 off across a gradient); nothing else should differ. The blur and shadow
//! fixtures check our compute passes against `render::blur`.
//!
//! Needs a GPU: runs only with `GPU_TESTS=1`. Failures write `cpu`, `gpu` and `diff` images to
//! `target/parity-failures/`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "../../render-cpu/tests/common/mod.rs"]
mod common;

use common::*;
use render::Renderer;
use render_vello::VelloRenderer;

const MAX_MEAN: f64 = 1.5;
/// Pixels differing by more than this count as "off" ...
const OFF: u8 = 24;
/// ... and at most this share of them may be.
const MAX_OFF_SHARE: f64 = 0.01;

fn gpu_tests() -> bool {
    let on = std::env::var("GPU_TESTS").is_ok_and(|v| v == "1");
    if !on {
        eprintln!("skipped: set GPU_TESTS=1 to run GPU tests");
    }
    on
}

fn render_gpu(r: &mut VelloRenderer, f: &Fixture) -> Vec<u8> {
    let mut sc = scene(f);
    let fr = frame(f, &mut sc);
    let mut t = r.target(fr.width, fr.height);
    r.render(&fr, &mut t).unwrap();
    r.read(&t).unwrap()
}

#[test]
fn gpu_matches_cpu() {
    if !gpu_tests() {
        return;
    }
    let mut r = VelloRenderer::headless().unwrap();
    if let Some(a) = r.adapter_info() {
        eprintln!("adapter: {} ({:?}, {})", a.name, a.backend, a.driver_info);
    }
    let only = std::env::var("GOLDEN").ok();
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/parity-failures");
    let mut failures = Vec::new();
    eprintln!("{:<28} {:>6} {:>6} {:>5} {:>8}", "fixture", "mean", "p99.9", "max", ">24 %");
    for name in names() {
        if only.as_ref().is_some_and(|o| !name.contains(o.as_str())) {
            continue;
        }
        let f = load(&name);
        let cpu = render_cpu(&f, 4);
        let gpu = render_gpu(&mut r, &f);
        let d = diff(&gpu, &cpu, OFF);
        let share = d.over as f64 / (f.width * f.height) as f64;
        eprintln!("{name:<28} {:>6.3} {:>6} {:>5} {:>8.3}", d.mean, d.p999, d.max, 100.0 * share);
        if d.mean > MAX_MEAN || share > MAX_OFF_SHARE {
            write_png(&dir.join(format!("{name}.cpu.png")), f.width, f.height, &cpu);
            write_png(&dir.join(format!("{name}.gpu.png")), f.width, f.height, &gpu);
            write_png(&dir.join(format!("{name}.diff.png")), f.width, f.height, &d.image);
            failures.push(format!("{name}: mean {:.3}, {:.3} % over {OFF}/255, max {}", d.mean, 100.0 * share, d.max));
        }
    }
    assert!(failures.is_empty(), "GPU/CPU parity failures:\n{}", failures.join("\n"));
}

#[test]
fn cached_runs_render_the_same_pixels() {
    if !gpu_tests() {
        return;
    }
    let mut r = VelloRenderer::headless().unwrap();
    for name in ["shapes_basic", "text_spans", "effects_shadows", "effects_background_blur"] {
        let f = load(name);
        let mut sc = scene(&f);
        let fr = frame(&f, &mut sc);
        let mut t = r.target(fr.width, fr.height);
        r.render(&fr, &mut t).unwrap();
        let first = r.read(&t).unwrap();
        assert!(r.stats.runs_encoded > 0, "{name}: nothing encoded");
        r.render(&fr, &mut t).unwrap();
        assert_eq!(r.stats.runs_encoded, 0, "{name}: second frame re-encoded");
        assert!(r.stats.runs_reused > 0);
        assert!(r.read(&t).unwrap() == first, "{name}: cached frame differs");
    }
}

/// Vello's atlas loses an image's pixels across a render that doesn't draw it; the renderer
/// re-uploads such images.
#[test]
fn images_survive_frames_without_them() {
    if !gpu_tests() {
        return;
    }
    let mut r = VelloRenderer::headless().unwrap();
    let (with, without) = (load("paint_stack_visibility"), load("shapes_basic"));
    let cpu = render_cpu(&with, 0);
    for i in 0..3 {
        let d = diff(&render_gpu(&mut r, &with), &cpu, OFF);
        assert!(d.mean <= MAX_MEAN, "pass {i}: mean {:.3}", d.mean);
        render_gpu(&mut r, &without);
    }
}

/// The patched Vello keeps its gradient-ramp texture between frames and re-uploads it only when
/// the ramp cache changes; frames with other gradients, or none, must not leave it stale.
#[test]
fn gradient_ramps_survive_frames() {
    if !gpu_tests() {
        return;
    }
    let mut r = VelloRenderer::headless().unwrap();
    let names = ["gradients_linear", "gradients_radial_angular", "shapes_basic"];
    let fixtures: Vec<_> = names.iter().map(|n| load(n)).collect();
    let cpu: Vec<_> = fixtures.iter().map(|f| render_cpu(f, 0)).collect();
    for i in [0, 0, 1, 0, 2, 1, 2, 2, 0, 1] {
        let d = diff(&render_gpu(&mut r, &fixtures[i]), &cpu[i], OFF);
        assert!(d.mean <= MAX_MEAN, "{}: mean {:.3}", names[i], d.mean);
    }
}
