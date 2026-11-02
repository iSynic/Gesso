//! Index correctness: bounds include rotation, clipping and effect outsets; incremental updates
//! after random edits (and undo/redo) equal a fresh build. Case count: `PROPTEST_CASES`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use doc::testing::{random_edit, render_test_page};
use doc::*;
use index::SpatialIndex;
use kurbo::Rect;
use proptest::prelude::*;

fn start() -> (Store, NodeId) {
    let mut s = Store::default().with_ids(IdGen::seeded(7));
    // Layout re-derives group bounds, so a move also updates the old and new ancestors.
    s.set_deriver(Box::new(layout::LayoutDeriver::new(text::TextEngine::new(text::FontDb::bundled_only()))));
    let page = s.transact(TxMeta::user("page"), render_test_page).unwrap().0;
    (s, page)
}

fn by_name(s: &Store, name: &str) -> NodeId {
    s.doc().iter().find(|(_, n)| n.name == name).unwrap().1.id
}

fn find(s: &Store, pred: impl Fn(&Node) -> bool) -> NodeId {
    s.doc().iter().find(|(_, n)| pred(n)).unwrap().1.id
}

fn close(a: Rect, b: Rect) -> bool {
    (a.x0 - b.x0).abs() < 1e-6 && (a.y0 - b.y0).abs() < 1e-6 && (a.x1 - b.x1).abs() < 1e-6 && (a.y1 - b.y1).abs() < 1e-6
}

fn same(a: &SpatialIndex, b: &SpatialIndex) -> Result<(), String> {
    let (x, y) = (a.snapshot(), b.snapshot());
    if x.len() != y.len() {
        return Err(format!("{} entries vs {}", x.len(), y.len()));
    }
    for (p, q) in x.iter().zip(&y) {
        let ok = p.0 == q.0
            && match (p.1, q.1) {
                (Some(r), Some(t)) => close(r, t),
                (None, None) => true,
                _ => false,
            }
            && p.2.as_coeffs().iter().zip(q.2.as_coeffs()).all(|(u, v)| (u - v).abs() < 1e-9);
        if !ok {
            return Err(format!("{p:?} vs {q:?}"));
        }
    }
    if a.len() != b.len() {
        return Err(format!("tree sizes {} vs {}", a.len(), b.len()));
    }
    Ok(())
}

#[test]
fn bounds_cover_rotation_clip_and_effects() {
    let (s, page) = start();
    let idx = SpatialIndex::build(s.doc());
    // Plain rectangle at frame (10,10) + (10,10).
    let r = find(&s, |n| matches!(n.kind, NodeKind::Rectangle {}) && n.props.radius != Default::default());
    assert!(close(idx.bounds(r).unwrap(), Rect::new(20.0, 20.0, 80.0, 60.0)));
    // Rotated 30°: a 80×30 box rotated about its centre, clipped by the frame (10..210, 10..170).
    let rot = find(&s, |n| n.props.rotation == 30.0);
    let b = idx.bounds(rot).unwrap();
    let (c, h) = ((10.0 + 150.0 + 40.0, 10.0 + 100.0 + 15.0), (40.0 * 30f64.to_radians().cos() + 15.0 * 30f64.to_radians().sin(), 40.0 * 30f64.to_radians().sin() + 15.0 * 30f64.to_radians().cos()));
    assert!(close(b, Rect::new(c.0 - h.0, c.1 - h.1, 210.0, c.1 + h.1)), "{b:?}");
    // Star inside a non-clipping frame pokes out of it, but is still clipped by the outer frame.
    let star = find(&s, |n| matches!(n.kind, NodeKind::Polygon { .. }));
    assert!(close(idx.bounds(star).unwrap(), Rect::new(90.0, 130.0, 150.0, 170.0)));
    // Drop shadow: blur 8 → σ 4 → 12 px reach, offset (4,4).
    let sh = find(&s, |n| !n.props.effects.is_empty());
    assert!(close(idx.bounds(sh).unwrap(), Rect::new(240.0 - 8.0, 20.0 - 8.0, 280.0 + 16.0, 60.0 + 16.0)));
    // Queries.
    let hits: Vec<NodeId> = idx.query(page, Rect::new(21.0, 21.0, 22.0, 22.0)).collect();
    assert!(hits.contains(&r));
    assert!(!hits.contains(&sh));
    let _ = by_name;
}

