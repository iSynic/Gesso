//! Pixel probes against values worked out by hand: stroke edges for each alignment, corner
//! radii, gradient end and middle colours, every blend mode against the W3C Compositing and
//! Blending formulas, shadow offsets and symmetric blur falloff.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;

use common::px;
use doc::*;
use kurbo::Affine;
use render::{NoAssets, Scene};
use serde_json::{Value, json};
use text::FontDb;

/// Renders `nodes` on a white page at 1:1, `w × h` pixels from the origin.
fn render(nodes: Vec<Value>, w: u32, h: u32) -> Vec<u8> {
    let mut s = Store::default().with_ids(IdGen::seeded(3));
    let page = s
        .transact(TxMeta::user("probe"), |tx| {
            let page = tx.insert_json(None, None, json!({"type": "page", "background": "#ffffff"}))?;
            for n in nodes {
                tx.insert_json(Some(page), None, n)?;
            }
            Ok(page)
        })
        .unwrap()
        .0;
    let doc = s.into_doc();
    let mut scene = Scene::new(&doc, page, FontDb::bundled_only(), Arc::new(NoAssets));
    let frame = scene.frame(&doc, Affine::IDENTITY, w, h, None);
    common::render_frame(&frame, 0)
}

fn near(a: [u8; 4], b: [u8; 4], tol: u8) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= tol)
}

const WHITE: [u8; 4] = [255, 255, 255, 255];
const BLACK: [u8; 4] = [0, 0, 0, 255];

#[test]
fn stroke_alignment_edges() {
    // A 40×40 square at (10, 10), no fill, 4 px black stroke. Probe row 30, columns 0..60.
    for (align, inside_from, inside_to) in [("inside", 10, 14), ("center", 8, 12), ("outside", 6, 10)] {
        let img = render(vec![json!({"type": "rectangle", "x": 10, "y": 10, "width": 40, "height": 40,
            "strokes": [{"paint": {"type": "solid", "color": "#000000"}, "weight": 4, "align": align}]})], 60, 60);
        for x in 0..30u32 {
            let p = px(&img, 60, x, 30);
            let expect = if (inside_from..inside_to).contains(&x) { BLACK } else { WHITE };
            assert!(near(p, expect, 2), "{align}: x {x} is {p:?}, expected {expect:?}");
        }
    }
}

#[test]
fn corner_radius_and_circle() {
    let img = render(vec![json!({"type": "rectangle", "x": 0, "y": 0, "width": 40, "height": 40, "radius": 20, "fills": [{"type": "solid", "color": "#000000"}]})], 40, 40);
    assert!(near(px(&img, 40, 1, 1), WHITE, 0), "corner outside the radius");
    assert!(near(px(&img, 40, 20, 20), BLACK, 0));
    // A point just inside the circle at 45°: r = 20, (20 − 13, 20 − 13) is 18.4 from the centre.
    assert!(near(px(&img, 40, 7, 7), BLACK, 40));
    // Tangent points are exactly on the edges.
    assert!(near(px(&img, 40, 20, 0), BLACK, 30) && near(px(&img, 40, 0, 20), BLACK, 30));
}

#[test]
fn linear_gradient_ends_and_middle_in_srgb() {
    let img = render(
        vec![json!({"type": "rectangle", "width": 101, "height": 10, "fills": [{"type": "linearGradient", "start": [0, 0.5], "end": [1, 0.5],
        "stops": [{"offset": 0, "color": "#ff0000"}, {"offset": 1, "color": "#0000ff"}]}]})],
        101,
        10,
    );
    assert!(near(px(&img, 101, 0, 5), [252, 0, 2, 255], 3), "{:?}", px(&img, 101, 0, 5));
    assert!(near(px(&img, 101, 100, 5), [2, 0, 252, 255], 3), "{:?}", px(&img, 101, 100, 5));
    // sRGB interpolation: the middle is (128, 0, 128), not the linear-light (188, 0, 188).
    assert!(near(px(&img, 101, 50, 5), [128, 0, 128, 255], 3), "{:?}", px(&img, 101, 50, 5));
}

