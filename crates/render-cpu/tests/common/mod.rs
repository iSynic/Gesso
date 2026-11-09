//! Shared by the golden, probe and parity tests: fixture loading and image comparison.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use doc::model::{AssetId, AssetMeta};
use doc::*;
use kurbo::{Affine, Rect};
use layout::LayoutDeriver;
use render::{Frame, MemoryAssets, Scene};
use serde_json::Value;
use text::{FontDb, TextEngine};

/// Asset ids the fixtures can use: a 64×48 test pattern and a 16×16 checker.
pub const PATTERN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const CHECKER: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn encode(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
    let mut img = image::RgbaImage::new(w, h);
    for (x, y, p) in img.enumerate_pixels_mut() {
        *p = image::Rgba(f(x, y));
    }
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
    out
}

pub fn assets() -> MemoryAssets {
    let mut m = MemoryAssets::default();
    m.0.insert(
        AssetId(PATTERN.into()),
        encode(64, 48, |x, y| {
            let b = if (x / 8 + y / 8) % 2 == 0 { 220 } else { 60 };
            [(x * 4) as u8, (y * 5) as u8, b, 255]
        }),
    );
    m.0.insert(AssetId(CHECKER.into()), encode(16, 16, |x, y| if (x / 4 + y / 4) % 2 == 0 { [40, 40, 40, 255] } else { [240, 240, 240, 255] }));
    m
}

pub struct Fixture {
    pub name: String,
    pub doc: Document,
    pub page: NodeId,
    pub width: u32,
    pub height: u32,
    /// World → device.
    pub camera: Affine,
}

pub fn fixtures_dir() -> PathBuf {
    // Shared with render-vello's parity test, so not relative to the including crate.
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../render-cpu/tests/fixtures")
}

pub fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../render-cpu/tests/golden")
}

/// Loads `tests/fixtures/<name>.json`: `{size: [w, h], scale?, zoom?, background?, nodes: [...]}`.
/// `size` is in device pixels at scale 1; `zoom` shows a larger world area (level of detail).
/// Nodes go through the normal insert path and layout, with bundled fonts only.
pub fn load(name: &str) -> Fixture {
    let path = fixtures_dir().join(format!("{name}.json"));
    let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap_or_else(|e| panic!("{name}.json: {e}"));
    from_json(name, &v)
}

/// A fixture from its JSON, as [`load`] reads it from a file.
pub fn from_json(name: &str, v: &Value) -> Fixture {
    let size = v["size"].as_array().unwrap();
    let (w, h) = (size[0].as_f64().unwrap(), size[1].as_f64().unwrap());
    let scale = v.get("scale").and_then(Value::as_f64).unwrap_or(1.0);
    let zoom = v.get("zoom").and_then(Value::as_f64).unwrap_or(1.0);
    let mut s = Store::default().with_ids(IdGen::seeded(11));
    s.set_deriver(Box::new(LayoutDeriver::new(TextEngine::new(FontDb::bundled_only()))));
    let page = s
        .transact(TxMeta::user("fixture"), |tx| {
            for (id, (w, h)) in [(PATTERN, (64, 48)), (CHECKER, (16, 16))] {
                tx.put_asset(AssetId(id.into()), AssetMeta { mime: "image/png".into(), width: w, height: h })?;
            }
            let bg = v.get("background").cloned().unwrap_or(Value::String("#ffffff".into()));
            let page = tx.insert_json(None, None, serde_json::json!({"type": "page", "background": bg}))?;
            for n in v["nodes"].as_array().unwrap() {
                tx.insert_json(Some(page), None, n.clone())?;
            }
            Ok(page)
        })
        .unwrap_or_else(|e| panic!("fixture {name}: {e}"))
        .0;
    Fixture { name: name.into(), doc: s.into_doc(), page, width: (w * scale).round() as u32, height: (h * scale).round() as u32, camera: Affine::scale(scale * zoom) }
}

pub fn names() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(fixtures_dir())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| e.path().file_stem().map(|s| s.to_string_lossy().into_owned()))
        .collect();
    v.sort();
    v
}

pub fn scene(f: &Fixture) -> Scene {
    Scene::new(&f.doc, f.page, FontDb::bundled_only(), Arc::new(assets()))
}

pub fn frame(f: &Fixture, scene: &mut Scene) -> Frame {
    scene.frame(&f.doc, f.camera, f.width, f.height, None)
}

pub fn render_frame(fr: &Frame, threads: u16) -> Vec<u8> {
    let mut px = render_cpu::Target::new(fr.width as u16, fr.height as u16);
    render::Renderer::render(&mut render_cpu::CpuRenderer::new(threads), fr, &mut px).unwrap();
    render_cpu::rgba(&px)
}

/// Straight RGBA8 of the CPU render.
pub fn render_cpu(f: &Fixture, threads: u16) -> Vec<u8> {
    let mut sc = scene(f);
    let fr = frame(f, &mut sc);
    render_frame(&fr, threads)
}

pub fn read_png(path: &Path) -> Option<(u32, u32, Vec<u8>)> {
    let img = image::open(path).ok()?.into_rgba8();
    Some((img.width(), img.height(), img.into_raw()))
}

pub fn write_png(path: &Path, w: u32, h: u32, rgba: &[u8]) {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).unwrap();
    }
    image::save_buffer(path, rgba, w, h, image::ColorType::Rgba8).unwrap();
}

/// How two RGBA8 images differ.
pub struct Diff {
    /// Largest channel difference.
    pub max: u8,
    /// Pixels whose largest channel difference exceeds the tolerance.
    pub over: usize,
    /// Mean absolute channel difference.
    pub mean: f64,
    /// 99.9th percentile of per-pixel largest channel differences.
    pub p999: u8,
    /// A visualisation: differing pixels in red.
    pub image: Vec<u8>,
}

pub fn diff(a: &[u8], b: &[u8], tol: u8) -> Diff {
    let mut img = Vec::with_capacity(a.len());
    let (mut max, mut over, mut sum) = (0u8, 0usize, 0u64);
    let mut per = Vec::with_capacity(a.len() / 4);
    for (p, q) in a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0.iter()) {
        let d = (0..4).map(|i| p[i].abs_diff(q[i])).max().unwrap_or(0);
        sum += (0..4).map(|i| p[i].abs_diff(q[i]) as u64).sum::<u64>();
        max = max.max(d);
        if d > tol {
            over += 1;
        }
        per.push(d);
        let v = if d == 0 { 0 } else { 64u8.saturating_add(d.saturating_mul(4)) };
        img.extend_from_slice(&[v, 0, 0, 255]);
    }
    per.sort_unstable();
    let p999 = per.get(((per.len() as f64 - 1.0) * 0.999).round() as usize).copied().unwrap_or(0);
    Diff { max, over, mean: sum as f64 / a.len().max(1) as f64, p999, image: img }
}

/// The world rectangle a fixture shows.
pub fn view(f: &Fixture) -> Rect {
    f.camera.inverse().transform_rect_bbox(Rect::new(0.0, 0.0, f.width as f64, f.height as f64))
}

/// Straight RGBA of pixel (x, y).
pub fn px(img: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * w + x) * 4) as usize;
    [img[i], img[i + 1], img[i + 2], img[i + 3]]
}
