//! Stage 4 render scenarios: frames on the GPU (Vello) and CPU exports (vello_cpu), the spatial
//! index, and micro-benchmarks for fragment building and the reference blur.
//!
//! A GPU frame is timed from `Scene::frame` (cull, rebuild dirty fragments) through
//! `Renderer::render` until the GPU has finished (`device.poll`), at 1920×1080.

use std::sync::Arc;
use std::time::Instant;

use doc::testing::{GenConfig, STRESS_ROOT, generate};
use doc::*;
use index::SpatialIndex;
use kurbo::{Affine, Rect};
use layout::LayoutDeriver;
use render::{NoAssets, Renderer, Scene};
use render_vello::{GpuTarget, VelloRenderer};
use serde_json::{Value, json};
use text::{FontDb, TextEngine};

use crate::harness::{Report, Scenario, ms, repeat, stats};

const W: u32 = 1920;
const H: u32 = 1080;

pub fn all() -> Vec<Box<dyn Scenario>> {
    vec![Box::new(Pan), Box::new(EditFrame), Box::new(Effects), Box::new(Export), Box::new(Index), Box::new(Micro)]
}

/// The adapter the GPU scenarios use, for the host block.
pub fn gpu_info() -> Value {
    match VelloRenderer::headless() {
        Ok(r) => r.adapter_info().map_or(Value::Null, |a| {
            json!({ "name": a.name, "backend": format!("{:?}", a.backend), "driver": a.driver, "driver_info": a.driver_info })
        }),
        Err(e) => json!({ "error": e.to_string() }),
    }
}

fn fonts() -> FontDb {
    FontDb::new(true)
}

/// A generated document, laid out (stored sizes match layout, as a saved file would).
pub(crate) fn laid_out(cfg: &GenConfig) -> Store {
    let doc = Document::from_data(generate(cfg)).unwrap_or_default();
    let mut s = Store::new(doc);
    s.set_deriver(Box::new(LayoutDeriver::new(TextEngine::new(fonts()))));
    let _ = s.transact_with_deriver(TxMeta::user("layout"), |tx, d| d.map_or(Ok(()), |d| d.derive_all(tx)));
    s
}

/// The page with the most nodes.
fn biggest_page(doc: &Document) -> NodeId {
    doc.pages().iter().max_by_key(|k| doc.descendants(**k).len()).and_then(|k| doc.node(*k)).map(|n| n.id).expect("a page")
}

fn page_of(doc: &Document, name: &str) -> (NodeId, NodeId) {
    let (_, n) = doc.iter().find(|(_, n)| n.name == name).expect("named node");
    let k = doc.key(n.id).expect("key");
    let page = doc.ancestors(k).filter_map(|a| doc.node(a)).find(|a| a.kind.is_page()).map(|p| p.id).expect("page");
    (page, n.id)
}

pub(crate) struct Gpu {
    r: VelloRenderer,
    t: GpuTarget,
}

impl Gpu {
    pub(crate) fn new() -> Option<Gpu> {
        match VelloRenderer::headless() {
            Ok(r) => {
                let t = r.target(W, H);
                Some(Gpu { r, t })
            }
            Err(e) => {
                eprintln!("  no GPU ({e}); skipped");
                None
            }
        }
    }

    /// One frame: (total ms, of which scene.frame ms).
    pub(crate) fn frame(&mut self, scene: &mut Scene, doc: &Document, camera: Affine) -> (f64, f64) {
        let (total, build, _) = self.frame_split(scene, doc, camera);
        (total, build)
    }

    /// One frame: (total ms, of which scene.frame ms, of which waiting for the GPU ms).
    pub(crate) fn frame_split(&mut self, scene: &mut Scene, doc: &Document, camera: Affine) -> (f64, f64, f64) {
        let t = Instant::now();
        let fr = scene.frame(doc, camera, W, H, None);
        let build = ms(t);
        let _ = self.r.render(&fr, &mut self.t);
        let w = Instant::now();
        let _ = self.r.wait();
        (ms(t), build, ms(w))
    }
}

/// Camera at `zoom` whose view's top-left is world `(x, y)`.
fn camera(zoom: f64, x: f64, y: f64) -> Affine {
    Affine::scale(zoom) * Affine::translate((-x, -y))
}