// ---- W3C Compositing and Blending Level 1, on colours in 0..1 ---------------------------------

fn sep(mode: &str, cb: f64, cs: f64) -> f64 {
    let d = |cb: f64| if cb <= 0.25 { ((16.0 * cb - 12.0) * cb + 4.0) * cb } else { cb.sqrt() };
    let hard = |cb: f64, cs: f64| if cs <= 0.5 { cb * 2.0 * cs } else { let s2 = 2.0 * cs - 1.0; cb + s2 - cb * s2 };
    match mode {
        "normal" => cs,
        "multiply" => cb * cs,
        "screen" => cb + cs - cb * cs,
        "overlay" => hard(cs, cb),
        "darken" => cb.min(cs),
        "lighten" => cb.max(cs),
        "colorDodge" => {
            if cb == 0.0 {
                0.0
            } else if cs >= 1.0 {
                1.0
            } else {
                (cb / (1.0 - cs)).min(1.0)
            }
        }
        "colorBurn" => {
            if cb >= 1.0 {
                1.0
            } else if cs <= 0.0 {
                0.0
            } else {
                1.0 - ((1.0 - cb) / cs).min(1.0)
            }
        }
        "hardLight" => hard(cb, cs),
        "softLight" => {
            if cs <= 0.5 {
                cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb)
            } else {
                cb + (2.0 * cs - 1.0) * (d(cb) - cb)
            }
        }
        "difference" => (cb - cs).abs(),
        "exclusion" => cb + cs - 2.0 * cb * cs,
        _ => unreachable!(),
    }
}

fn lum(c: [f64; 3]) -> f64 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

fn clip_color(c: [f64; 3]) -> [f64; 3] {
    let l = lum(c);
    let n = c.iter().copied().fold(f64::INFINITY, f64::min);
    let x = c.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let mut c = c;
    if n < 0.0 {
        c = c.map(|v| l + (v - l) * l / (l - n));
    }
    if x > 1.0 {
        c = c.map(|v| l + (v - l) * (1.0 - l) / (x - l));
    }
    c
}

fn set_lum(c: [f64; 3], l: f64) -> [f64; 3] {
    let d = l - lum(c);
    clip_color(c.map(|v| v + d))
}

fn sat(c: [f64; 3]) -> f64 {
    c.iter().copied().fold(f64::NEG_INFINITY, f64::max) - c.iter().copied().fold(f64::INFINITY, f64::min)
}

fn set_sat(c: [f64; 3], s: f64) -> [f64; 3] {
    let mut idx = [0usize, 1, 2];
    idx.sort_by(|a, b| c[*a].total_cmp(&c[*b]));
    let (mn, md, mx) = (idx[0], idx[1], idx[2]);
    let mut out = [0.0; 3];
    if c[mx] > c[mn] {
        out[md] = (c[md] - c[mn]) * s / (c[mx] - c[mn]);
        out[mx] = s;
    }
    out
}

fn blend(mode: &str, cb: [f64; 3], cs: [f64; 3]) -> [f64; 3] {
    match mode {
        "hue" => set_lum(set_sat(cs, sat(cb)), lum(cb)),
        "saturation" => set_lum(set_sat(cb, sat(cs)), lum(cb)),
        "color" => set_lum(cs, lum(cb)),
        "luminosity" => set_lum(cb, lum(cs)),
        m => [sep(m, cb[0], cs[0]), sep(m, cb[1], cs[1]), sep(m, cb[2], cs[2])],
    }
}

