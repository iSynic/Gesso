//! Stage 7b scenarios: SVG and PDF export of a 200-node screen, PDF of a 100k-node page, a 4K
//! JPEG, importing a 10k-path SVG, pasting 2,000 nodes, and GPU frames while 100 images decode
//! in the background.

use std::fmt::Write as _;
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use doc::testing::GenConfig;
use doc::*;
use editor::{Camera, Session};
use kurbo::{Point, Vec2};
use render::{MemoryAssets, NoAssets, Scene};
use serde_json::json;
use text::FontDb;

use crate::harness::{Report, Scenario, ms, repeat, stats};
use crate::render::{Gpu, centred, laid_out};

pub fn all() -> Vec<Box<dyn Scenario>> {
    vec![Box::new(Export)]
}

fn fonts() -> FontDb {
    FontDb::new(true)
}

/// The top-level frame whose subtree size is closest to `n`, and that size.
fn screen_of(doc: &Document, n: usize) -> (NodeId, usize) {
    doc.pages()
        .iter()
        .filter_map(|p| doc.node(*p))
        .flat_map(|p| p.children.iter().copied())
        .map(|k| (k, doc.descendants(k).len() + 1))
        .min_by_key(|(_, s)| s.abs_diff(n))
        .and_then(|(k, s)| Some((doc.node(k)?.id, s)))
        .expect("a frame")
}

fn page_children(doc: &Document, page: NodeKey) -> Vec<NodeId> {
    doc.node(page).map(|p| p.children.iter().filter_map(|c| doc.node(*c)).map(|n| n.id).collect()).unwrap_or_default()
}