/// Camera at `zoom` centred on `c`.
pub(crate) fn centred(zoom: f64, c: kurbo::Point) -> Affine {
    camera(zoom, c.x - W as f64 / 2.0 / zoom, c.y - H as f64 / 2.0 / zoom)
}

// ---- pan and zoom over 100k nodes -------------------------------------------------------------

struct Pan;

impl Scenario for Pan {
    fn name(&self) -> &'static str {
        "pan"
    }
    fn describe(&self) -> &'static str {
        "GPU frames over a 100k-node page at 1920×1080: pan at 100/15/5 %, zoom sweep, first frame"
    }
    fn run(&self, r: &mut Report) {
        let Some(mut gpu) = Gpu::new() else { return };
        let s = laid_out(&GenConfig { target_nodes: 100_000, ..Default::default() });
        let doc = s.doc();
        let page = biggest_page(doc);
        let t = Instant::now();
        let mut scene = Scene::new(doc, page, fonts(), Arc::new(NoAssets));
        r.once("scene_new_100k", ms(t), None, None);
        let b = scene.page_bounds(doc).unwrap_or(Rect::new(0.0, 0.0, 16000.0, 12000.0));
        r.info("page_nodes", doc.descendants(doc.key(page).expect("page")).len());
        r.info("fragments", scene.fragment_count());
        r.info("page_bounds", format!("{:.0}×{:.0}", b.width(), b.height()));

        let (first, first_build) = gpu.frame(&mut scene, doc, centred(1.0, b.center()));
        r.once("first_frame_100pct_cold", first, None, None);
        r.info("first_frame_100pct_scene_ms", first_build);

        for (label, zoom) in [("100pct", 1.0), ("15pct", 0.15), ("5pct", 0.05)] {
            // A horizontal pan across the middle of the page, then back; the first pass warms
            // the fragments of this band.
            let view_w = W as f64 / zoom;
            let span = (b.width() - view_w).max(0.0);
            let path: Vec<Affine> = (0..120).map(|i| {
                let f = if i < 60 { i as f64 / 59.0 } else { (119 - i) as f64 / 59.0 };
                camera(zoom, b.x0 + span * f, b.center().y - H as f64 / 2.0 / zoom)
            }).collect();
            for c in &path {
                gpu.frame(&mut scene, doc, *c);
            }
            let (mut total, mut build, mut wait) = (Vec::new(), Vec::new(), Vec::new());
            for c in &path {
                let (t, bl, w) = gpu.frame_split(&mut scene, doc, *c);
                total.push(t);
                build.push(bl);
                wait.push(w);
            }
            r.metric(&format!("pan_{label}_warm"), stats(total), Some(16.0), Some(8.0));
            r.metric(&format!("pan_{label}_of_which_scene"), stats(build), None, None);
            r.metric(&format!("pan_{label}_of_which_gpu_wait"), stats(wait), None, None);
            r.info(&format!("pan_{label}_visible_fragments"), scene.stats.visible);
            r.info(&format!("pan_{label}_runs_reused"), gpu.r.stats.runs_reused);
            r.info(&format!("pan_{label}_vello_paths"), gpu.r.stats.paths);
            r.info(&format!("pan_{label}_execute_ms"), gpu.r.stats.execute_ms);
            r.info(&format!("pan_{label}_vello_submit_ms"), gpu.r.stats.submit_ms);
        }

        // Zoom sweep 3 % → 200 % → 3 % around the centre, crossing every band: worst frame
        // includes re-encoding fragments for a new band (no encode budget).
        let zooms: Vec<f64> = (0..120).map(|i| {
            let f = if i < 60 { i as f64 / 59.0 } else { (119 - i) as f64 / 59.0 };
            0.03 * (2.0f64 / 0.03).powf(f)
        }).collect();
        let mut first_sweep = Vec::new();
        for z in &zooms {
            first_sweep.push(gpu.frame(&mut scene, doc, centred(*z, b.center())).0);
        }
        r.metric("zoom_sweep_first_pass", stats(first_sweep), Some(16.0), Some(8.0));
        let m: Vec<f64> = zooms.iter().map(|z| gpu.frame(&mut scene, doc, centred(*z, b.center())).0).collect();
        r.metric("zoom_sweep_warm", stats(m), Some(16.0), Some(8.0));

        // First frame of the 50 MB document, everything cold (new scene, new renderer caches).
        drop(scene);
        let s50 = laid_out(&GenConfig { target_nodes: 64_500, seed: 2, ..Default::default() });
        let d50 = s50.doc();
        let p50 = biggest_page(d50);
        for (label, fit) in [("fit", true), ("100pct", false)] {
            gpu.r.clear_caches();
            let t = Instant::now();
            let mut sc = Scene::new(d50, p50, fonts(), Arc::new(NoAssets));
            let pb = sc.page_bounds(d50).unwrap_or(Rect::new(0.0, 0.0, 1.0, 1.0));
            let cam = if fit { render::fit_camera(pb, W, H) } else { centred(1.0, pb.center()) };
            let new_ms = ms(t);
            let (f, build) = gpu.frame(&mut sc, d50, cam);
            r.once(&format!("first_frame_50mb_{label}"), new_ms + f, None, None);
            r.info(&format!("first_frame_50mb_{label}_scene_new_ms"), new_ms);
            r.info(&format!("first_frame_50mb_{label}_scene_frame_ms"), build);
        }
    }
}

