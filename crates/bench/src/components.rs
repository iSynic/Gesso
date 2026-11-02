//! Stage 7a component scenarios: editing a component shown by 100, 1,000 and 5,000 instances,
//! an override, a variable bound by 10k nodes and a mode switch, each to a frame on the GPU;
//! hit-testing with half the page inside instances; opening a 50 MB document with instances.

use std::sync::Arc;
use std::time::Instant;

use doc::testing::GenConfig;
use doc::*;
use editor::{Camera, Session};
use kurbo::{Affine, Point, Vec2};
use layout::LayoutDeriver;
use render::{NoAssets, Scene};
use serde_json::{Value, json};
use text::{FontDb, TextEngine};

use crate::harness::{Report, Scenario, ms, repeat, rss_mb, stats};
use crate::render::{Gpu, centred, laid_out};

pub fn all() -> Vec<Box<dyn Scenario>> {
    vec![Box::new(Components)]
}

const COLL: &str = "0Co00000000000000000V1";
const LIGHT: &str = "0Mo00000000000000000ML";
const DARK: &str = "0Mo00000000000000000MD";
const ACCENT: &str = "0Va00000000000000000VC";
const BUTTON: &str = "0Cm00000000000000000B1";
const ICON: &str = "0Cm00000000000000000I1";
const BG: &str = "0Rc00000000000000000BG";

fn variables(light: &str) -> Variables {
    serde_json::from_value(json!([{"id": COLL, "name": "Theme", "defaultMode": LIGHT,
        "modes": [{"id": LIGHT, "name": "Light"}, {"id": DARK, "name": "Dark"}],
        "variables": [{"id": ACCENT, "name": "accent", "type": "color", "values": {LIGHT: light, DARK: "#93c5fd"}}]}]))
    .unwrap_or_default()
}

/// Adds the theme collection to the document's variables (generated documents have their own).
fn add_theme(tx: &mut Tx<'_>) -> Result<(), Error> {
    let mut v = tx.doc().variables().clone();
    v.collections.retain(|c| c.id.to_string() != COLL);
    v.collections.extend(variables("#2563eb").collections);
    tx.set_variables(v)
}

fn fill(c: &str) -> Value {
    json!([{"type": "solid", "color": c}])
}

/// A button component (background, label, dot, nested icon instance: 8 nodes shown per
/// instance) on its own, at (0, -200).
fn button() -> Value {
    json!({"id": BUTTON, "type": "component", "name": "Button", "y": -200, "width": 160, "height": 48, "fills": fill("#ffffff"), "radius": 8, "children": [
        {"id": BG, "type": "rectangle", "name": "Bg", "x": 4, "y": 4, "width": 152, "height": 40, "radius": 6, "fills": fill("#2563eb"), "constraints": {"h": "leftRight", "v": "topBottom"}},
        {"type": "text", "name": "Label", "x": 40, "y": 14, "spans": [{"text": "Continue"}], "autoResize": "widthAndHeight", "style": {"fontFamily": "Inter", "fontSize": 14.0, "color": "#ffffff"}},
        {"type": "ellipse", "name": "Dot", "x": 140, "y": 20, "width": 8, "height": 8, "fills": fill("#ffffff"), "constraints": {"h": "right"}},
        {"type": "instance", "name": "Icon", "component": ICON, "x": 12, "y": 12, "width": 24, "height": 24}]})
}

fn icon() -> Value {
    json!({"id": ICON, "type": "component", "name": "Icon", "x": 200, "y": -200, "width": 24, "height": 24, "children": [
        {"type": "rectangle", "name": "Shape", "x": 4, "y": 4, "width": 16, "height": 16, "radius": 3, "fills": fill("#ffffff")}]})
}

