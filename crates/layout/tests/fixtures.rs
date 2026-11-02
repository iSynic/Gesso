//! Hand-computed layouts. Each test builds a small tree in one transaction; the deriver lays it
//! out at commit, and the stored rects are checked against values worked out by hand.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use doc::*;
use layout::LayoutDeriver;
use serde_json::{Value, json};
use text::{FontDb, TextEngine};

fn store() -> (Store, NodeId) {
    let mut s = Store::default().with_ids(IdGen::seeded(7));
    s.set_deriver(Box::new(LayoutDeriver::new(TextEngine::new(FontDb::bundled_only()))));
    let page = s.transact(TxMeta::user("page"), |tx| tx.insert_json(None, None, json!({"type": "page"}))).unwrap().0;
    (s, page)
}

fn add(s: &mut Store, parent: NodeId, node: Value) -> NodeId {
    s.transact(TxMeta::user("add"), |tx| tx.insert_json(Some(parent), None, node)).unwrap().0
}

fn rect(s: &Store, id: NodeId) -> [f64; 4] {
    let p = &s.doc().get(id).unwrap().props;
    [p.x, p.y, p.width, p.height]
}

fn kids(s: &Store, id: NodeId) -> Vec<NodeId> {
    let d = s.doc();
    d.get(id).unwrap().children.iter().map(|k| d.node(*k).unwrap().id).collect()
}

fn kid_rects(s: &Store, id: NodeId) -> Vec<[f64; 4]> {
    kids(s, id).into_iter().map(|k| rect(s, k)).collect()
}

fn update(s: &mut Store, id: NodeId, props: Value) {
    s.transact(TxMeta::user("edit"), |tx| tx.update_json(id, props.as_object().unwrap())).unwrap();
}

fn r(w: f64, h: f64) -> Value {
    json!({"type": "rectangle", "width": w, "height": h})
}

#[test]
fn row_with_gap_and_padding_hugs() {
    let (mut s, page) = store();
    let f = add(
        &mut s,
        page,
        json!({"type": "frame", "x": 7, "y": 9, "sizingH": "hug", "sizingV": "hug",
               "autoLayout": {"gap": 10, "padding": [5, 5, 5, 5]},
               "children": [r(20.0, 10.0), r(30.0, 20.0), r(40.0, 15.0)]}),
    );
    assert_eq!(rect(&s, f), [7.0, 9.0, 120.0, 30.0]);
    assert_eq!(kid_rects(&s, f), vec![[5.0, 5.0, 20.0, 10.0], [35.0, 5.0, 30.0, 20.0], [75.0, 5.0, 40.0, 15.0]]);
}

#[test]
fn fill_children_share_space_and_stretch_across() {
    let (mut s, page) = store();
    let fill = json!({"type": "rectangle", "sizingH": "fill", "sizingV": "fill"});
    let f = add(
        &mut s,
        page,
        json!({"type": "frame", "width": 300, "height": 100, "autoLayout": {}, "children": [r(100.0, 40.0), fill.clone(), fill]}),
    );
    assert_eq!(kid_rects(&s, f), vec![[0.0, 0.0, 100.0, 40.0], [100.0, 0.0, 100.0, 100.0], [200.0, 0.0, 100.0, 100.0]]);
    // Narrowing the frame reflows the fill children only.
    update(&mut s, f, json!({"width": 200}));
    assert_eq!(kid_rects(&s, f), vec![[0.0, 0.0, 100.0, 40.0], [100.0, 0.0, 50.0, 100.0], [150.0, 0.0, 50.0, 100.0]]);
}

#[test]
fn space_between_and_center() {
    let (mut s, page) = store();
    let f = add(
        &mut s,
        page,
        json!({"type": "frame", "width": 200, "height": 50, "autoLayout": {"mainAlign": "spaceBetween", "crossAlign": "center"},
               "children": [r(20.0, 10.0), r(20.0, 30.0), r(20.0, 10.0)]}),
    );
    assert_eq!(kid_rects(&s, f), vec![[0.0, 20.0, 20.0, 10.0], [90.0, 10.0, 20.0, 30.0], [180.0, 20.0, 20.0, 10.0]]);
}

