//! The scenarios. Each builds its own data, so any one can run alone.

use std::time::Instant;

use doc::model::{TextAlign, TextSpan, TextStyle};
use doc::testing::{GenConfig, STRESS_ROOT, generate};
use doc::*;
use layout::{LayoutDeriver, LayoutEngine, TaffyLayout};
use serde_json::{Value, json};
use text::{FontDb, TextEngine, TextInput};

use crate::harness::{Report, Scenario, ms, repeat, rss_mb, stats};

pub fn all() -> Vec<Box<dyn Scenario>> {
    vec![Box::new(Ops), Box::new(Files), Box::new(Open), Box::new(Layout), Box::new(Edit), Box::new(Text)]
}

/// Fonts used by the benchmarks: bundled Inter plus system fallback, as in the app.
fn engine() -> TextEngine {
    TextEngine::new(FontDb::new(true))
}

fn find(doc: &Document, name: &str) -> NodeId {
    doc.iter().find(|(_, n)| n.name == name).map(|(_, n)| n.id).expect("named node")
}

/// A store with the layout deriver whose document has been fully laid out once.
/// Returns it with the time the initial layout took.
fn laid_out(cfg: &GenConfig) -> (Store, f64, usize) {
    let doc = Document::from_data(generate(cfg)).unwrap_or_default();
    let mut s = Store::new(doc);
    s.set_deriver(Box::new(LayoutDeriver::new(engine())));
    let t = Instant::now();
    let writes = s.transact_with_deriver(TxMeta::user("layout"), |tx, d| d.map_or(Ok(()), |d| d.derive_all(tx))).map_or(0, |(_, ch)| ch.updated.len());
    (s, ms(t), writes)
}

fn deriver(s: &mut Store) -> Option<&mut LayoutDeriver> {
    s.deriver_mut().and_then(|d| d.as_any_mut().downcast_mut::<LayoutDeriver>())
}

fn children(doc: &Document, id: NodeId) -> Vec<NodeId> {
    doc.get(id).map(|n| n.children.iter().filter_map(|k| doc.node(*k)).map(|c| c.id).collect()).unwrap_or_default()
}

// ---- Stage 2: document ops and files ------------------------------------------------------

struct Ops;