/// A store with the button and `n` instances of it in a 50-column grid, each with a label
/// override, on its own page.
fn instances(n: usize) -> (Store, NodeId) {
    let mut s = Store::default().with_ids(IdGen::seeded(7));
    s.set_deriver(Box::new(LayoutDeriver::new(TextEngine::new(FontDb::new(true)))));
    let page = s.transact(TxMeta::user("page"), |tx| tx.insert_json(None, None, json!({"type": "page"}))).expect("page").0;
    let r = s.transact(TxMeta::user("setup"), |tx| {
        tx.set_variables(variables("#2563eb"))?;
        tx.insert_json(Some(page), None, icon())?;
        tx.insert_json(Some(page), None, button())?;
        let label_id = tx.doc().iter().find(|(_, n)| n.name == "Label").map(|(_, n)| n.id.to_string()).unwrap_or_default();
        for i in 0..n {
            let (x, y) = ((i % 50) as f64 * 180.0, (i / 50) as f64 * 60.0);
            tx.insert_json(Some(page), None, json!({"type": "instance", "name": format!("Use {i}"), "component": BUTTON, "x": x, "y": y, "width": 160, "height": 48,
                "overrides": [{"target": [label_id], "prop": "name", "value": format!("Label {i}")}]}))?;
        }
        Ok(())
    });
    assert!(r.is_ok(), "instances setup: {r:?}");
    (s, page)
}