// ---- edits → frame ----------------------------------------------------------------------------

struct EditFrame;

impl Scenario for EditFrame {
    fn name(&self) -> &'static str {
        "editframe"
    }
    fn describe(&self) -> &'static str {
        "Edit → pixels on the GPU: keystroke in the 10k auto-layout tree, dragging an artboard and an icon"
    }
    fn run(&self, r: &mut Report) {
        let Some(mut gpu) = Gpu::new() else { return };
        let mut s = laid_out(&GenConfig { target_nodes: 100_000, auto_layout_heavy: true, ..Default::default() });
        let (stress_page, root) = page_of(s.doc(), STRESS_ROOT);
        let kids = |s: &Store, id: NodeId| -> Vec<NodeId> { s.doc().get(id).map(|n| n.children.iter().filter_map(|k| s.doc().node(*k)).map(|c| c.id).collect()).unwrap_or_default() };
        let sections = kids(&s, root);
        let card = kids(&s, sections[3])[3];
        let body = kids(&s, card)[2];

        // Keystrokes: transaction + layout, scene update, frame, GPU.
        let mut scene = Scene::new(s.doc(), stress_page, fonts(), Arc::new(NoAssets));
        r.info("stress_fragments", scene.fragment_count());
        let cam = centred(1.0, scene.node_bounds(s.doc(), card).map_or(kurbo::Point::ZERO, |b| b.center()));
        for _ in 0..3 {
            gpu.frame(&mut scene, s.doc(), cam);
        }
        let mut text = String::from("Typing ");
        let (mut tx_ms, mut apply_ms, mut frame_ms, mut total) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for i in 0..200 {
            text.push(if i % 6 == 5 { ' ' } else { 'a' });
            let props = json!({"spans": [{"text": text}]});
            let t = Instant::now();
            let Ok((_, ch)) = s.transact(TxMeta::user("type").coalesce("typing"), |tx| tx.update_json(body, props.as_object().unwrap_or(&Default::default()))) else { continue };
            tx_ms.push(ms(t));
            let t2 = Instant::now();
            scene.apply(s.doc(), &ch);
            apply_ms.push(ms(t2));
            frame_ms.push(gpu.frame(&mut scene, s.doc(), cam).0);
            total.push(ms(t));
        }
        r.metric("keystroke_to_frame", stats(total), Some(16.0), Some(8.0));
        r.metric("keystroke_of_which_tx_and_layout", stats(tx_ms), None, None);
        r.metric("keystroke_of_which_scene_apply", stats(apply_ms), None, None);
        r.metric("keystroke_of_which_frame_and_gpu", stats(frame_ms), None, None);
        r.info("keystroke_fragments_rebuilt_last", scene.stats.built);

        // Dragging on the busiest page: a whole artboard (transform only) and the absolutely
        // positioned icon inside a card (re-encodes the artboard's fragment).
        let page = biggest_page(s.doc());
        let mut scene = Scene::new(s.doc(), page, fonts(), Arc::new(NoAssets));
        let (_, screen) = page_of(s.doc(), "Screen 12");
        let icon = s.doc().iter().find(|(_, n)| n.name == "Icon" && s.doc().key(n.id).is_some_and(|k| s.doc().ancestors(k).filter_map(|a| s.doc().node(a)).any(|a| a.id == screen))).map(|(_, n)| n.id).expect("icon");
        let at = scene.node_bounds(s.doc(), screen).map_or(kurbo::Point::ZERO, |b| b.center());
        for (label, node, base) in [("artboard", screen, s.doc().get(screen).map_or((0.0, 0.0), |n| (n.props.x, n.props.y))), ("icon_in_card", icon, (280.0, 12.0))] {
            let cam = centred(if label == "artboard" { 0.5 } else { 1.0 }, at);
            for _ in 0..3 {
                gpu.frame(&mut scene, s.doc(), cam);
            }
            let (mut tx_ms, mut apply_ms, mut wait_ms) = (Vec::new(), Vec::new(), Vec::new());
            let m = repeat(120, |i| {
                let d = (i % 40) as f64;
                let props = json!({"x": base.0 + d, "y": base.1 + d / 2.0});
                let t = Instant::now();
                if let Ok((_, ch)) = s.transact(TxMeta::user("drag").coalesce("drag"), |tx| tx.update_json(node, props.as_object().unwrap_or(&Default::default()))) {
                    tx_ms.push(ms(t));
                    let t = Instant::now();
                    scene.apply(s.doc(), &ch);
                    apply_ms.push(ms(t));
                }
                wait_ms.push(gpu.frame_split(&mut scene, s.doc(), cam).2);
            });
            r.metric(&format!("drag_{label}_per_frame"), m, Some(16.0), Some(8.0));
            r.metric(&format!("drag_{label}_of_which_tx_and_layout"), stats(tx_ms), None, None);
            r.metric(&format!("drag_{label}_of_which_scene_apply"), stats(apply_ms), None, None);
            r.metric(&format!("drag_{label}_of_which_gpu_wait"), stats(wait_ms), None, None);
            r.info(&format!("drag_{label}_vello_paths"), gpu.r.stats.paths);
            r.info(&format!("drag_{label}_vello_glyphs"), gpu.r.stats.glyphs);
            r.info(&format!("drag_{label}_execute_ms"), gpu.r.stats.execute_ms);
            r.info(&format!("drag_{label}_vello_submit_ms"), gpu.r.stats.submit_ms);
        }
    }
}