impl Scenario for Ops {
    fn name(&self) -> &'static str {
        "ops"
    }
    fn describe(&self) -> &'static str {
        "Document ops at 100k nodes, no layout (Stage 2)"
    }
    fn run(&self, r: &mut Report) {
        let template = generate(&GenConfig { target_nodes: 100_000, ..Default::default() });
        let mut store = Store::default().with_ids(IdGen::seeded(9));
        let t = Instant::now();
        let _ = store.transact(TxMeta::user("setup"), |tx| {
            tx.set_variables(template.variables.clone())?;
            for (id, meta) in &template.assets {
                tx.put_asset(id.clone(), meta.clone())?;
            }
            Ok(())
        });
        // Pages, then each artboard, then each card, one transaction each.
        for page in &template.pages {
            let mut p = page.clone();
            let boards = std::mem::take(&mut p.children);
            let pid = p.id;
            let _ = store.transact(TxMeta::user("page"), |tx| tx.insert(None, None, p));
            for mut b in boards {
                let cards = std::mem::take(&mut b.children);
                let bid = b.id;
                let _ = store.transact(TxMeta::user("board"), |tx| tx.insert(Some(pid), None, b));
                for c in cards {
                    let _ = store.transact(TxMeta::user("card"), |tx| tx.insert(Some(bid), None, c));
                }
            }
        }
        r.once("build_100k_via_transactions", ms(t), None, None);
        r.info("nodes", store.doc().len());

        let doc = store.doc();
        let rects: Vec<NodeId> = doc.iter().filter(|(_, n)| matches!(n.kind, NodeKind::Rectangle {})).map(|(_, n)| n.id).collect();
        let cards: Vec<NodeId> = doc.iter().filter(|(_, n)| n.name.starts_with("Card")).map(|(_, n)| n.id).collect();
        let boards: Vec<NodeId> = doc.iter().filter(|(_, n)| n.name.starts_with("Screen")).map(|(_, n)| n.id).collect();

        let m = repeat(2000, |i| {
            let _ = store.transact(TxMeta::user("x"), |tx| tx.update(rects[i % rects.len()], PropValue::X(i as f64)));
        });
        r.metric("update_one_prop", m, None, None);
        let props = json!({"opacity": 0.5, "fills": [{"type": "solid", "color": "#ff8800"}]});
        let m = repeat(2000, |i| {
            let _ = store.transact(TxMeta::user("x"), |tx| tx.update_json(rects[i % rects.len()], props.as_object().unwrap_or(&Default::default())));
        });
        r.metric("update_json_two_props", m, None, None);
        let m = repeat(50, |round| {
            let _ = store.transact(TxMeta::user("batch"), |tx| {
                for (j, id) in rects.iter().take(250).enumerate() {
                    tx.update(*id, PropValue::Y((round + j) as f64))?;
                    tx.update(*id, PropValue::Fills(vec![doc::model::Paint::solid(doc::model::Color::rgb(round as u8, j as u8, 0))]))?;
                }
                Ok(())
            });
        });
        r.metric("tx_500_ops", m, None, None);
        r.metric("undo_500_ops", repeat(50, |_| drop(store.undo())), None, None);
        r.metric("redo_500_ops", repeat(50, |_| drop(store.redo())), None, None);
        let m = repeat(1000, |i| {
            let (c, b) = (cards[i * 7 % cards.len()], boards[i % boards.len()]);
            let _ = store.transact(TxMeta::user("mv"), |tx| tx.move_node(c, Some(b), Some(0)));
        });
        r.metric("move_card_between_artboards", m, None, None);
        let m = repeat(500, |i| {
            let _ = store.transact(TxMeta::user("del"), |tx| tx.delete(cards[i * 13 % cards.len()]));
            let _ = store.undo();
        });
        r.metric("delete_card_and_undo", m, None, None);
        let page = store.doc().node(store.doc().pages()[1]).unwrap().id;
        let m = repeat(20, |_| {
            let _ = store.transact(TxMeta::user("copy"), |tx| {
                for b in boards.iter().take(5) {
                    tx.copy(*b, Some(page), None, None)?;
                }
                Ok(())
            });
        });
        r.metric("copy_5_artboards", m, None, None);
    }
}

struct Files;

/// Save and open timings for one generated document, without layout.
fn file_bench(r: &mut Report, label: &str, cfg: &GenConfig, target: Option<f64>) {
    let doc = Document::from_data(generate(cfg)).unwrap_or_default();
    let nodes = doc.len();
    let text = format::to_json(&doc);
    let dir = std::env::temp_dir().join(format!("bench-{label}-{}.canvas", std::process::id()));
    let Ok(bundle) = format::Bundle::create(&dir) else { return };
    let t = Instant::now();
    let _ = bundle.save(&doc);
    r.once(&format!("{label}_save"), ms(t), None, None);
    drop(doc);
    let (mut parse, mut build, mut open) = (Vec::new(), Vec::new(), Vec::new());
    for _ in 0..5 {
        let t = Instant::now();
        let bytes = std::fs::read(bundle.document_path()).unwrap_or_default();
        let Ok(data) = format::data_from_json(&bytes) else { return };
        parse.push(ms(t));
        let t2 = Instant::now();
        let d = Document::from_data(data);
        build.push(ms(t2));
        open.push(ms(t));
        drop(d);
    }
    r.metric(&format!("{label}_open"), stats(open), target, None);
    r.metric(&format!("{label}_read_and_parse"), stats(parse), None, None);
    r.metric(&format!("{label}_build_and_validate"), stats(build), None, None);
    r.info(&format!("{label}_nodes"), nodes);
    r.info(&format!("{label}_mb"), text.len() as f64 / 1048576.0);
    let _ = std::fs::remove_dir_all(&dir);
}

