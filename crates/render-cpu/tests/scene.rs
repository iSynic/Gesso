//! Incremental rendering equals a fresh build: random edits applied to one long-lived `Scene`
//! (fragment lists, index and caches updated from each change set) render pixel-identical to a
//! `Scene` built from scratch, at two zoom bands and with fragment splitting forced on or off.
//! Undoing every edit restores the original pixels. Case count: `PROPTEST_CASES` (default 48).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;

use doc::testing::{random_edit, render_test_page};
use doc::*;
use kurbo::Affine;
use layout::LayoutDeriver;
use proptest::prelude::*;
use render::{NoAssets, Scene};
use text::{FontDb, TextEngine};

/// (camera, width, height): 1:1 (near band) and 0.2 (mid band, greeked text).
const VIEWS: [(f64, u32, u32); 2] = [(1.0, 360, 240), (0.2, 96, 64)];

fn start() -> (Store, NodeId) {
    let mut s = Store::default().with_ids(IdGen::seeded(5));
    s.set_deriver(Box::new(LayoutDeriver::new(TextEngine::new(FontDb::bundled_only()))));
    let page = s.transact(TxMeta::user("page"), render_test_page).unwrap().0;
    (s, page)
}

fn scene(doc: &Document, page: NodeId, split: Option<usize>) -> Scene {
    let mut sc = Scene::new(doc, page, FontDb::bundled_only(), Arc::new(NoAssets));
    if let Some(n) = split {
        sc.set_split_threshold(n);
    }
    sc
}

fn shots(sc: &mut Scene, doc: &Document) -> Vec<Vec<u8>> {
    VIEWS.iter().map(|&(z, w, h)| common::render_frame(&sc.frame(doc, Affine::scale(z), w, h, None), 0)).collect()
}