#[test]
fn wrap_uses_cross_gap_between_lines() {
    let (mut s, page) = store();
    let f = add(
        &mut s,
        page,
        json!({"type": "frame", "width": 100, "sizingV": "hug", "autoLayout": {"gap": 10, "crossGap": 5, "wrap": true},
               "children": [r(40.0, 10.0), r(40.0, 10.0), r(40.0, 10.0), r(40.0, 10.0)]}),
    );
    assert_eq!(rect(&s, f)[3], 25.0);
    assert_eq!(kid_rects(&s, f), vec![[0.0, 0.0, 40.0, 10.0], [50.0, 0.0, 40.0, 10.0], [0.0, 15.0, 40.0, 10.0], [50.0, 15.0, 40.0, 10.0]]);
}

#[test]
fn grid_with_fixed_and_fr_tracks() {
    let (mut s, page) = store();
    let cell = json!({"type": "rectangle", "height": 10, "sizingH": "fill"});
    let placed = json!({"type": "rectangle", "width": 5, "height": 5, "layoutChild": {"gridCell": [2, 1]}});
    let f = add(
        &mut s,
        page,
        json!({"type": "frame", "width": 250, "sizingV": "hug",
               "autoLayout": {"direction": "grid", "gap": 10, "crossGap": 4, "columns": [{"fixed": 50}, {"fr": 1}, {"fr": 1}]},
               "children": [cell.clone(), cell.clone(), cell, placed]}),
    );
    assert_eq!(kid_rects(&s, f), vec![[0.0, 0.0, 50.0, 10.0], [60.0, 0.0, 90.0, 10.0], [160.0, 0.0, 90.0, 10.0], [160.0, 14.0, 5.0, 5.0]]);
    assert_eq!(rect(&s, f)[3], 19.0);
}

#[test]
fn absolute_child_keeps_its_position() {
    let (mut s, page) = store();
    let abs = json!({"type": "rectangle", "x": 50, "y": 60, "width": 10, "height": 10, "layoutChild": {"absolute": true}});
    let f = add(
        &mut s,
        page,
        json!({"type": "frame", "sizingH": "hug", "sizingV": "hug", "autoLayout": {"padding": [2, 2, 2, 2]}, "children": [r(20.0, 10.0), abs]}),
    );
    assert_eq!(rect(&s, f), [0.0, 0.0, 24.0, 14.0]);
    assert_eq!(kid_rects(&s, f), vec![[2.0, 2.0, 20.0, 10.0], [50.0, 60.0, 10.0, 10.0]]);
}

#[test]
fn min_and_max_clamp_fill() {
    let (mut s, page) = store();
    let f = add(
        &mut s,
        page,
        json!({"type": "frame", "width": 300, "height": 10, "autoLayout": {},
               "children": [{"type": "rectangle", "height": 10, "sizingH": "fill", "maxWidth": 50},
                            {"type": "rectangle", "height": 10, "sizingH": "fill"},
                            {"type": "rectangle", "height": 10, "width": 5, "minWidth": 30}]}),
    );
    let w: Vec<f64> = kid_rects(&s, f).iter().map(|r| r[2]).collect();
    assert_eq!(w, vec![50.0, 220.0, 30.0]);
}

#[test]
fn nested_auto_layout_hugs_outward() {
    let (mut s, page) = store();
    let row = json!({"type": "frame", "sizingH": "hug", "sizingV": "hug", "autoLayout": {"gap": 5}, "children": [r(10.0, 10.0), r(10.0, 10.0)]});
    let col = add(
        &mut s,
        page,
        json!({"type": "frame", "sizingH": "hug", "sizingV": "hug", "autoLayout": {"direction": "vertical", "gap": 3}, "children": [row, r(50.0, 5.0)]}),
    );
    assert_eq!(rect(&s, col), [0.0, 0.0, 50.0, 18.0]);
    let inner = kids(&s, col)[0];
    assert_eq!(rect(&s, inner), [0.0, 0.0, 25.0, 10.0]);
    // Growing a leaf deep inside reflows both levels.
    let leaf = kids(&s, inner)[1];
    update(&mut s, leaf, json!({"width": 60}));
    assert_eq!(rect(&s, inner)[2], 75.0);
    assert_eq!(rect(&s, col)[2], 75.0);
    assert_eq!(rect(&s, kids(&s, col)[1])[1], 13.0);
}