impl Scenario for Files {
    fn name(&self) -> &'static str {
        "files"
    }
    fn describe(&self) -> &'static str {
        "Save and open (parse + build) at 100k nodes, 50 MB and 92 MB, no layout (Stage 2)"
    }
    fn run(&self, r: &mut Report) {
        file_bench(r, "100k", &GenConfig { target_nodes: 100_000, ..Default::default() }, None);
        file_bench(r, "50mb", &GenConfig { target_nodes: 64_500, seed: 2, ..Default::default() }, Some(2000.0));
        file_bench(r, "large", &GenConfig { target_nodes: 115_000, body_words: 40, seed: 3, ..Default::default() }, None);
    }
}

// ---- Stage 3: layout and text -------------------------------------------------------------

struct Open;

impl Scenario for Open {
    fn name(&self) -> &'static str {
        "open"
    }
    fn describe(&self) -> &'static str {
        "Open a laid-out 50 MB document end to end: read, parse, build, shape all text, lay out"
    }
    fn run(&self, r: &mut Report) {
        let cfg = GenConfig { target_nodes: 64_500, seed: 2, ..Default::default() };
        let (store, layout_ms, writes) = laid_out(&cfg);
        r.once("first_full_layout_of_unlaid_64k", layout_ms, None, None);
        r.info("first_layout_writes", writes);
        let dir = std::env::temp_dir().join(format!("bench-open-{}.canvas", std::process::id()));
        let Ok(bundle) = format::Bundle::create(&dir) else { return };
        let _ = bundle.save(store.doc());
        let bytes_len = std::fs::metadata(bundle.document_path()).map_or(0, |m| m.len());
        r.info("mb", bytes_len as f64 / 1048576.0);
        r.info("nodes", store.doc().len());
        drop(store);
        let (mut parse, mut build, mut prep, mut total, mut mismatched) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), 0);
        let mut rss = 0.0;
        for _ in 0..5 {
            let before = rss_mb();
            let t = Instant::now();
            let bytes = std::fs::read(bundle.document_path()).unwrap_or_default();
            let Ok(data) = format::data_from_json(&bytes) else { return };
            drop(bytes);
            parse.push(ms(t));
            let t2 = Instant::now();
            let Ok(doc) = Document::from_data(data) else { return };
            build.push(ms(t2));
            let t3 = Instant::now();
            let mut d = LayoutDeriver::new(engine());
            mismatched = d.prepare(&doc);
            prep.push(ms(t3));
            total.push(ms(t));
            rss = rss_mb() - before;
            let stats = d.text().stats();
            r.info("text_layouts_cached", stats.entries);
            r.info("text_sizes_cached", stats.metrics);
            drop((doc, d));
        }
        r.metric("open_end_to_end", stats(total), Some(2000.0), None);
        r.metric("read_and_parse", stats(parse), None, None);
        r.metric("build_and_validate", stats(build), None, None);
        r.metric("shape_all_text_parallel_and_layout", stats(prep), None, None);
        r.info("stored_values_differing_from_layout", mismatched);
        r.info("rss_delta_after_open_mb", rss);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

struct Layout;