// ---- effects ----------------------------------------------------------------------------------

struct Effects;

fn effects_doc(background_blurs: bool) -> (Document, NodeId) {
    let mut s = Store::default().with_ids(IdGen::seeded(4));
    let page = s
        .transact(TxMeta::user("fx"), |tx| {
            let page = tx.insert_json(None, None, json!({"type": "page", "background": "#f0f0f0"}))?;
            if background_blurs {
                // A busy backdrop, then 10 translucent cards that blur it.
                for i in 0..120 {
                    let (x, y) = ((i % 15) as f64 * 128.0, (i / 15) as f64 * 135.0);
                    tx.insert_json(Some(page), None, json!({"type": "ellipse", "x": x, "y": y, "width": 110, "height": 110, "fills": [{"type": "solid", "color": format!("#{:06x}", (i * 2654435761u64) & 0xffffff)}]}))?;
                }
                for i in 0..10 {
                    let (x, y) = (60.0 + (i % 5) as f64 * 360.0, 200.0 + (i / 5) as f64 * 420.0);
                    tx.insert_json(Some(page), None, json!({"type": "rectangle", "x": x, "y": y, "width": 300, "height": 240, "radius": 16,
                        "fills": [{"type": "solid", "color": "#ffffff66"}], "effects": [{"type": "backgroundBlur", "radius": 20}]}))?;
                }
            } else {
                // 50 layer-blurred ellipses and 50 stars with drop shadows (the offscreen path).
                for i in 0..100 {
                    let (x, y) = (20.0 + (i % 10) as f64 * 190.0, 20.0 + (i / 10) as f64 * 105.0);
                    let n = if i % 2 == 0 {
                        json!({"type": "ellipse", "x": x, "y": y, "width": 80, "height": 80, "fills": [{"type": "solid", "color": "#3070e0"}], "effects": [{"type": "layerBlur", "radius": 8}]})
                    } else {
                        json!({"type": "polygon", "sides": 5, "starInnerRatio": 0.5, "x": x, "y": y, "width": 80, "height": 80, "fills": [{"type": "solid", "color": "#e0a020"}],
                            "effects": [{"type": "dropShadow", "color": "#00000066", "offset": [4, 6], "blur": 16}]})
                    };
                    tx.insert_json(Some(page), None, n)?;
                }
            }
            Ok(page)
        })
        .map(|(p, _)| p)
        .expect("effects doc");
    (s.into_doc(), page)
}

