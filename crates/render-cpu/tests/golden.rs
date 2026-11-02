//! Golden images: every fixture in `tests/fixtures/` rendered on vello_cpu (bundled fonts only)
//! must match `tests/golden/<name>.png`: each channel within 2/255, or at most 0.1 % of pixels
//! beyond that. Failures write `actual` and `diff` images to `target/golden-failures/`.
//! `UPDATE_GOLDENS=1` re-blesses (review the images before keeping them); `GOLDEN=name` filters.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::*;

const TOL: u8 = 2;
const MAX_OVER: f64 = 0.001;

#[test]
fn goldens() {
    let update = std::env::var("UPDATE_GOLDENS").is_ok_and(|v| v == "1");
    let only = std::env::var("GOLDEN").ok();
    let fail_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/golden-failures");
    let mut failures = Vec::new();
    let mut checked = 0;
    for name in names() {
        if only.as_ref().is_some_and(|o| !name.contains(o.as_str())) {
            continue;
        }
        let f = load(&name);
        let actual = render_cpu(&f, 4);
        let path = golden_dir().join(format!("{name}.png"));
        if update {
            write_png(&path, f.width, f.height, &actual);
            continue;
        }
        checked += 1;
        let Some((w, h, expected)) = read_png(&path) else {
            failures.push(format!("{name}: no golden (run with UPDATE_GOLDENS=1)"));
            write_png(&fail_dir.join(format!("{name}.actual.png")), f.width, f.height, &actual);
            continue;
        };
        if (w, h) != (f.width, f.height) {
            failures.push(format!("{name}: size {}x{} vs golden {w}x{h}", f.width, f.height));
            continue;
        }
        let d = diff(&actual, &expected, TOL);
        let allowed = (MAX_OVER * (w * h) as f64).floor() as usize;
        if d.over > allowed {
            write_png(&fail_dir.join(format!("{name}.actual.png")), w, h, &actual);
            write_png(&fail_dir.join(format!("{name}.diff.png")), w, h, &d.image);
            failures.push(format!("{name}: {} pixels over {TOL}/255 (allowed {allowed}), max {}", d.over, d.max));
        }
    }
    assert!(failures.is_empty(), "golden mismatches ({} of {checked}):\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn threads_do_not_change_pixels() {
    for name in ["shapes_basic", "text_spans", "effects_shadows", "blend_modes_a"] {
        let f = load(name);
        assert!(render_cpu(&f, 0) == render_cpu(&f, 8), "{name}: 1 thread vs 8 threads differ");
    }
}
