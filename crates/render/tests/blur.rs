//! The reference blur against the analytic Gaussian: a hard edge blurred with σ has the error
//! function's profile; octave downsampling keeps large σ close to it.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use render::blur::{Pixels, blur, drop_shadow};

fn erf(x: f64) -> f64 {
    // Abramowitz–Stegun 7.1.26 (|error| < 1.5e-7).
    let t = 1.0 / (1.0 + 0.3275911 * x.abs());
    let y = 1.0 - (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t + 0.254829592) * t * (-x * x).exp();
    if x >= 0.0 { y } else { -y }
}

/// A 1-pixel-high strip, opaque from column `edge` on; returns alpha per column after blur.
fn edge_profile(sigma: f64, w: u32, edge: u32) -> Vec<f64> {
    let h = 400;
    let mut p = Pixels::new(0, 0, w, h);
    for y in 0..h {
        for x in edge..w {
            let i = ((y * w + x) * 4) as usize;
            p.data[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
        }
    }
    blur(&mut p, sigma);
    (0..w).map(|x| p.data[((h / 2 * w + x) * 4 + 3) as usize] as f64 / 255.0).collect()
}

#[test]
fn hard_edge_follows_the_error_function() {
    for sigma in [1.0, 2.0, 4.0, 6.0, 12.0, 24.0] {
        let w = 400;
        let edge = 200;
        let prof = edge_profile(sigma, w, edge);
        let mut worst = 0.0f64;
        for x in (edge as i32 - (3.0 * sigma) as i32)..(edge as i32 + (3.0 * sigma) as i32) {
            // Pixel centres at x + 0.5; the edge is at `edge`.
            let d = x as f64 + 0.5 - edge as f64;
            let expect = 0.5 * (1.0 + erf(d / (sigma * std::f64::consts::SQRT_2)));
            worst = worst.max((prof[x as usize] - expect).abs());
        }
        let limit = if sigma > 8.0 { 0.03 } else { 0.01 };
        assert!(worst < limit, "σ {sigma}: worst error {worst:.4}");
    }
}

#[test]
fn shadow_moves_by_offset_and_spread_grows_it() {
    let mut p = Pixels::new(0, 0, 100, 100);
    for y in 40..60 {
        for x in 40..60 {
            let i = ((y * 100 + x) * 4) as usize;
            p.data[i..i + 4].copy_from_slice(&[255, 0, 0, 255]);
        }
    }
    let s = drop_shadow(&p, [0.0, 0.0, 0.0, 1.0], 10, 5, 0.0, 0);
    let a = |x: u32, y: u32| s.data[((y * 100 + x) * 4 + 3) as usize];
    assert_eq!((a(50, 45), a(69, 64), a(49, 50), a(55, 65)), (255, 255, 0, 0));
    let s = drop_shadow(&p, [0.0, 0.0, 0.0, 1.0], 0, 0, 0.0, 3);
    let a = |x: u32, y: u32| s.data[((y * 100 + x) * 4 + 3) as usize];
    assert_eq!((a(37, 50), a(36, 50), a(62, 62), a(63, 63)), (255, 0, 255, 0));
}
