//! Stage 5 editor scenarios: the canvas app's CPU work outside rendering (hit-testing, marquee,
//! snapping, gesture frames, the layers tree, the autosave journal), on one page holding the
//! whole generated document.

use std::time::Instant;

use doc::testing::GenConfig;
use doc::*;
use editor::snap::{self, Moving, Targets};
use editor::transform::{Mods, MoveGesture};
use editor::autosave::Autosave;
use editor::{Camera, Session};
use kurbo::{Point, Vec2};
use serde_json::json;
use text::FontDb;

use crate::harness::{Report, Scenario, ms, repeat, stats};
use crate::render::laid_out;

pub fn all() -> Vec<Box<dyn Scenario>> {
    vec![Box::new(Editor)]
}

/// The generated document with every page's screens moved onto the first content page (stacked
/// below each other), so one page holds all ~100k nodes.
pub(crate) fn one_page(cfg: &GenConfig) -> Document {
    let mut s = laid_out(cfg);
    let doc = s.doc();
    let pages: Vec<NodeId> = doc.pages().iter().filter_map(|&k| doc.node(k)).filter(|p| p.children.iter().filter_map(|&c| doc.node(c)).any(|c| c.name.starts_with("Screen"))).map(|p| p.id).collect();
    let Some((&target, rest)) = pages.split_first() else { return s.into_doc() };
    let screens = |doc: &Document, page: NodeId| -> Vec<(NodeId, f64)> { doc.get(page).map(|p| p.children.iter().filter_map(|&c| doc.node(c)).map(|c| (c.id, c.props.y)).collect()).unwrap_or_default() };
    let height = screens(s.doc(), target).iter().map(|(id, y)| y + s.doc().get(*id).map_or(0.0, |n| n.props.height)).fold(0.0, f64::max) + 2000.0;
    let moves: Vec<(usize, Vec<(NodeId, f64)>)> = rest.iter().enumerate().map(|(i, &p)| (i + 1, screens(s.doc(), p))).collect();
    let r = s.transact(TxMeta::user("merge pages"), |tx| {
        for (i, list) in &moves {
            for (id, y) in list {
                tx.move_node(*id, Some(target), None)?;
                tx.update_json(*id, json!({ "y": y + height * *i as f64 }).as_object().expect("object"))?;
            }
        }
        Ok(())
    });
    assert!(r.is_ok(), "merge pages");
    s.into_doc()
}

struct Editor;