fn check(inc: &mut Scene, doc: &Document, page: NodeId, split: Option<usize>, when: &str) -> Result<(), TestCaseError> {
    let a = shots(inc, doc);
    let mut fresh = scene(doc, page, split);
    let b = shots(&mut fresh, doc);
    for (i, (x, y)) in a.iter().zip(&b).enumerate() {
        if x != y {
            let d = common::diff(x, y, 0);
            let (z, w, h) = VIEWS[i];
            let why = explain(&inc.frame(doc, Affine::scale(z), w, h, None), &fresh.frame(doc, Affine::scale(z), w, h, None));
            return Err(TestCaseError::fail(format!("{when}, view {i}: {} pixels differ, max {}
{why}", d.over, d.max)));
        }
    }
    Ok(())
}

/// The first frame item that differs between an incremental and a fresh frame.
fn explain(a: &render::Frame, b: &render::Frame) -> String {
    let short = |l: &render::DisplayList| format!("{:?}", l.cmds).chars().take(400).collect::<String>();
    for (i, (x, y)) in a.items.iter().zip(&b.items).enumerate() {
        if format!("{:?}", x.list.cmds) != format!("{:?}", y.list.cmds) || x.xf != y.xf || x.clips.len() != y.clips.len() {
            return format!("item {i}: xf {:?} vs {:?}, clips {} vs {}
  incremental {}
  fresh       {}", x.xf, y.xf, x.clips.len(), y.clips.len(), short(&x.list), short(&y.list));
        }
    }
    format!("{} items vs {}", a.items.len(), b.items.len())
}

fn edit() -> impl Strategy<Value = (u8, u8, u8, u16)> {
    (any::<u8>(), any::<u8>(), any::<u8>(), any::<u16>())
}

fn config() -> ProptestConfig {
    let cases = std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(48);
    ProptestConfig { cases, ..ProptestConfig::default() }
}

proptest! {
    #![proptest_config(config())]
    #[test]
    fn incremental_equals_fresh(edits in prop::collection::vec(edit(), 1..20), small_split in any::<bool>()) {
        // A threshold of 2 splits every eligible frame and group into Pre/children/Post fragments.
        let split = small_split.then_some(2);
        let (mut s, page) = start();
        let mut inc = scene(s.doc(), page, split);
        let original = shots(&mut inc, s.doc());
        let mut done = 0;
        for (i, e) in edits.into_iter().enumerate() {
            let Ok((_, ch)) = s.transact(TxMeta::user("edit"), |tx| random_edit(tx, page, e)) else { continue };
            done += usize::from(!ch.is_empty());
            inc.apply(s.doc(), &ch);
            check(&mut inc, s.doc(), page, split, &format!("after edit {i} {e:?}"))?;
        }
        for i in 0..done {
            let Some(ch) = s.undo().unwrap() else { break };
            inc.apply(s.doc(), &ch);
            check(&mut inc, s.doc(), page, split, &format!("after undo {i}"))?;
        }
        prop_assert!(shots(&mut inc, s.doc()) == original, "undo did not restore the original pixels");
    }
}

/// Found by the property test: flip a frame, move a rectangle into it (its group resizes), then
/// undo both before the next frame. The index used to keep the rectangle under the frame.
#[test]
fn flip_move_undo_with_splits() {
    let split = Some(2);
    let (mut s, page) = start();
    let mut inc = scene(s.doc(), page, split);
    for e in [(167, 110, 0, 0), (26, 248, 22, 0)] {
        let ch = s.transact(TxMeta::user("edit"), |tx| random_edit(tx, page, e)).unwrap().1;
        inc.apply(s.doc(), &ch);
        check(&mut inc, s.doc(), page, split, &format!("after edit {e:?}")).unwrap();
    }
    // Both undos before the next frame.
    for _ in 0..2 {
        let ch = s.undo().unwrap().unwrap();
        inc.apply(s.doc(), &ch);
    }
    check(&mut inc, s.doc(), page, split, "after undoing both").unwrap();
}

/// Found by the property test: a shadow makes a frame unsplittable; undoing it must split it again.
#[test]
fn frame_splits_again_after_its_effect_is_undone() {
    let split = Some(2);
    let (mut s, page) = start();
    let mut inc = scene(s.doc(), page, split);
    for e in [(26, 217, 47, 0), (94, 0, 0, 0), (8, 27, 0, 0)] {
        let ch = s.transact(TxMeta::user("edit"), |tx| random_edit(tx, page, e)).unwrap().1;
        inc.apply(s.doc(), &ch);
        check(&mut inc, s.doc(), page, split, &format!("after edit {e:?}")).unwrap();
    }
    let ch = s.undo().unwrap().unwrap();
    inc.apply(s.doc(), &ch);
    check(&mut inc, s.doc(), page, split, "after undo").unwrap();
    let mut fresh = scene(s.doc(), page, split);
    shots(&mut fresh, s.doc());
    assert_eq!(inc.fragment_count(), fresh.fragment_count());
}

/// Found by the property test: a clipping frame shrinks past a rotated child whose cached list
/// is still current; culling must apply the frame's clip as a fresh scene does.
#[test]
fn shrunk_clip_culls_a_cached_fragment() {
    let split = Some(2);
    let (mut s, page) = start();
    let mut inc = scene(s.doc(), page, split);
    for e in [(126, 94, 0, 42593), (212, 24, 0, 13057), (16, 10, 0, 55455)] {
        let ch = s.transact(TxMeta::user("edit"), |tx| random_edit(tx, page, e)).unwrap().1;
        inc.apply(s.doc(), &ch);
        check(&mut inc, s.doc(), page, split, &format!("after edit {e:?}")).unwrap();
    }
}

/// A page of `n` frames, each holding 10 px text.
fn small_text_page(n: usize) -> (Store, NodeId) {
    let mut s = Store::default().with_ids(IdGen::seeded(9));
    s.set_deriver(Box::new(LayoutDeriver::new(TextEngine::new(FontDb::bundled_only()))));
    let page = s
        .transact(TxMeta::user("page"), |tx| {
            let page = tx.insert_json(None, None, serde_json::json!({"type": "page"}))?;
            for i in 0..n {
                let text = serde_json::json!({"type": "text", "x": 8, "y": 8, "spans": [{"text": "Caption"}], "style": {"fontFamily": "Inter", "fontSize": 10, "fontWeight": 400, "italic": false, "letterSpacing": 0, "decoration": "none", "color": "#000000"}});
                tx.insert_json(Some(page), None, serde_json::json!({"type": "frame", "x": i as f64 * 120.0, "y": 0, "width": 100, "height": 40, "children": [text]}))?;
            }
            Ok(page)
        })
        .unwrap()
        .0;
    (s, page)
}

/// Level of detail is decided per zoom band; from 100 % on, text of any readable size is drawn
/// as glyphs, never as bars.
#[test]
fn small_text_is_not_greeked_at_full_zoom() {
    let (s, page) = small_text_page(1);
    let mut sc = scene(s.doc(), page, None);
    let greeked = |sc: &mut Scene, z: f64| sc.frame(s.doc(), Affine::scale(z), 400, 200, None).items.iter().map(|i| i.list.greeked).sum::<u32>();
    for z in [1.0, 1.5, 4.0] {
        assert_eq!(greeked(&mut sc, z), 0, "zoom {z}");
    }
    // 2.5 px tall at a quarter: bars.
    assert_eq!(greeked(&mut sc, 0.3), 1);
}

/// With an encode budget, a band change draws the old band's lists until the new ones are built
/// over the following frames.
#[test]
fn a_budgeted_band_change_completes_over_frames() {
    let (s, page) = small_text_page(40);
    let mut sc = scene(s.doc(), page, None);
    sc.frame(s.doc(), Affine::scale(0.3), 1600, 100, None);
    let mut frames = 0;
    loop {
        let fr = sc.frame(s.doc(), Affine::scale(1.0), 5000, 100, Some(std::time::Duration::ZERO));
        assert_eq!(fr.items.len(), 40, "every fragment draws, stale or not");
        frames += 1;
        if sc.stats.stale == 0 {
            break;
        }
        assert!(frames <= 40, "the band change never completed");
    }
    assert!(frames > 1, "a zero budget should defer some fragments");
}
