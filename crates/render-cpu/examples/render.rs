//! Renders a `.canvas` bundle's page (or the built-in test page) to PNG.
//!
//! `cargo run -p render-cpu --release --example render -- File.canvas [--page N] [--scale S] [--node ID] -o out.png`
//! `cargo run -p render-cpu --release --example render -- --test-page -o out.png`
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Instant;

use doc::*;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let value = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let out = value("-o").unwrap_or_else(|| "out.png".into());
    let scale: f64 = value("--scale").map_or(1.0, |s| s.parse().expect("--scale"));
    let page_no: usize = value("--page").map_or(0, |s| s.parse().expect("--page"));
    let (doc, assets): (Document, Arc<dyn render::AssetSource>) = if args.iter().any(|a| a == "--test-page") {
        let mut s = Store::default().with_ids(IdGen::seeded(7));
        s.transact(TxMeta::user("page"), doc::testing::render_test_page).unwrap();
        (s.into_doc(), Arc::new(render::NoAssets))
    } else {
        let path = args.first().expect("path to a .canvas bundle");
        let opened = format::Bundle::open(path).expect("open bundle");
        (opened.doc, Arc::new(opened.bundle))
    };
    let page = doc.node(doc.pages()[page_no]).unwrap().id;
    let node = value("--node").map(|s| s.parse::<NodeId>().expect("--node id"));
    let t = Instant::now();
    let png = render_cpu::render_png(&doc, page, node, scale, text::FontDb::new(!args.iter().any(|a| a == "--bundled-fonts")), assets).unwrap();
    std::fs::write(&out, png).unwrap();
    eprintln!("wrote {out} in {:.1} ms", t.elapsed().as_secs_f64() * 1e3);
}