#[test]
fn hidden_and_fully_clipped_nodes_are_not_indexed() {
    let (mut s, page) = start();
    let r = find(&s, |n| matches!(n.kind, NodeKind::Ellipse { .. }));
    let changes = s.transact(TxMeta::user("hide"), |tx| tx.update_json(r, serde_json::json!({"x": 500}).as_object().unwrap())).unwrap().1;
    let mut idx = SpatialIndex::build(s.doc());
    assert!(idx.bounds(r).is_none(), "moved outside the clipping frame");
    let frame = find(&s, |n| matches!(n.kind, NodeKind::Frame { clip: true, .. }));
    let ch = s.transact(TxMeta::user("hide"), |tx| tx.update_json(frame, serde_json::json!({"visible": false}).as_object().unwrap())).unwrap().1;
    idx.apply(s.doc(), &ch);
    assert_eq!(idx.query(page, Rect::new(0.0, 0.0, 220.0, 180.0)).count(), 0);
    same(&idx, &SpatialIndex::build(s.doc())).unwrap();
    let _ = changes;
}

fn edit() -> impl Strategy<Value = (u8, u8, u8, u16)> {
    (any::<u8>(), any::<u8>(), any::<u8>(), any::<u16>())
}

proptest! {
    #[test]
    fn incremental_equals_fresh(edits in prop::collection::vec(edit(), 1..25)) {
        let (mut s, page) = start();
        let mut idx = SpatialIndex::build(s.doc());
        let mut steps = 0;
        for e in edits {
            let Ok((_, ch)) = s.transact(TxMeta::user("edit"), |tx| random_edit(tx, page, e)) else { continue };
            idx.apply(s.doc(), &ch);
            steps += usize::from(!ch.is_empty());
            if let Err(m) = same(&idx, &SpatialIndex::build(s.doc())) { prop_assert!(false, "after edit {e:?}: {m}"); }
        }
        // Undo and redo without checking in between, then check: errors must not hide in
        // entries that a later resync trips over.
        for _ in 0..steps {
            let Some(ch) = s.undo().unwrap() else { break };
            idx.apply(s.doc(), &ch);
        }
        if let Err(m) = same(&idx, &SpatialIndex::build(s.doc())) { prop_assert!(false, "after undo: {m}"); }
        while let Some(ch) = s.redo().unwrap() {
            idx.apply(s.doc(), &ch);
        }
        if let Err(m) = same(&idx, &SpatialIndex::build(s.doc())) { prop_assert!(false, "after redo: {m}"); }
    }
}

/// Moving a big subtree reloads the page's R-tree in one bulk load instead of updating it item
/// by item; the result is the same as a fresh build, before and after undo.
#[test]
fn large_moves_reload_the_tree() {
    let (mut s, page) = start();
    let frame = s
        .transact(TxMeta::user("big"), |tx| {
            let kids: Vec<_> = (0..1500).map(|i| serde_json::json!({"type": "rectangle", "x": (i % 50) * 12, "y": (i / 50) * 12, "width": 10, "height": 10})).collect();
            tx.insert_json(Some(page), None, serde_json::json!({"type": "frame", "x": 2000, "y": 0, "width": 600, "height": 360, "children": kids}))
        })
        .unwrap()
        .0;
    let mut idx = SpatialIndex::build(s.doc());
    let everything = Rect::new(-1e6, -1e6, 1e6, 1e6);
    let hits = |i: &SpatialIndex| {
        let mut v: Vec<NodeId> = i.query(page, everything).collect();
        v.sort();
        v
    };
    let (_, ch) = s.transact(TxMeta::user("move"), |tx| {
        tx.update(frame, PropValue::X(2500.0))?;
        tx.update(frame, PropValue::Y(300.0))
    }).unwrap();
    idx.apply(s.doc(), &ch);
    let fresh = SpatialIndex::build(s.doc());
    same(&idx, &fresh).unwrap();
    assert_eq!(hits(&idx), hits(&fresh));
    assert_eq!(idx.query(page, Rect::new(2000.0, 0.0, 2100.0, 100.0)).count(), 0, "nothing left at the old place");
    let ch = s.undo().unwrap().unwrap();
    idx.apply(s.doc(), &ch);
    let fresh = SpatialIndex::build(s.doc());
    same(&idx, &fresh).unwrap();
    assert_eq!(hits(&idx), hits(&fresh));
}