#[test]
fn blend_modes_match_the_w3c_formulas() {
    let (b, s) = ([0.2, 0.6, 0.9], [0.8, 0.4, 0.3]);
    let hex = |c: [f64; 3]| format!("#{:02x}{:02x}{:02x}", (c[0] * 255.0).round() as u8, (c[1] * 255.0).round() as u8, (c[2] * 255.0).round() as u8);
    let q = |c: [f64; 3]| c.map(|v| (v * 255.0).round() / 255.0);
    let (b, s) = (q(b), q(s));
    let modes = [
        "normal", "multiply", "screen", "overlay", "darken", "lighten", "colorDodge", "colorBurn", "hardLight", "softLight", "difference", "exclusion", "hue", "saturation",
        "color", "luminosity",
    ];
    for m in modes {
        let img = render(
            vec![
                json!({"type": "rectangle", "width": 20, "height": 20, "fills": [{"type": "solid", "color": hex(b)}]}),
                json!({"type": "rectangle", "width": 20, "height": 20, "blend": m, "fills": [{"type": "solid", "color": hex(s)}]}),
            ],
            20,
            20,
        );
        let e = blend(m, b, s).map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
        let got = px(&img, 20, 10, 10);
        assert!(near(got, [e[0], e[1], e[2], 255], 3), "{m}: got {got:?}, expected {e:?}");
    }
}

/// Darkness-weighted centroid (red channel) of the part of an image right of `x0`.
fn centroid(img: &[u8], w: u32, h: u32, x0: u32) -> (f64, f64) {
    let (mut sx, mut sy, mut sw) = (0.0, 0.0, 0.0);
    for y in 0..h {
        for x in x0..w {
            let d = 255.0 - px(img, w, x, y)[0] as f64;
            sx += d * (x as f64 + 0.5);
            sy += d * (y as f64 + 0.5);
            sw += d;
        }
    }
    (sx / sw, sy / sw)
}

#[test]
fn shadows_land_at_their_offset() {
    // A red shape (no darkness in the red channel) with its shadow moved clear of it: the
    // shadow's centroid is the shape's centre plus the offset, for the fast path (rectangle) and
    // the offscreen path (ellipse).
    for kind in ["rectangle", "ellipse"] {
        let img = render(
            vec![json!({"type": kind, "x": 20, "y": 20, "width": 20, "height": 20, "fills": [{"type": "solid", "color": "#ff0000"}],
            "effects": [{"type": "dropShadow", "color": "#000000", "offset": [40, 30], "blur": 4}]})],
            120,
            120,
        );
        let (cx, cy) = centroid(&img, 120, 120, 45);
        assert!((cx - 70.0).abs() < 0.3 && (cy - 60.0).abs() < 0.3, "{kind}: centroid ({cx:.2}, {cy:.2}), expected (70, 60)");
    }
    // A transparent fill casts no shadow on either path.
    for kind in ["rectangle", "ellipse"] {
        let img = render(
            vec![json!({"type": kind, "x": 20, "y": 20, "width": 20, "height": 20, "fills": [{"type": "solid", "color": "#ff000000"}],
            "effects": [{"type": "dropShadow", "color": "#000000", "offset": [4, 4], "blur": 4}]})],
            60,
            60,
        );
        assert!(img.as_chunks::<4>().0.iter().all(|p| *p == WHITE), "{kind}: a transparent shape cast a shadow");
    }
}

#[test]
fn layer_blur_falls_off_symmetrically() {
    let img = render(
        vec![json!({"type": "rectangle", "x": 40, "y": 20, "width": 40, "height": 40, "fills": [{"type": "solid", "color": "#000000"}],
        "effects": [{"type": "layerBlur", "radius": 8}]})],
        120,
        80,
    );
    // σ = 4: the edge at x = 40 is 50 % grey, and the profile mirrors around the centre x = 60.
    let at = |x: u32| px(&img, 120, x, 40)[0] as i32;
    assert!((at(40) - 128).abs() <= 24 && (at(39) - 128).abs() <= 24, "edge {} {}", at(39), at(40));
    for d in 0..20u32 {
        assert!((at(60 - 1 - d) - at(60 + d)).abs() <= 2, "asymmetric at ±{d}: {} vs {}", at(60 - 1 - d), at(60 + d));
    }
    assert!(at(10) > 250 && at(60) < 5);
}