impl Scenario for Effects {
    fn name(&self) -> &'static str {
        "effects"
    }
    fn describe(&self) -> &'static str {
        "GPU frames with offscreen effects: 50 layer blurs + 50 star shadows; 10 background blurs"
    }
    fn run(&self, r: &mut Report) {
        let Some(mut gpu) = Gpu::new() else { return };
        for (label, bg) in [("50_blurs_50_shadows", false), ("10_background_blurs", true)] {
            let (doc, page) = effects_doc(bg);
            let mut scene = Scene::new(&doc, page, fonts(), Arc::new(NoAssets));
            let cam = Affine::IDENTITY;
            for _ in 0..3 {
                gpu.frame(&mut scene, &doc, cam);
            }
            let (mut cpu, mut wait) = (Vec::new(), Vec::new());
            let m = repeat(60, |_| {
                let t = Instant::now();
                let fr = scene.frame(&doc, cam, W, H, None);
                let _ = gpu.r.render(&fr, &mut gpu.t);
                cpu.push(ms(t));
                let t = Instant::now();
                let _ = gpu.r.wait();
                wait.push(ms(t));
            });
            r.metric(&format!("frame_{label}_of_which_cpu_submit"), stats(cpu), None, None);
            r.metric(&format!("frame_{label}_of_which_gpu_wait"), stats(wait), None, None);
            let gate = if bg { None } else { Some(8.0) };
            r.metric(&format!("frame_{label}"), m, Some(16.0), gate);
            r.info(&format!("{label}_vello_renders"), gpu.r.stats.renders);
            r.info(&format!("{label}_compute_passes"), gpu.r.stats.compute_passes);
            r.info(&format!("{label}_cpu_offscreen_submit_ms"), gpu.r.stats.offscreen_ms);
            r.info(&format!("{label}_cpu_effect_submit_ms"), gpu.r.stats.effects_ms);
            eprintln!("    renders {} passes {} offscreen {:.2} ms effects {:.2} ms scene {:.2} ms", gpu.r.stats.renders, gpu.r.stats.compute_passes, gpu.r.stats.offscreen_ms, gpu.r.stats.effects_ms, scene.stats.build_ms);
        }
    }
}

// ---- CPU export -------------------------------------------------------------------------------

struct Export;

impl Scenario for Export {
    fn name(&self) -> &'static str {
        "export"
    }
    fn describe(&self) -> &'static str {
        "CPU export (vello_cpu, all cores) of a 100k-node page at 4K: detail at 100 % and the whole page"
    }
    fn run(&self, r: &mut Report) {
        let s = laid_out(&GenConfig { target_nodes: 100_000, ..Default::default() });
        let doc = s.doc();
        let page = biggest_page(doc);
        let mut scene = Scene::new(doc, page, fonts(), Arc::new(NoAssets));
        let b = scene.page_bounds(doc).unwrap_or(Rect::new(0.0, 0.0, 1.0, 1.0));
        let mut cpu = render_cpu::CpuRenderer::default();
        let detail = Rect::from_origin_size(b.origin(), (3840.0, 2160.0));
        let overview_scale = 3840.0 / b.width();
        for (label, rect, scale) in [("detail", detail, 1.0), ("overview", b, overview_scale)] {
            let mut png_ms = Vec::new();
            let m = repeat(5, |_| {
                if let Ok(px) = render_cpu::render_rect(&mut scene, doc, rect, scale, &mut cpu) {
                    let t = Instant::now();
                    let _ = render_cpu::png(&px);
                    png_ms.push(ms(t));
                }
            });
            r.metric(&format!("export_4k_{label}_render"), m, None, None);
            r.metric(&format!("export_4k_{label}_png_encode"), stats(png_ms), None, None);
        }
    }
}

// ---- spatial index ----------------------------------------------------------------------------

struct Index;