impl Scenario for Layout {
    fn name(&self) -> &'static str {
        "layout"
    }
    fn describe(&self) -> &'static str {
        "Full layout of a 10k-node auto-layout tree (mirror build + Taffy + text measure)"
    }
    fn run(&self, r: &mut Report) {
        // Laid out once, as a saved document would be; the runs below recompute everything.
        let (store, _, _) = laid_out(&GenConfig { target_nodes: 10, auto_layout_heavy: true, ..Default::default() });
        let doc = store.into_doc();
        let root = find(&doc, STRESS_ROOT);
        let mut out = Vec::new();
        let m = repeat(10, |_| {
            let mut text = engine();
            let mut flow = TaffyLayout::new();
            out.clear();
            flow.invalidate_all(&doc);
            flow.compute(&doc, &mut text, &mut out);
        });
        r.metric("full_layout_10k_cold_text", m, Some(50.0), None);
        let mut text = engine();
        let m = repeat(30, |_| {
            let mut flow = TaffyLayout::new();
            out.clear();
            flow.invalidate_all(&doc);
            flow.compute(&doc, &mut text, &mut out);
        });
        r.metric("full_layout_10k_warm_text", m, Some(50.0), Some(15.0));
        let (mut build, mut compute) = (Vec::new(), Vec::new());
        for _ in 0..30 {
            let mut flow = TaffyLayout::new();
            out.clear();
            let t = Instant::now();
            flow.invalidate_all(&doc);
            build.push(ms(t));
            let t = Instant::now();
            flow.compute(&doc, &mut text, &mut out);
            compute.push(ms(t));
        }
        r.metric("of_which_mirror_build", stats(build), None, None);
        r.metric("of_which_compute_and_write_back", stats(compute), None, None);
        r.info("writes_on_relayout", out.len());
        let mut flow = TaffyLayout::new();
        flow.invalidate_all(&doc);
        r.info("mirror_nodes", flow.mirror_len(root).unwrap_or(0));
        r.info("text_nodes", doc.iter().filter(|(_, n)| matches!(n.kind, NodeKind::Text { .. })).count());
    }
}

struct Edit;