/// Times `edit` (a transaction) through to a presented frame: transaction with layout and
/// expansion, scene update, frame build, GPU render and wait.
fn edit_to_frame(gpu: &mut Gpu, s: &mut Store, scene: &mut Scene, cam: Affine, runs: usize, mut edit: impl FnMut(&mut Tx<'_>, usize) -> Result<(), Error>) -> (Value, Value, Value, usize) {
    for _ in 0..3 {
        gpu.frame(scene, s.doc(), cam);
    }
    let (mut total, mut tx_ms, mut build_ms, mut expanded) = (Vec::new(), Vec::new(), Vec::new(), 0);
    for i in 0..runs {
        let t = Instant::now();
        let Ok((_, ch)) = s.transact(TxMeta::user("edit"), |tx| edit(tx, i)) else { continue };
        tx_ms.push(ms(t));
        expanded = expanded.max(ch.updated.len() + ch.inserted.len());
        scene.apply(s.doc(), &ch);
        build_ms.push(gpu.frame(scene, s.doc(), cam).1);
        total.push(ms(t));
    }
    (stats(total), stats(tx_ms), stats(build_ms), expanded)
}

fn fit(scene: &Scene, doc: &Document) -> Affine {
    let b = scene.page_bounds(doc).unwrap_or_default();
    let z = (1920.0 / b.width().max(1.0)).min(1080.0 / b.height().max(1.0)) * 0.95;
    centred(z, b.center())
}

struct Components;

impl Scenario for Components {
    fn name(&self) -> &'static str {
        "components"
    }
    fn describe(&self) -> &'static str {
        "Components and variables: component edits fanned out to 100/1k/5k instances, an override, a variable bound by 10k nodes and a mode switch (each to a GPU frame); hit-testing with half the nodes in instances; opening 50 MB with instances"
    }
    fn run(&self, r: &mut Report) {
        let fonts = || FontDb::new(true);
        if let Some(mut gpu) = Gpu::new() {
            // Component edit → every instance re-expanded → frame, with all instances in view.
            for n in [100usize, 1000, 5000] {
                let (mut s, page) = instances(n);
                let mut scene = Scene::new(s.doc(), page, fonts(), Arc::new(NoAssets));
                let cam = fit(&scene, s.doc());
                let bg: NodeId = BG.parse().expect("id");
                let (total, tx, build, writes) = edit_to_frame(&mut gpu, &mut s, &mut scene, cam, if n > 1000 { 20 } else { 50 }, |tx, i| {
                    tx.update(bg, PropValue::from_json(PropKey::Fills, fill(["#dc2626", "#16a34a"][i % 2])).map_err(|e| Error::Input(e.to_string()))?)
                });
                let gate = (n <= 1000).then_some(16.0);
                r.metric(&format!("component_edit_{n}_instances_to_frame"), total, gate, None);
                r.metric(&format!("component_edit_{n}_of_which_tx"), tx, None, None);
                r.metric(&format!("component_edit_{n}_of_which_frame_build"), build, None, None);
                r.info(&format!("component_edit_{n}_changes"), writes);

                // One override edit, viewed at 100 %.
                if n == 1000 {
                    let target = s.doc().iter().find(|(_, x)| x.name == "Use 500").and_then(|(k, _)| s.doc().descendants(k).into_iter().filter_map(|k| s.doc().node(k)).find(|x| x.name == "Bg").map(|x| x.id));
                    let cam = centred(1.0, target.and_then(|t| scene.node_bounds(s.doc(), t)).map_or(Point::ZERO, |b| b.center()));
                    if let Some(t) = target {
                        let (total, _, _, _) = edit_to_frame(&mut gpu, &mut s, &mut scene, cam, 50, |tx, i| tx.update(t, PropValue::Opacity([0.5, 0.75][i % 2])));
                        r.metric("override_edit_to_frame", total, Some(16.0), None);
                    }
                }
            }

            // A colour variable bound by 10k rectangles (100 frames of 100), all in view.
            let mut s = Store::default().with_ids(IdGen::seeded(8));
            s.set_deriver(Box::new(LayoutDeriver::new(TextEngine::new(fonts()))));
            let page = s.transact(TxMeta::user("page"), |tx| tx.insert_json(None, None, json!({"type": "page"}))).expect("page").0;
            let rect = |i: usize| json!({"type": "rectangle", "x": (i % 10) as f64 * 12.0, "y": (i / 10) as f64 * 12.0, "width": 10, "height": 10, "fills": fill("#000000"), "bindings": {"fills/0/color": ACCENT}});
            let screen = |j: usize, n: usize| json!({"type": "frame", "name": format!("Screen {j}"), "x": (j % 10) as f64 * 140.0, "y": (j / 10) as f64 * 140.0, "width": 130, "height": 130, "fills": fill("#ffffff"), "children": (0..n).map(rect).collect::<Vec<_>>()});
            let r0 = s.transact(TxMeta::user("setup"), |tx| {
                tx.set_variables(variables("#2563eb"))?;
                for j in 0..100 {
                    tx.insert_json(Some(page), None, screen(j, 100))?;
                }
                Ok(())
            });
            assert!(r0.is_ok(), "variables setup");
            let mut scene = Scene::new(s.doc(), page, fonts(), Arc::new(NoAssets));
            let cam = fit(&scene, s.doc());
            let (total, tx, _, writes) = edit_to_frame(&mut gpu, &mut s, &mut scene, cam, 30, |tx, i| tx.set_variables(variables(["#dc2626", "#16a34a"][i % 2])));
            r.metric("variable_10k_bound_to_frame", total, Some(50.0), None);
            r.metric("variable_10k_bound_of_which_tx", tx, None, None);
            r.info("variable_10k_bound_writes", writes);

            // A mode switch on one screen of 1,000 bound nodes, in view.
            let r1 = s.transact(TxMeta::user("screen"), |tx| tx.insert_json(Some(page), None, json!({"type": "frame", "name": "Big", "y": 1500, "width": 400, "height": 1300, "fills": fill("#ffffff"),
                "children": (0..1000).map(|i| json!({"type": "rectangle", "x": (i % 30) as f64 * 13.0, "y": (i / 30) as f64 * 13.0, "width": 11, "height": 11, "fills": fill("#000000"), "bindings": {"fills/0/color": ACCENT}})).collect::<Vec<_>>()})));
            if let Ok((big, ch)) = r1 {
                scene.apply(s.doc(), &ch);
                let cam = centred(0.8, scene.node_bounds(s.doc(), big).map_or(Point::ZERO, |b| b.center()));
                let (coll, light, dark) = (COLL.parse().expect("id"), LIGHT.parse().expect("id"), DARK.parse().expect("id"));
                let (total, _, _, _) = edit_to_frame(&mut gpu, &mut s, &mut scene, cam, 50, |tx, i| tx.update(big, PropValue::Modes([(coll, if i % 2 == 0 { dark } else { light })].into())));
                r.metric("mode_switch_1k_bound_to_frame", total, Some(16.0), None);
            }
        }

        // Hit-testing with ~half the page's nodes inside instances.
        let base = crate::editor::one_page(&GenConfig { target_nodes: 50_000, ..Default::default() });
        let mut s = Session::new(base, None, FontDb::new(true)).expect("session");
        let page = s.doc().pages().iter().filter_map(|&k| s.doc().node(k)).max_by_key(|p| p.children.len()).map(|p| p.id).expect("page");
        s.set_page(page);
        let right = s.scene.index.page_bounds(page).map_or(0.0, |b| b.x1) + 500.0;
        let r2 = s.transact(TxMeta::user("instances"), |tx| {
            add_theme(tx)?;
            tx.insert_json(Some(page), None, icon())?;
            let mut c = button();
            c["x"] = json!(right);
            tx.insert_json(Some(page), None, c)?;
            for i in 0..6000 {
                tx.insert_json(Some(page), None, json!({"type": "instance", "component": BUTTON, "x": right + (i % 60) as f64 * 180.0, "y": (i / 60) as f64 * 60.0, "width": 160, "height": 48}))?;
            }
            Ok(())
        });
        assert!(r2.is_ok(), "hit-test setup: {:?}", r2.err());
        let doc = s.doc();
        let total = doc.key(page).map_or(0, |k| doc.descendants(k).len());
        let virt = doc.iter().filter(|(_, n)| n.virt.is_some()).count();
        r.info("hit_page_nodes", total);
        r.info("hit_virtual_nodes", virt);
        s.viewport = Vec2::new(1920.0, 1080.0);
        let b = s.scene.index.page_bounds(page).expect("bounds");
        s.camera = Camera { zoom: 1.0, origin: b.origin() };
        let at = |i: usize| Point::new(b.x0 + (i * 977) as f64 % b.width(), b.y0 + (i * 613) as f64 % b.height());
        let mut found = 0;
        r.metric("hit_test_half_in_instances", repeat(2000, |i| found += usize::from(s.pick(at(i), false).is_some())), Some(1.0), Some(0.2));
        r.metric("hit_test_deep_half_in_instances", repeat(2000, |i| found += usize::from(s.pick(at(i), true).is_some())), Some(1.0), Some(0.2));
        r.info("hit_found", found);
        drop(s);

        // Opening 50 MB with 2,000 instances: read, parse, build, shape text, expand, lay out.
        let mut store = laid_out(&GenConfig { target_nodes: 64_500, seed: 2, ..Default::default() });
        let page = store.doc().pages().first().and_then(|k| store.doc().node(*k)).map(|n| n.id).expect("page");
        let r3 = store.transact(TxMeta::user("instances"), |tx| {
            add_theme(tx)?;
            tx.insert_json(Some(page), None, icon())?;
            tx.insert_json(Some(page), None, button())?;
            for i in 0..2000 {
                tx.insert_json(Some(page), None, json!({"type": "instance", "component": BUTTON, "x": -20000.0 + (i % 50) as f64 * 180.0, "y": (i / 50) as f64 * 60.0, "width": 160, "height": 48}))?;
            }
            Ok(())
        });
        assert!(r3.is_ok(), "open setup");
        let dir = std::env::temp_dir().join(format!("bench-open-instances-{}.canvas", std::process::id()));
        let Ok(bundle) = format::Bundle::create(&dir) else { return };
        let _ = bundle.save(store.doc());
        r.info("open_mb", std::fs::metadata(bundle.document_path()).map_or(0, |m| m.len()) as f64 / 1048576.0);
        drop(store);
        let (mut total, mut expand, mut rss) = (Vec::new(), Vec::new(), 0.0);
        for _ in 0..5 {
            let before = rss_mb();
            let t = Instant::now();
            let bytes = std::fs::read(bundle.document_path()).unwrap_or_default();
            let Ok(data) = format::data_from_json(&bytes) else { return };
            drop(bytes);
            let Ok(doc) = Document::from_data(data) else { return };
            let mut d = LayoutDeriver::new(TextEngine::new(FontDb::new(true)));
            d.prepare(&doc);
            let t2 = Instant::now();
            let mut s = Store::new(doc);
            s.set_deriver(Box::new(d));
            expand.push(ms(t2));
            total.push(ms(t));
            rss = rss_mb() - before;
            r.info("open_virtual_nodes", s.doc().iter().filter(|(_, n)| n.virt.is_some()).count());
            drop(s);
        }
        r.metric("open_50mb_with_instances", stats(total), Some(2000.0), None);
        r.metric("open_of_which_expand_instances", stats(expand), None, None);
        r.info("open_rss_delta_mb", rss);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