type Step = dyn Fn(&mut Tx<'_>) -> Result<(), Error>;

/// The top-down refresh's cases, each checked against a fresh build: a clipping frame resized
/// (children keep their place, their clip changes), rotated (children rebuilt), moved together
/// with a change deep inside, and hidden.
#[test]
fn refresh_cases_equal_fresh() {
    let (mut s, page) = start();
    let (outer, inner, leaf) = s
        .transact(TxMeta::user("tree"), |tx| {
            let leaf = serde_json::json!({"type": "rectangle", "name": "leaf", "x": 5, "y": 5, "width": 30, "height": 30});
            let inner = serde_json::json!({"type": "frame", "name": "inner", "x": 20, "y": 20, "width": 60, "height": 60, "children": [leaf, {"type": "ellipse", "x": 40, "y": 40, "width": 50, "height": 50}]});
            let outer = tx.insert_json(Some(page), None, serde_json::json!({"type": "frame", "x": 3000, "y": 0, "width": 100, "height": 100, "clip": true, "children": [inner]}))?;
            Ok(outer)
        })
        .map(|(o, _)| (o, by_name(&s, "inner"), by_name(&s, "leaf")))
        .unwrap();
    let mut idx = SpatialIndex::build(s.doc());
    let steps: Vec<Box<Step>> = vec![
        Box::new(move |tx| tx.update(outer, PropValue::Width(50.0))),
        Box::new(move |tx| tx.update(outer, PropValue::Rotation(30.0))),
        Box::new(move |tx| {
            tx.update(outer, PropValue::X(3100.0))?;
            tx.update(leaf, PropValue::Width(80.0))
        }),
        Box::new(move |tx| tx.update(inner, PropValue::Y(-10.0))),
        Box::new(move |tx| tx.update(inner, PropValue::Visible(false))),
        Box::new(move |tx| tx.update(outer, PropValue::Y(50.0))),
    ];
    for (i, f) in steps.iter().enumerate() {
        let (_, ch) = s.transact(TxMeta::user("step"), |tx| f(tx)).unwrap();
        idx.apply(s.doc(), &ch);
        if let Err(m) = same(&idx, &SpatialIndex::build(s.doc())) {
            panic!("step {i}: {m}");
        }
    }
    while let Some(ch) = s.undo().unwrap() {
        idx.apply(s.doc(), &ch);
    }
    same(&idx, &SpatialIndex::build(s.doc())).unwrap();
}

/// A bulk-loaded tree has leaves at different depths. Inserting after it, as after opening a
/// document and pasting, must not break it: rstar's forced reinsertion assumes even depth and
/// panicked here, at this size and others.
#[test]
fn inserts_into_a_bulk_loaded_page() {
    let (mut s, page) = start();
    let mut seed = 6700_u64 * 7 + 1;
    let mut next = move |range: f64| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (((seed >> 33) as f64) / (1_u64 << 31) as f64 * range).floor()
    };
    let mut rect = move || serde_json::json!({"type": "rectangle", "x": next(1600.0), "y": next(1600.0), "width": 1.0 + next(300.0), "height": 20});
    let mut add = |s: &mut Store, n: usize| {
        let nodes: Vec<_> = (0..n).map(|_| rect()).collect();
        s.transact(TxMeta::user("add"), |tx| nodes.into_iter().try_for_each(|r| tx.insert_json(Some(page), None, r).map(|_| ()))).unwrap().1
    };
    add(&mut s, 6700);
    let mut idx = SpatialIndex::build(s.doc());
    let ch = add(&mut s, 3350);
    idx.apply(s.doc(), &ch);
    same(&idx, &SpatialIndex::build(s.doc())).unwrap();
}
