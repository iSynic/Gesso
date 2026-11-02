//! SVG import, checked by an independent renderer: hand-written SVGs (tests/svg) imported and
//! drawn by us must look like resvg's rendering of the same file; and every render fixture,
//! exported to SVG and imported back, must look like the original.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "../../render-cpu/tests/common/mod.rs"]
mod common;

use std::path::Path;
use std::sync::Arc;

use doc::{IdGen, Store, TxMeta};
use export::import::{Imported, import_svg};
use kurbo::Rect;
use layout::LayoutDeriver;
use render::{MemoryAssets, Scene};
use serde_json::json;
use text::{FontDb, TextEngine};

const SCALE: f64 = 2.0;

/// Our rendering of an import, on white, `w × h` units at `scale`.
fn render_import(imp: &Imported, w: f64, h: f64, scale: f64) -> Vec<u8> {
    let mut s = Store::default().with_ids(IdGen::seeded(5));
    s.set_deriver(Box::new(LayoutDeriver::new(TextEngine::new(FontDb::bundled_only()))));
    let mut assets = MemoryAssets::default();
    let page = s
        .transact(TxMeta::user("import"), |tx| {
            for bytes in &imp.images {
                let (id, meta) = format::identify_asset(bytes).unwrap();
                assets.0.insert(id.clone(), bytes.clone());
                tx.put_asset(id, meta)?;
            }
            let page = tx.insert_json(None, None, json!({"type": "page", "background": "#ffffff"}))?;
            tx.insert_json(Some(page), None, imp.node.clone())?;
            Ok(page)
        })
        .unwrap()
        .0;
    let doc = s.into_doc();
    let mut scene = Scene::new(&doc, page, FontDb::bundled_only(), Arc::new(assets));
    let px = render_cpu::render_rect(&mut scene, &doc, Rect::new(0.0, 0.0, w, h), scale, &mut render_cpu::CpuRenderer::default()).unwrap();
    render_cpu::rgba(&px)
}

/// resvg's rendering on white, unpremultiplied.
fn resvg_render(svg: &str, w: u32, h: u32) -> Vec<u8> {
    let mut opt = resvg::usvg::Options { font_family: "Inter Variable".into(), ..Default::default() };
    let db = Arc::make_mut(&mut opt.fontdb);
    db.load_font_data(text::INTER_TTF.to_vec());
    db.set_serif_family("Inter Variable");
    let tree = resvg::usvg::Tree::from_str(svg, &opt).unwrap();
    let mut pm = resvg::tiny_skia::Pixmap::new(w, h).unwrap();
    pm.fill(resvg::tiny_skia::Color::WHITE);
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(SCALE as f32, SCALE as f32), &mut pm.as_mut());
    pm.data().to_vec()
}

/// Straight RGBA over white.
fn on_white(rgba: &[u8]) -> Vec<u8> {
    rgba.as_chunks::<4>().0.iter()
        .flat_map(|p| {
            let a = u32::from(p[3]);
            let c = |v: u8| ((u32::from(v) * a + 255 * (255 - a) + 127) / 255) as u8;
            [c(p[0]), c(p[1]), c(p[2]), 255]
        })
        .collect()
}

fn out_dir() -> std::path::PathBuf {
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/svg-import");
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn hand_written_svgs_import_like_resvg_draws_them() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/svg");
    let out = out_dir();
    let mut files: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "svg")).collect();
    files.sort();
    assert!(files.len() >= 8);
    let mut bad = Vec::new();
    for f in files {
        let name = f.file_stem().unwrap().to_string_lossy().into_owned();
        let src = std::fs::read_to_string(&f).unwrap();
        let imp = import_svg(&src, &name, &[]).unwrap_or_else(|e| panic!("{name}: {e}"));
        let (w, h) = (imp.node["width"].as_f64().unwrap(), imp.node["height"].as_f64().unwrap());
        let (pw, ph) = ((w * SCALE) as u32, (h * SCALE) as u32);
        let ours = on_white(&render_import(&imp, w, h, SCALE));
        let theirs = resvg_render(&src, pw, ph);
        let d = common::diff(&ours, &theirs, 32);
        let share = d.over as f64 / f64::from(pw * ph);
        println!("{name:12} mean {:.3} p99.9 {:3} over {:.3}% warnings {:?}", d.mean, d.p999, share * 100.0, imp.warnings);
        if share > 0.01 || d.mean > 1.5 {
            bad.push(name.clone());
            common::write_png(&out.join(format!("{name}-ours.png")), pw, ph, &ours);
            common::write_png(&out.join(format!("{name}-resvg.png")), pw, ph, &theirs);
        }
    }
    assert!(bad.is_empty(), "imports differ from resvg: {bad:?} (see target/svg-import)");
}

#[test]
fn exported_svg_imports_back_like_the_original() {
    let out = out_dir();
    let mut bad = Vec::new();
    for name in common::names() {
        let f = common::load(&name);
        let page = f.doc.key(f.page).unwrap();
        let ids: Vec<_> = f.doc.node(page).unwrap().children.iter().map(|k| f.doc.node(*k).unwrap().id).collect();
        let content = export::collect(&f.doc, &ids, FontDb::bundled_only(), &common::assets()).unwrap();
        let svg = export::svg::write(&content, SCALE).unwrap();
        let imp = import_svg(&svg, &name, &[]).unwrap();
        let ours = on_white(&raster::rgba(&export::raster::render(&content, SCALE).unwrap()));
        // Exported at SCALE, so the import is SCALE times larger: draw it at 1×.
        let back = on_white(&render_import(&imp, content.width * SCALE, content.height * SCALE, 1.0));
        let (pw, ph) = content.pixels(SCALE);
        let d = common::diff(&ours, &back, 32);
        let share = d.over as f64 / f64::from(pw * ph);
        println!("{name:28} mean {:.3} p99.9 {:3} over {:.3}% warnings {:?}", d.mean, d.p999, share * 100.0, imp.warnings);
        // Effects are dropped on import (SVG filters), so only unaffected fixtures must match.
        let lossy = imp.warnings.iter().any(|w| w.contains("filters") || w.contains("mask") || w.contains("pattern") || w.contains("rotated") || w.contains("left out"));
        if !lossy && (share > 0.01 || d.mean > 1.5) {
            bad.push(name.clone());
            common::write_png(&out.join(format!("rt-{name}-orig.png")), pw, ph, &ours);
            common::write_png(&out.join(format!("rt-{name}-back.png")), pw, ph, &back);
        }
    }
    assert!(bad.is_empty(), "round trip differs: {bad:?} (see target/svg-import)");
}

use export::raster;

