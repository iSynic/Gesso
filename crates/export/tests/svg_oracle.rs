//! SVG export checked by an independent renderer: every render fixture is exported to SVG,
//! rendered by resvg, and compared with our own CPU render of the same display lists.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "../../render-cpu/tests/common/mod.rs"]
mod common;

use std::path::Path;

use export::raster;

fn resvg_render(svg: &str, w: u32, h: u32) -> Vec<u8> {
    let tree = resvg::usvg::Tree::from_str(svg, &resvg::usvg::Options::default()).unwrap();
    let mut pm = resvg::tiny_skia::Pixmap::new(w, h).unwrap();
    resvg::render(&tree, resvg::tiny_skia::Transform::default(), &mut pm.as_mut());
    pm.data().to_vec()
}

#[test]
fn svg_renders_like_the_display_list() {
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/svg-oracle");
    std::fs::create_dir_all(&out).unwrap();
    let mut report = Vec::new();
    let mut bad = Vec::new();
    for name in common::names() {
        let f = common::load(&name);
        let page = f.doc.key(f.page).unwrap();
        let ids: Vec<_> = f.doc.node(page).unwrap().children.iter().map(|k| f.doc.node(*k).unwrap().id).collect();
        let content = export::collect(&f.doc, &ids, text::FontDb::bundled_only(), &common::assets()).unwrap();
        let scale = 2.0;
        let ours = raster::render(&content, scale).unwrap();
        let (w, h) = (ours.width() as u32, ours.height() as u32);
        let svg = export::svg::write(&content, scale).unwrap();
        std::fs::write(out.join(format!("{name}.svg")), &svg).unwrap();
        let theirs = resvg_render(&svg, w, h);
        let d = common::diff(ours.data_as_u8_slice(), &theirs, 24);
        let share = d.over as f64 / (w * h) as f64;
        report.push(format!("{name:28} {w}x{h} mean {:.3} p99.9 {} max {} over {:.4}%", d.mean, d.p999, d.max, share * 100.0));
        // Edges antialias differently; image sampling differs most (images_fits ≈ 1.1 mean).
        if share > 0.025 || d.mean > 1.5 {
            bad.push(name.clone());
        }
        if share > 0.005 || d.mean > 1.0 {
            common::write_png(&out.join(format!("{name}-ours.png")), w, h, &raster::rgba(&ours));
            common::write_png(&out.join(format!("{name}-diff.png")), w, h, &d.image);
            common::write_png(&out.join(format!("{name}-resvg.png")), w, h, &theirs);
        }
    }
    println!("{}", report.join("\n"));
    assert!(bad.is_empty(), "SVG differs from the display list: {bad:?} (see target/svg-oracle)");
}