#[test]
fn text_hugs_with_bundled_inter() {
    let (mut s, page) = store();
    let f = add(
        &mut s,
        page,
        json!({"type": "frame", "sizingH": "hug", "sizingV": "hug", "autoLayout": {"padding": [4, 8, 4, 8]},
               "children": [{"type": "text", "spans": [{"text": "Button"}], "autoResize": "widthAndHeight"}]}),
    );
    let t = rect(&s, kids(&s, f)[0]);
    assert!((35.0..60.0).contains(&t[2]), "text width {}", t[2]);
    assert!((14.0..20.0).contains(&t[3]), "text height {}", t[3]);
    assert_eq!(rect(&s, f)[2..], [t[2] + 16.0, t[3] + 8.0]);
    assert_eq!(t[..2], [8.0, 4.0]);
    // Editing the text widens the button.
    let id = kids(&s, f)[0];
    update(&mut s, id, json!({"spans": [{"text": "A much longer button"}]}));
    assert!(rect(&s, f)[2] > t[2] + 16.0 + 50.0);
}

#[test]
fn standalone_text_grows_in_height_at_fixed_width() {
    let (mut s, page) = store();
    let t = add(&mut s, page, json!({"type": "text", "width": 60, "spans": [{"text": "one two three four five six seven"}]}));
    let [_, _, w, h] = rect(&s, t);
    assert_eq!(w, 60.0);
    assert!(h > 50.0, "height {h}");
    update(&mut s, t, json!({"width": 1000}));
    assert!(rect(&s, t)[3] < 20.0);
}

#[test]
fn constraints_follow_parent_resize() {
    let (mut s, page) = store();
    let c = |h: &str, v: &str| json!({"type": "rectangle", "x": 10, "y": 10, "width": 20, "height": 20, "constraints": {"h": h, "v": v}});
    let f = add(
        &mut s,
        page,
        json!({"type": "frame", "width": 100, "height": 100,
               "children": [c("left", "top"), c("right", "bottom"), c("leftRight", "topBottom"), c("center", "center"), c("scale", "scale")]}),
    );
    update(&mut s, f, json!({"width": 200, "height": 150}));
    assert_eq!(
        kid_rects(&s, f),
        vec![[10.0, 10.0, 20.0, 20.0], [110.0, 60.0, 20.0, 20.0], [10.0, 10.0, 120.0, 70.0], [60.0, 35.0, 20.0, 20.0], [20.0, 15.0, 40.0, 30.0]]
    );
    // Undo puts everything back.
    s.undo().unwrap();
    assert!(kid_rects(&s, f).iter().all(|r| *r == [10.0, 10.0, 20.0, 20.0]));
}

#[test]
fn hug_frame_resize_drives_constraints_in_nested_frame() {
    let (mut s, page) = store();
    let inner = json!({"type": "frame", "width": 50, "height": 20, "sizingH": "fill",
                       "children": [{"type": "rectangle", "x": 40, "width": 10, "height": 10, "constraints": {"h": "right"}}]});
    let f = add(&mut s, page, json!({"type": "frame", "width": 100, "height": 20, "autoLayout": {}, "children": [inner]}));
    let inner = kids(&s, f)[0];
    assert_eq!(rect(&s, inner)[2], 100.0);
    assert_eq!(rect(&s, kids(&s, inner)[0])[0], 90.0);
}

#[test]
fn group_fits_its_children() {
    let (mut s, page) = store();
    let g = add(
        &mut s,
        page,
        json!({"type": "group", "x": 100, "y": 100,
               "children": [{"type": "rectangle", "x": 10, "y": 10, "width": 20, "height": 20},
                            {"type": "rectangle", "x": -5, "y": 0, "width": 10, "height": 10}]}),
    );
    assert_eq!(rect(&s, g), [95.0, 100.0, 35.0, 30.0]);
    assert_eq!(kid_rects(&s, g), vec![[15.0, 10.0, 20.0, 20.0], [0.0, 0.0, 10.0, 10.0]]);
    // Moving a child out to the right grows the group; nothing moves on the canvas.
    let a = kids(&s, g)[0];
    update(&mut s, a, json!({"x": 100}));
    assert_eq!(rect(&s, g), [95.0, 100.0, 120.0, 30.0]);
}