impl Scenario for Editor {
    fn name(&self) -> &'static str {
        "editor"
    }
    fn describe(&self) -> &'static str {
        "Canvas app CPU work at 100k nodes on one page: hit-test, marquee, snapping, move frames, layers tree, autosave journal"
    }
    fn run(&self, r: &mut Report) {
        let t = Instant::now();
        let mut s = Session::new(one_page(&GenConfig { target_nodes: 100_000, ..Default::default() }), None, FontDb::new(true)).expect("session");
        r.info("session_open_ms", ms(t));
        let page = s.doc().pages().iter().filter_map(|&k| s.doc().node(k)).max_by_key(|p| p.children.len()).map(|p| p.id).expect("page");
        s.set_page(page);
        let page_nodes = s.doc().key(page).map_or(0, |k| s.doc().descendants(k).len());
        r.info("page_nodes", page_nodes);
        s.viewport = Vec2::new(1920.0, 1080.0);
        let b = s.scene.index.page_bounds(page).expect("page bounds");
        let at = |i: usize| Point::new(b.x0 + (i * 977) as f64 % b.width(), b.y0 + (i * 613) as f64 % b.height());

        // Hit-testing at 100 %: index query, then exact tests and scope climbing.
        s.camera = Camera { zoom: 1.0, origin: b.origin() };
        let mut found = 0;
        r.metric("hit_test_click", repeat(2000, |i| found += usize::from(s.pick(at(i), false).is_some())), Some(1.0), Some(0.2));
        r.metric("hit_test_deep", repeat(2000, |i| found += usize::from(s.pick(at(i), true).is_some())), Some(1.0), Some(0.2));
        r.info("hit_test_found", found);

        // Marquee: the whole page (selects the screens), and a viewport-sized one inside a screen.
        let mut n = 0;
        r.metric("marquee_whole_page", repeat(50, |_| n = s.marquee(Point::new(b.x0 - 10.0, b.y0 - 10.0), Point::new(b.x1 + 10.0, b.y1 + 10.0), false).len()), None, None);
        r.info("marquee_whole_page_selected", n);
        let screen = s.doc().iter().find(|(_, n)| n.name == "Screen 1").map(|(_, n)| n.id).expect("screen");
        let sb = s.scene.index.bounds(screen).expect("screen bounds");
        s.select(vec![screen]);
        let _ = s.double_click(sb.center());
        r.metric("marquee_in_screen", repeat(200, |_| n = s.marquee(Point::new(sb.x0 + 5.0, sb.y0 + 5.0), Point::new(sb.x1 - 5.0, sb.y1 - 5.0), false).len()), None, None);
        r.info("marquee_in_screen_selected", n);
        s.clear_selection();

        // Snapping: candidate collection (once per gesture) and the per-frame snap.
        s.camera = Camera { zoom: 1.0, origin: sb.origin() - Vec2::new(200.0, 100.0) };
        let view = s.view();
        let exclude = [screen].into_iter().collect();
        let mut targets = Targets::default();
        r.metric("snap_collect_100pct", repeat(200, |_| targets = Targets::collect(s.doc(), &s.scene.index, page, page, &exclude, view)), None, Some(1.0));
        let m = repeat(2000, |i| {
            let r = sb + Vec2::new((i % 300) as f64 * 1.3, (i % 170) as f64 * 0.7);
            std::hint::black_box(snap::snap_rect(r, &targets, 6.0, true, Moving::default()));
        });
        r.metric("snap_per_frame", m, None, Some(1.0));
        r.metric("distance_measures", repeat(2000, |i| drop(std::hint::black_box(snap::measures(sb + Vec2::new(i as f64 % 50.0, 0.0), &targets)))), None, None);

        // Autosave journal: starting one on an untitled (or recovered) session writes a base
        // snapshot once; each batch afterwards costs the main thread only its own ops.
        let dir = std::env::temp_dir().join(format!("bench-autosave-{}", std::process::id()));
        let t = Instant::now();
        let mut autosave = Autosave::new(dir.clone(), &mut s);
        r.info("autosave_begin_with_base_snapshot_ms", ms(t));

        // Move frames (editor side: snap, coalesced transaction, scene/index updates).
        s.select(vec![screen]);
        let start = sb.center();
        let mut g = MoveGesture::begin(&mut s, start, false).expect("begin").expect("gesture");
        let mut frames = Vec::new();
        for i in 1..=120 {
            let t = Instant::now();
            let _ = g.update(&mut s, start + Vec2::new(i as f64 * 2.5, i as f64 * 1.25), Mods::default());
            frames.push(ms(t));
        }
        r.metric("move_frame_screen", stats(frames), None, Some(4.0));
        let t = Instant::now();
        let _ = g.finish(&mut s);
        r.once("move_drop_screen", ms(t), None, None);
        autosave.snapshot(&mut s);
        r.once("autosave_batch_after_drag", autosave.last.clone_ms, None, Some(8.0));
        r.info("autosave_batch_after_drag_ops", json!([autosave.last.ops, autosave.last.compacted]));
        // Deleting a screen and undoing it journals the whole subtree once (the re-insert).
        let _ = s.undo();
        s.select(vec![screen]);
        let _ = editor::commands::execute(&mut s, editor::Command::Delete);
        let _ = s.undo();
        autosave.snapshot(&mut s);
        r.once("autosave_batch_delete_undo_screen", autosave.last.clone_ms, None, Some(8.0));
        autosave.flush();
        let journal_bytes: u64 = std::fs::read_dir(&dir).map(|d| d.flatten().filter(|e| e.path().to_string_lossy().ends_with(".jsonl")).filter_map(|e| e.metadata().ok()).map(|m| m.len()).sum()).unwrap_or(0);
        r.info("autosave_journal_kb", journal_bytes as f64 / 1024.0);
        autosave.discard();
        autosave.flush();
        let _ = std::fs::remove_dir_all(&dir);

        // Layers tree: rebuild with everything expanded (worst case), and after a rename.
        let containers: Vec<NodeId> = s.doc().key(page).map(|pk| s.doc().descendants(pk).into_iter().filter_map(|k| s.doc().node(k)).filter(|n| !n.children.is_empty()).map(|n| n.id).collect()).unwrap_or_default();
        for id in &containers {
            s.layers.set_expanded(*id, true);
        }
        let mut rows = 0;
        let m = repeat(10, |i| {
            s.layers.set_expanded(containers[0], i % 2 == 0);
            rows = s.layer_rows().1.len();
        });
        r.metric("layers_rebuild_all_expanded", m, None, None);
        r.info("layers_rows", rows);
        for id in &containers {
            s.layers.set_expanded(*id, false);
        }
        let m = repeat(10, |i| {
            s.layers.set_expanded(containers[0], i % 2 == 0);
            rows = s.layer_rows().1.len();
        });
        r.metric("layers_rebuild_collapsed", m, None, None);
        r.info("layers_rows_collapsed", rows);

        // The snapshot route the journal replaced: `to_data` on the main thread per autosave.
        r.metric("snapshot_to_data_100k", repeat(10, |_| drop(std::hint::black_box(s.doc().to_data()))), None, None);
        drop(s);
        let big = Document::from_data(doc::testing::generate(&GenConfig { target_nodes: 64_500, seed: 2, ..Default::default() })).unwrap_or_default();
        let bytes = format::data_to_json(&big.to_data()).len();
        r.info("doc_50mb_json_mb", bytes as f64 / 1048576.0);
        r.metric("snapshot_to_data_50mb", repeat(10, |_| drop(std::hint::black_box(big.to_data()))), None, None);
        let data = big.to_data();
        r.metric("snapshot_serialise_50mb", repeat(3, |_| drop(std::hint::black_box(format::data_to_json(&data)))), None, None);
    }
}