impl Scenario for Index {
    fn name(&self) -> &'static str {
        "index"
    }
    fn describe(&self) -> &'static str {
        "Spatial index over 100k nodes: bulk load, viewport query, incremental update"
    }
    fn run(&self, r: &mut Report) {
        let mut s = laid_out(&GenConfig { target_nodes: 100_000, ..Default::default() });
        let page = biggest_page(s.doc());
        r.metric("bulk_load_100k", repeat(5, |_| drop(SpatialIndex::build(s.doc()))), None, None);
        let mut idx = SpatialIndex::build(s.doc());
        r.info("indexed", idx.len());
        // Queries walk the page's bounds, so they hit content (hit counts are reported).
        let b = idx.page_bounds(page).unwrap_or(Rect::new(0.0, 0.0, 14000.0, 10000.0));
        let at = |i: usize| (b.x0 + (i * 977) as f64 % b.width().max(1.0), b.y0 + (i * 613) as f64 % b.height().max(1.0));
        let mut hits = 0;
        let m = repeat(1000, |i| {
            let (x, y) = at(i);
            hits += idx.query(page, Rect::new(x, y, x + W as f64, y + H as f64)).count();
        });
        r.metric("query_viewport_100pct", m, None, Some(0.2));
        r.info("query_viewport_hits_avg", hits / 1000);
        let mut hits = 0;
        let m = repeat(1000, |i| {
            let (x, y) = at(i);
            hits += idx.query(page, Rect::new(x, y, x + 1.0, y + 1.0)).count();
        });
        r.metric("query_point", m, Some(1.0), Some(0.2));
        r.info("query_point_hits_avg", hits as f64 / 1000.0);
        let rect = s.doc().iter().find(|(_, n)| n.name == "Tag").map(|(_, n)| n.id).expect("tag");
        let card = s.doc().iter().find(|(_, n)| n.name == "Card 3").map(|(_, n)| n.id).expect("card");
        for (label, node, prop) in [("leaf", rect, "rotation"), ("card_subtree", card, "rotation")] {
            let mut apply = Vec::new();
            for i in 0..500 {
                let props = json!({ prop: (i % 30) as f64 });
                if let Ok((_, ch)) = s.transact(TxMeta::user("u"), |tx| tx.update_json(node, props.as_object().unwrap_or(&Default::default()))) {
                    let t = Instant::now();
                    idx.apply(s.doc(), &ch);
                    apply.push(ms(t));
                }
            }
            r.metric(&format!("update_{label}"), stats(apply), None, None);
        }
    }
}

// ---- micro-benchmarks -------------------------------------------------------------------------

struct Micro;

impl Scenario for Micro {
    fn name(&self) -> &'static str {
        "micro"
    }
    fn describe(&self) -> &'static str {
        "Fragment building (one artboard, near band) and the CPU reference blur"
    }
    fn run(&self, r: &mut Report) {
        let s = laid_out(&GenConfig { target_nodes: 2_000, ..Default::default() });
        let doc = s.doc();
        let page = biggest_page(doc);
        let (_, screen) = page_of(doc, "Screen 1");
        let mut scene = Scene::new(doc, page, fonts(), Arc::new(NoAssets));
        let cam = centred(1.0, scene.node_bounds(doc, screen).map_or(kurbo::Point::ZERO, |b| b.center()));
        scene.frame(doc, cam, W, H, None);
        // Rebuild the artboard's fragment each time (an edit inside it), text cached.
        let fill = json!({"fills": [{"type": "solid", "color": "#fefefe"}]});
        let mut st = Store::new(doc.clone());
        let m = repeat(50, |_| {
            if let Ok((_, ch)) = st.transact(TxMeta::user("f"), |tx| tx.update_json(screen, fill.as_object().unwrap_or(&Default::default()))) {
                scene.apply(st.doc(), &ch);
            }
            scene.frame(st.doc(), cam, W, H, None);
        });
        r.metric("rebuild_artboard_fragment_and_frame", m, None, None);
        r.info("artboard_nodes", doc.descendants(doc.key(screen).expect("key")).len());
        for (w, sigma) in [(256u32, 4.0), (512, 8.0), (512, 24.0), (1024, 32.0)] {
            let mut px = render::blur::Pixels::new(0, 0, w, w);
            for (i, v) in px.data.iter_mut().enumerate() {
                *v = (i * 31 % 256) as u8;
            }
            let m = repeat(10, |_| render::blur::blur(&mut px, sigma));
            r.metric(&format!("cpu_blur_{w}px_sigma{sigma}"), m, None, None);
        }
    }
}