#[test]
fn derived_writes_share_the_undo_step_and_settled_documents_do_not_churn() {
    let (mut s, page) = store();
    let f = add(
        &mut s,
        page,
        json!({"type": "frame", "sizingH": "hug", "sizingV": "hug", "autoLayout": {"gap": 10}, "children": [r(20.0, 10.0), r(30.0, 10.0)]}),
    );
    let before = s.doc().to_data();
    let leaf = kids(&s, f)[0];
    update(&mut s, leaf, json!({"width": 50}));
    assert_eq!(rect(&s, f)[2], 90.0);
    assert_eq!(rect(&s, kids(&s, f)[1])[0], 60.0);
    s.undo().unwrap();
    assert_eq!(s.doc().to_data(), before);
    s.redo().unwrap();
    assert_eq!(rect(&s, f)[2], 90.0);
    // A full relayout of a settled document changes nothing and makes no undo step.
    let (_, changes) = s.transact_with_deriver(TxMeta::user("relayout"), |tx, d| d.unwrap().derive_all(tx)).unwrap();
    assert!(changes.is_empty(), "{changes:?}");
    // Editing a computed position directly is snapped back to the layout.
    update(&mut s, leaf, json!({"x": 33}));
    assert_eq!(rect(&s, leaf)[0], 0.0);
}

#[test]
fn hidden_children_leave_the_flow() {
    let (mut s, page) = store();
    let f = add(&mut s, page, json!({"type": "frame", "sizingH": "hug", "height": 10, "autoLayout": {"gap": 10}, "children": [r(20.0, 10.0), r(30.0, 10.0), r(5.0, 10.0)]}));
    let mid = kids(&s, f)[1];
    update(&mut s, mid, json!({"visible": false}));
    assert_eq!(rect(&s, f)[2], 35.0);
    assert_eq!(rect(&s, mid), [30.0, 0.0, 30.0, 10.0], "a hidden node keeps its last rect");
    assert_eq!(rect(&s, kids(&s, f)[2])[0], 30.0);
}

#[test]
fn typing_reflows_the_stress_tree() {
    use doc::testing::{GenConfig, STRESS_ROOT, generate};
    let mut s = Store::new(Document::from_data(generate(&GenConfig { target_nodes: 10, auto_layout_heavy: true, ..Default::default() })).unwrap());
    s.set_deriver(Box::new(LayoutDeriver::new(TextEngine::new(FontDb::bundled_only()))));
    s.transact_with_deriver(TxMeta::user("layout"), |tx, d| d.unwrap().derive_all(tx)).unwrap();
    let root = s.doc().iter().find(|(_, n)| n.name == STRESS_ROOT).unwrap().1.id;
    let section = kids(&s, root)[3];
    let card = kids(&s, section)[0];
    let body = kids(&s, card)[2];
    let (h0, root_h0) = (rect(&s, body)[3], rect(&s, root)[3]);
    let next_section_y = rect(&s, kids(&s, root)[4])[1];
    update(&mut s, body, json!({"spans": [{"text": "word ".repeat(80)}]}));
    let grew = rect(&s, body)[3] - h0;
    assert!(grew > 50.0, "body grew {grew}");
    // The card, its section, the sections below and the root all moved or grew by the same amount.
    assert!(rect(&s, kids(&s, root)[4])[1] > next_section_y);
    assert!((rect(&s, root)[3] - root_h0 - (rect(&s, kids(&s, root)[4])[1] - next_section_y)).abs() < 0.02);
    // And it equals a full layout from scratch.
    let mut fresh = Store::new(Document::from_data(s.doc().to_data()).unwrap());
    fresh.set_deriver(Box::new(LayoutDeriver::new(TextEngine::new(FontDb::bundled_only()))));
    let (_, ch) = fresh.transact_with_deriver(TxMeta::user("full"), |tx, d| d.unwrap().derive_all(tx)).unwrap();
    assert!(ch.is_empty(), "{:?}", ch.updated.len());
}