/// An SVG of `n` shapes: rectangles, circles, curved paths, a gradient every tenth, in groups
/// of 100 with transforms.
fn big_svg(n: usize) -> String {
    let mut s = String::from(r##"<svg xmlns="http://www.w3.org/2000/svg" width="2000" height="2000"><defs><linearGradient id="g"><stop offset="0" stop-color="#2563eb"/><stop offset="1" stop-color="#dc2626"/></linearGradient></defs>"##);
    for g in 0..n.div_ceil(100) {
        let _ = write!(s, r#"<g transform="translate({} {})">"#, (g % 10) * 200, (g / 10) * 200);
        for i in 0..100.min(n - g * 100) {
            let (x, y) = ((i % 10) * 20, (i / 10) * 20);
            let fill = if i % 10 == 0 { "url(#g)".to_string() } else { format!("#{:06x}", (i * 2_654_435) & 0xff_ffff) };
            let _ = match i % 3 {
                0 => write!(s, r#"<rect x="{x}" y="{y}" width="16" height="16" rx="3" fill="{fill}"/>"#),
                1 => write!(s, r##"<circle cx="{}" cy="{}" r="8" fill="{fill}" stroke="#111" stroke-width="1"/>"##, x + 8, y + 8),
                _ => write!(s, r#"<path d="M{x} {y} C{} {y} {} {} {} {} Z" fill="{fill}"/>"#, x + 16, x + 16, y + 16, x + 8, y + 16),
            };
        }
        s.push_str("</g>");
    }
    s.push_str("</svg>");
    s
}

/// A `size`×`size` JPEG with detail (so decoding does real work), different per `seed`.
fn photo(size: u32, seed: u32) -> Vec<u8> {
    let img = image::RgbImage::from_fn(size, size, |x, y| {
        let v = (x.wrapping_mul(7).wrapping_add(y.wrapping_mul(13)).wrapping_add(seed.wrapping_mul(97))) ^ (x * y / 7);
        image::Rgb([(v & 0xff) as u8, ((v >> 3) & 0xff) as u8, ((x + y + seed) & 0xff) as u8])
    });
    let mut out = Vec::new();
    let _ = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85).encode_image(&img);
    out
}

struct Export;

impl Scenario for Export {
    fn name(&self) -> &'static str {
        "export_formats"
    }
    fn describe(&self) -> &'static str {
        "Export and import: SVG/PDF of a 200-node screen, PDF of a 100k-node page, a 4K JPEG, a 10k-path SVG import, a 2,000-node paste, GPU frames while 100 images decode"
    }
    fn run(&self, r: &mut Report) {
        // A screen of about 200 nodes from a generated document.
        let s = laid_out(&GenConfig { target_nodes: 10_000, ..Default::default() });
        let doc = s.doc();
        let (screen, nodes) = screen_of(doc, 200);
        r.info("screen_nodes", nodes);
        let assets = NoAssets;
        let opts = export::Options { scale: 1.0, quality: 90 };
        let mut sizes = (0, 0);
        r.metric(
            "svg_200_node_screen",
            repeat(20, |_| {
                let c = export::collect(doc, &[screen], fonts(), &assets).expect("collect");
                sizes.0 = export::write(&c, export::Format::Svg, opts).expect("svg").len();
            }),
            None,
            None,
        );
        r.metric(
            "pdf_200_node_screen",
            repeat(20, |_| {
                let c = export::collect(doc, &[screen], fonts(), &assets).expect("collect");
                sizes.1 = export::write(&c, export::Format::Pdf, opts).expect("pdf").len();
            }),
            None,
            None,
        );
        r.info("svg_200_node_screen_kb", sizes.0.div_ceil(1024));
        r.info("pdf_200_node_screen_kb", sizes.1.div_ceil(1024));

        // A 4K-wide JPEG of the same screen.
        let c = export::collect(doc, &[screen], fonts(), &assets).expect("collect");
        let scale = 3840.0 / c.width.max(1.0);
        r.info("jpeg_4k_pixels", format!("{}x{}", (c.width * scale).ceil(), (c.height * scale).ceil()));
        r.metric(
            "jpeg_4k_screen",
            repeat(5, |_| {
                let _ = export::write(&c, export::Format::Jpeg, export::Options { scale, quality: 85 }).expect("jpeg");
            }),
            None,
            None,
        );

        // PDF of 100k nodes: every page of the generated document drawn onto one PDF page (they
        // overlap; the generator's largest page holds about 24k nodes).
        let big = laid_out(&GenConfig { target_nodes: 100_000, ..Default::default() });
        let bd = big.doc();
        let ids: Vec<NodeId> = bd.pages().iter().flat_map(|p| page_children(bd, *p)).collect();
        r.info("pdf_page_nodes", bd.pages().iter().map(|p| bd.descendants(*p).len()).sum::<usize>());
        let mut kb = 0;
        let mut collect_ms = Vec::new();
        r.metric(
            "pdf_100k_node_page",
            repeat(3, |_| {
                let t = Instant::now();
                let c = export::collect(bd, &ids, fonts(), &assets).expect("collect");
                collect_ms.push(ms(t));
                kb = export::write(&c, export::Format::Pdf, opts).expect("pdf").len().div_ceil(1024);
            }),
            None,
            None,
        );
        r.metric("pdf_100k_node_page_of_which_collect", stats(collect_ms), None, None);
        r.info("pdf_100k_node_page_kb", kb);
        drop(big);

        // Importing a 10k-path SVG and inserting it.
        let svg = big_svg(10_000);
        r.info("svg_import_kb", svg.len().div_ceil(1024));
        let mut parse_ms = Vec::new();
        r.metric(
            "svg_import_10k_paths",
            repeat(5, |_| {
                let t = Instant::now();
                let imp = export::import::import_svg(&svg, "Big", &[]).expect("import");
                parse_ms.push(ms(t));
                let mut st = Store::default();
                let _ = st.transact(TxMeta::user("import"), |tx| {
                    let page = tx.insert_json(None, None, json!({"type": "page"}))?;
                    tx.insert_json(Some(page), None, imp.node)
                });
            }),
            None,
            None,
        );
        r.metric("svg_import_10k_paths_of_which_convert", stats(parse_ms), None, None);

        // Pasting 2,000 nodes (copied from a session, pasted into the same page).
        let one = laid_out(&GenConfig { target_nodes: 2_000, ..Default::default() });
        let doc2 = one.into_doc();
        let page2 = *doc2.pages().iter().max_by_key(|k| doc2.descendants(**k).len()).expect("page");
        let page_id = doc2.node(page2).map(|n| n.id).expect("page");
        let ids = page_children(&doc2, page2);
        if let Ok(mut sess) = Session::new(doc2, None, fonts()) {
            sess.set_page(page_id);
            sess.viewport = Vec2::new(1920.0, 1080.0);
            sess.camera = Camera { zoom: 0.25, origin: Point::ZERO };
            sess.select(ids);
            let text = editor::clipboard::copy(&sess).expect("copy");
            r.info("paste_nodes", sess.selection().iter().filter_map(|id| sess.doc().key(*id)).map(|k| sess.doc().descendants(k).len() + 1).sum::<usize>());
            r.info("paste_payload_kb", text.len().div_ceil(1024));
            r.metric(
                "paste_2000_nodes",
                repeat(5, |_| {
                    let _ = editor::clipboard::paste(&mut sess, &text).expect("paste");
                    let _ = sess.sync_tx();
                }),
                None,
                None,
            );
        }

        // GPU frames while 100 images (1024², JPEG) decode in the background.
        let Some(mut gpu) = Gpu::new() else { return };
        let mut mem = MemoryAssets::default();
        let mut st = Store::default().with_ids(IdGen::seeded(9));
        let t = Instant::now();
        let photos: Vec<Vec<u8>> = (0..100).map(|i| photo(1024, i)).collect();
        r.info("decode_images_prepare_ms", ms(t));
        let page = st
            .transact(TxMeta::user("images"), |tx| {
                let page = tx.insert_json(None, None, json!({"type": "page"}))?;
                for (i, bytes) in photos.iter().enumerate() {
                    let (id, meta) = format::identify_asset(bytes).map_err(|e| Error::Input(e.to_string()))?;
                    mem.0.insert(id.clone(), bytes.clone());
                    tx.put_asset(id.clone(), meta)?;
                    tx.insert_json(Some(page), None, json!({"type": "image", "asset": id.0, "x": (i % 10) as f64 * 110.0, "y": (i / 10) as f64 * 110.0, "width": 100, "height": 100}))?;
                }
                Ok(page)
            })
            .expect("images")
            .0;
        let doc3 = st.into_doc();
        let mem = Arc::new(mem);
        let cam = centred(1.0, Point::new(545.0, 545.0));
        // Synchronous decode, for comparison: the first frame waits for every image.
        let mut sync = Scene::new(&doc3, page, fonts(), mem.clone());
        r.info("decode_100_images_sync_first_frame_ms", gpu.frame(&mut sync, &doc3, cam).0);
        drop(sync);
        let mut scene = Scene::new(&doc3, page, fonts(), mem);
        let (tx, rx) = channel();
        let tx = Mutex::new(tx);
        scene.set_image_waker(Some(Arc::new(move || {
            if let Ok(t) = tx.lock() {
                let _ = t.send(());
            }
        })));
        let t = Instant::now();
        let (mut frames, mut builds) = (Vec::new(), Vec::new());
        loop {
            let (f, b, w) = gpu.frame_split(&mut scene, &doc3, cam);
            frames.push(f);
            builds.push((b, w));
            if scene.images_pending() == 0 && frames.len() > 1 {
                break;
            }
            // Redraw on a decode, or right away while decodes wait to be taken in (as the app
            // does), at most once per 16 ms.
            if !scene.images_ready() {
                let _ = rx.recv_timeout(Duration::from_millis(16));
            }
            while rx.try_recv().is_ok() {}
            if t.elapsed() > Duration::from_secs(60) {
                break;
            }
        }
        // One more frame draws the last decodes.
        frames.push(gpu.frame(&mut scene, &doc3, cam).0);
        r.info("decode_100_images_all_drawn_ms", ms(t));
        let worst = frames.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).map_or(0, |(i, _)| i);
        r.info("decode_frames_worst_index", worst);
        r.info("decode_frames_worst_scene_and_gpu_wait_ms", format!("{:?}", builds.get(worst)));
        r.info("decode_frames_first_ms", frames.first().copied().unwrap_or(0.0));
        r.metric("frames_while_100_images_decode", stats(frames), Some(16.0), None);
    }
}