impl Scenario for Edit {
    fn name(&self) -> &'static str {
        "edit"
    }
    fn describe(&self) -> &'static str {
        "Edits end to end (transaction + layout + write-back) in a 100k-node document with a 10k-node auto-layout tree"
    }
    fn run(&self, r: &mut Report) {
        let (mut s, layout_ms, writes) = laid_out(&GenConfig { target_nodes: 100_000, auto_layout_heavy: true, ..Default::default() });
        r.once("first_full_layout_of_unlaid_110k", layout_ms, None, None);
        r.info("nodes", s.doc().len());
        r.info("first_layout_writes", writes);
        let root = find(s.doc(), STRESS_ROOT);
        let sections = children(s.doc(), root);
        let card = |s: &Store, i: usize| children(s.doc(), sections[(i * 37) % sections.len()]).get(i % 10).copied().expect("card");

        // Typing into a card body: append a character per keystroke, coalesced like the app.
        let body = children(s.doc(), card(&s, 3))[2];
        let mut text = String::from("Typing ");
        let mut passes = Vec::new();
        let m = repeat(300, |i| {
            text.push(if i % 6 == 5 { ' ' } else { 'a' });
            let props = json!({"spans": [{"text": text}]});
            let _ = s.transact(TxMeta::user("type").coalesce("typing"), |tx| tx.update_json(body, props.as_object().unwrap_or(&Default::default())));
            passes.push(deriver(&mut s).map_or(0, |d| d.stats().last_passes));
        });
        r.metric("keystroke_in_10k_tree", m, None, Some(2.0));
        r.info("keystroke_layout_passes_max", passes.iter().max().copied().unwrap_or(0));

        // Changing a property layout doesn't read: the deriver's fixed cost.
        let rect = s.doc().iter().find(|(_, n)| matches!(n.kind, NodeKind::Rectangle {})).map(|(_, n)| n.id).expect("a rectangle");
        let m = repeat(1000, |i| {
            let _ = s.transact(TxMeta::user("op"), |tx| tx.update(rect, PropValue::Opacity(0.5 + (i % 2) as f64 / 4.0)));
        });
        r.metric("non_layout_prop_update", m, None, None);

        // A fixed-size leaf grows: its card, section and everything below shift.
        let icon = children(s.doc(), children(s.doc(), card(&s, 5))[3])[0];
        let m = repeat(100, |i| {
            let _ = s.transact(TxMeta::user("w"), |tx| tx.update(icon, PropValue::Width(20.0 + (i % 4) as f64)));
        });
        r.metric("resize_leaf_in_10k_tree", m, None, Some(2.0));

        // Root width: every wrapping row and every text re-wraps.
        let m = repeat(20, |i| {
            let _ = s.transact(TxMeta::user("root"), |tx| tx.update(root, PropValue::Width(if i % 2 == 0 { 1280.0 } else { 1440.0 })));
        });
        r.metric("resize_root_of_10k_tree", m, Some(50.0), Some(15.0));
        r.metric("undo_root_resize", repeat(10, |_| drop(s.undo())), Some(50.0), Some(15.0));
        r.metric("redo_root_resize", repeat(10, |_| drop(s.redo())), Some(50.0), Some(15.0));

        // Structural: copy a card into a section, then delete it (mirror rebuild).
        let section = sections[0];
        let src = card(&s, 0);
        let mut copies = Vec::new();
        let m = repeat(30, |_| {
            if let Ok((id, _)) = s.transact(TxMeta::user("ins"), |tx| tx.copy(src, Some(section), Some(0), None)) {
                copies.push(id);
            }
        });
        r.metric("insert_card_in_10k_tree", m, Some(50.0), None);
        let m = repeat(copies.len(), |i| {
            let _ = s.transact(TxMeta::user("del"), |tx| tx.delete(copies[i]));
        });
        r.metric("delete_card_in_10k_tree", m, Some(50.0), None);

        // Constraints: a plain frame with 200 constrained children.
        let page = s.doc().node(s.doc().pages()[1]).unwrap().id;
        let (hs, vs) = (["left", "right", "leftRight", "center", "scale"], ["top", "bottom", "topBottom", "center", "scale"]);
        let kids: Vec<Value> = (0..200)
            .map(|i| {
                let (h, v) = (hs[i % 5], vs[i / 5 % 5]);
                json!({"type": "rectangle", "x": (i % 20) * 40, "y": (i / 20) * 40, "width": 30, "height": 30, "constraints": {"h": h, "v": v}})
            })
            .collect();
        let frame = s.transact(TxMeta::user("frame"), |tx| tx.insert_json(Some(page), None, json!({"type": "frame", "width": 800, "height": 400, "children": kids}))).unwrap().0;
        let m = repeat(100, |i| {
            let _ = s.transact(TxMeta::user("resize"), |tx| tx.update(frame, PropValue::Width(800.0 + (i % 10) as f64 * 10.0)));
        });
        r.metric("constraint_reflow_200_children", m, None, Some(2.0));
    }
}

struct Text;

impl Scenario for Text {
    fn name(&self) -> &'static str {
        "text"
    }
    fn describe(&self) -> &'static str {
        "Shape 10k labels cold: sequential vs parallel"
    }
    fn run(&self, r: &mut Report) {
        let style = TextStyle::default();
        let spans: Vec<[TextSpan; 1]> = (0..10_000).map(|i| [TextSpan { text: format!("Label {i} design canvas"), ..Default::default() }]).collect();
        let inputs: Vec<(TextInput<'_>, Option<f32>)> = spans.iter().map(|s| (TextInput { spans: s, style: &style, align: TextAlign::Left }, Some(160.0))).collect();
        let mut e = engine();
        let m = repeat(5, |_| {
            e.clear_cache();
            for (i, w) in &inputs {
                e.shape(*i, *w);
            }
        });
        r.metric("shape_10k_labels_sequential", m, None, None);
        let m = repeat(5, |_| {
            e.clear_cache();
            e.shape_many(&inputs);
        });
        r.metric("shape_10k_labels_parallel", m, None, None);
        let m = repeat(5, |_| {
            e.shape_many(&inputs);
        });
        r.metric("shape_10k_labels_all_cached", m, None, None);
        r.info("threads", std::thread::available_parallelism().map_or(0, |n| n.get()));
    }
}
