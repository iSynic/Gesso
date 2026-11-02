//! Move, resize, rotate, snapping, reorder and reparent.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use doc::{Document, NodeId, Store, TxMeta};
use editor::transform::{self, Handle, Mods, MoveGesture, ResizeGesture, RotateGesture, reorder_slot};
use editor::{Session, select};
use kurbo::{Point, Vec2};
use proptest::prelude::*;
use serde_json::{Value, json};
use text::FontDb;

fn red() -> Value {
    json!([{"type": "solid", "color": "#e04040"}])
}

fn session(children: Value) -> Session {
    let mut s = Store::new(Document::new());
    s.transact(TxMeta::user("setup"), |tx| {
        let page = tx.insert_json(None, None, json!({"type": "page"}))?;
        for c in children.as_array().into_iter().flatten() {
            tx.insert_json(Some(page), None, c.clone())?;
        }
        Ok(())
    })
    .unwrap();
    let mut s = Session::new(s.into_doc(), None, FontDb::bundled_only()).unwrap();
    s.viewport = Vec2::new(2000.0, 2000.0);
    s.camera = editor::Camera { zoom: 1.0, origin: Point::new(-500.0, -500.0) };
    s
}

fn id(s: &Session, name: &str) -> NodeId {
    s.doc().iter().find(|(_, n)| n.name == name).map(|(_, n)| n.id).unwrap_or_else(|| panic!("no node {name}"))
}

fn props(s: &Session, name: &str) -> doc::CommonProps {
    s.doc().get(id(s, name)).unwrap().props.clone()
}

fn centre(s: &Session, name: &str) -> Point {
    let n = s.doc().get(id(s, name)).unwrap();
    s.scene.index.world(n.id).unwrap() * Point::new(n.props.width / 2.0, n.props.height / 2.0)
}

fn json_of(s: &Session) -> String {
    format::to_json(s.doc())
}

fn close(a: Point, b: Point) -> bool {
    a.distance(b) < 1e-6
}

fn drag_move(s: &mut Session, from: Point, to: Point, mods: Mods) {
    let mut g = MoveGesture::begin(s, from, mods.alt).unwrap().unwrap();
    g.update(s, to, mods).unwrap();
    g.finish(s).unwrap();
}

#[test]
fn move_under_a_rotated_parent_follows_the_pointer_and_undoes() {
    let mut s = session(json!([
        {"type": "frame", "name": "f", "x": 0, "y": 0, "width": 200, "height": 200, "rotation": 90, "clip": false, "children": [
            {"type": "rectangle", "name": "r", "x": 20, "y": 20, "width": 40, "height": 40, "fills": red()}
        ]},
    ]));
    s.snap_to_pixels = false;
    s.snap_to_objects = false;
    let before = json_of(&s);
    let c0 = centre(&s, "r");
    s.select(vec![id(&s, "r")]);
    drag_move(&mut s, c0, c0 + Vec2::new(10.0, 0.0), Mods { ctrl: true, ..Default::default() });
    assert!(close(centre(&s, "r"), c0 + Vec2::new(10.0, 0.0)), "{:?}", centre(&s, "r"));
    // In the parent's rotated frame the move is along -y.
    let p = props(&s, "r");
    assert!((p.x - 20.0).abs() < 1e-9 && (p.y - 10.0).abs() < 1e-9, "{} {}", p.x, p.y);
    assert_eq!(s.store().undo_label(), Some("Move"));
    s.undo().unwrap();
    assert_eq!(json_of(&s), before);
}

#[test]
fn move_snaps_to_edges_within_the_threshold_only() {
    let mut s = session(json!([
        {"type": "rectangle", "name": "a", "x": 0, "y": 0, "width": 100, "height": 100, "fills": red()},
        {"type": "rectangle", "name": "b", "x": 200, "y": 300, "width": 50, "height": 50, "fills": red()},
    ]));
    s.select(vec![id(&s, "b")]);
    // b's left edge to 104: 4 px from a's right edge at 100 → snaps.
    let mut g = MoveGesture::begin(&mut s, Point::new(225.0, 325.0), false).unwrap().unwrap();
    let fb = g.update(&mut s, Point::new(129.0, 325.0), Mods::default()).unwrap().clone();
    assert_eq!(props(&s, "b").x, 100.0);
    assert!(fb.guides.iter().any(|g| g.a.x == 100.0 && g.b.x == 100.0), "{:?}", fb.guides);
    // 7 px away: no snap, rounded to whole pixels.
    g.update(&mut s, Point::new(132.4, 325.0), Mods::default()).unwrap();
    assert_eq!(props(&s, "b").x, 107.0);
    g.finish(&mut s).unwrap();
    // One undo step for the whole gesture.
    s.undo().unwrap();
    assert_eq!(props(&s, "b").x, 200.0);
}

#[test]
fn equal_spacing_snaps_and_marks_the_gaps() {
    let mut s = session(json!([
        {"type": "rectangle", "name": "a", "x": 0, "y": 0, "width": 100, "height": 100, "fills": red()},
        {"type": "rectangle", "name": "b", "x": 160, "y": 0, "width": 100, "height": 100, "fills": red()},
        {"type": "rectangle", "name": "c", "x": 500, "y": 0, "width": 100, "height": 100, "fills": red()},
    ]));
    s.select(vec![id(&s, "c")]);
    let mut g = MoveGesture::begin(&mut s, Point::new(550.0, 50.0), false).unwrap().unwrap();
    // The gap a→b is 60; dropping c's left edge near 260 + 60 = 320 snaps to it.
    let fb = g.update(&mut s, Point::new(372.0, 50.0), Mods::default()).unwrap().clone();
    assert_eq!(props(&s, "c").x, 320.0);
    assert_eq!(fb.gaps.len(), 2, "{:?}", fb.gaps);
    assert!(fb.gaps.iter().all(|m| (m.value - 60.0).abs() < 1e-9));
    // Distance labels to the nearest neighbour.
    assert!(fb.measures.iter().any(|m| (m.value - 60.0).abs() < 1e-9));
}

fn path_d(s: &Session, name: &str) -> String {
    match &s.doc().get(id(s, name)).unwrap().kind {
        doc::NodeKind::Path { paths } => paths[0].d.clone(),
        k => panic!("not a path: {k:?}"),
    }
}

fn path_node(x: f64, y: f64) -> Value {
    json!({"type": "path", "name": "p", "x": x, "y": y, "width": 100, "height": 50,
           "paths": [{"d": "M0 0 L100 0 L100 50 L0 50 Z"}], "fills": red()})
}

#[test]
fn resizing_a_path_scales_its_shape() {
    let mut s = session(json!([path_node(100.0, 100.0)]));
    s.snap_to_objects = false;
    s.snap_to_pixels = false;
    let before = path_d(&s, "p");
    s.select(vec![id(&s, "p")]);
    let mut g = ResizeGesture::begin(&s, Handle::SE).unwrap();
    g.update(&mut s, Point::new(300.0, 200.0), Mods::default()).unwrap();
    assert_eq!(path_d(&s, "p"), "M0 0L200 0L200 100L0 100Z");
    // Through zero and out again: the shape comes from the start of the drag.
    g.update(&mut s, Point::new(100.0, 150.0), Mods::default()).unwrap();
    g.update(&mut s, Point::new(150.0, 125.0), Mods::default()).unwrap();
    assert_eq!(path_d(&s, "p"), "M0 0L50 0L50 25L0 25Z");
    s.undo().unwrap();
    assert_eq!(path_d(&s, "p"), before);
    assert_eq!(props(&s, "p").width, 100.0);
}

#[test]
fn panel_width_scales_a_path_but_never_to_zero() {
    let mut s = session(json!([path_node(0.0, 0.0)]));
    let ids = vec![id(&s, "p")];
    editor::inspect::write(&mut s, &ids, editor::inspect::Applies::All, "width", json!(300), None).unwrap();
    assert_eq!(path_d(&s, "p"), "M0 0L300 0L300 50L0 50Z");
    editor::inspect::write(&mut s, &ids, editor::inspect::Applies::All, "width", json!(0), None).unwrap();
    assert_eq!(path_d(&s, "p"), "M0 0L300 0L300 50L0 50Z");
}

#[test]
fn a_path_scales_with_its_frame_constraints() {
    let mut s = session(json!([{"type": "frame", "name": "f", "x": 0, "y": 0, "width": 200, "height": 200, "children": [
        {"type": "path", "name": "p", "x": 0, "y": 0, "width": 100, "height": 50, "constraints": {"h": "scale", "v": "scale"},
         "paths": [{"d": "M0 0 L100 50"}], "fills": red()}]}]));
    let f = id(&s, "f");
    s.transact(TxMeta::user("grow"), |tx| {
        tx.update(f, doc::PropValue::Width(400.0))?;
        tx.update(f, doc::PropValue::Height(100.0))
    })
    .unwrap();
    let p = props(&s, "p");
    assert_eq!((p.width, p.height), (200.0, 25.0));
    assert_eq!(path_d(&s, "p"), "M0 0L200 25");
    s.undo().unwrap();
    assert_eq!(path_d(&s, "p"), "M0 0 L100 50");
}

#[test]
fn resize_corner_aspect_centre_and_flip() {
    let mut s = session(json!([{"type": "rectangle", "name": "r", "x": 100, "y": 100, "width": 100, "height": 50, "fills": red()}]));
    s.snap_to_objects = false;
    s.select(vec![id(&s, "r")]);
    let mut g = ResizeGesture::begin(&s, Handle::SE).unwrap();
    g.update(&mut s, Point::new(250.0, 160.0), Mods::default()).unwrap();
    let p = props(&s, "r");
    assert_eq!((p.x, p.y, p.width, p.height), (100.0, 100.0, 150.0, 60.0));
    // Shift: keep 2:1 using the larger factor.
    g.update(&mut s, Point::new(300.0, 160.0), Mods { shift: true, ..Default::default() }).unwrap();
    let p = props(&s, "r");
    assert_eq!((p.width, p.height), (200.0, 100.0));
    // Alt: about the centre (150, 125).
    g.update(&mut s, Point::new(210.0, 135.0), Mods { alt: true, ..Default::default() }).unwrap();
    let p = props(&s, "r");
    assert_eq!((p.x, p.y, p.width, p.height), (90.0, 115.0, 120.0, 20.0));
    // Past the opposite edge: flips, size stays positive.
    g.update(&mut s, Point::new(60.0, 160.0), Mods::default()).unwrap();
    let p = props(&s, "r");
    assert!(p.flip_x && !p.flip_y);
    assert_eq!((p.x, p.width), (60.0, 40.0));
}

#[test]
fn resize_of_a_rotated_node_keeps_the_opposite_corner() {
    let mut s = session(json!([{"type": "rectangle", "name": "r", "x": 100, "y": 100, "width": 100, "height": 50, "rotation": 30, "fills": red()}]));
    s.select(vec![id(&s, "r")]);
    let b = transform::selection_box(&s).unwrap();
    let nw = b.handle(Handle::NW);
    let target = b.world * Point::new(160.0, 80.0);
    let mut g = ResizeGesture::begin(&s, Handle::SE).unwrap();
    g.update(&mut s, target, Mods::default()).unwrap();
    let p = props(&s, "r");
    assert!((p.width - 160.0).abs() < 1e-6 && (p.height - 80.0).abs() < 1e-6);
    assert_eq!(p.rotation, 30.0);
    let b2 = transform::selection_box(&s).unwrap();
    assert!(close(b2.handle(Handle::NW), nw));
    assert!(close(b2.handle(Handle::SE), target));
}

#[test]
fn resize_fixes_hug_and_fill_axes() {
    let mut s = session(json!([{"type": "text", "name": "t", "x": 0, "y": 0, "width": 100, "height": 20, "sizingH": "hug", "spans": [{"text": "hello"}]}]));
    s.select(vec![id(&s, "t")]);
    let mut g = ResizeGesture::begin(&s, Handle::E).unwrap();
    g.update(&mut s, Point::new(150.0, 10.0), Mods::default()).unwrap();
    let p = props(&s, "t");
    assert_eq!(p.sizing_h, doc::model::Sizing::Fixed);
    assert_eq!(p.width, 150.0);
}

#[test]
fn multi_select_scale_and_rotate() {
    let mut s = session(json!([
        {"type": "rectangle", "name": "a", "x": 0, "y": 0, "width": 50, "height": 50, "fills": red()},
        {"type": "rectangle", "name": "b", "x": 150, "y": 50, "width": 50, "height": 50, "fills": red()},
    ]));
    s.snap_to_objects = false;
    s.select(vec![id(&s, "a"), id(&s, "b")]);
    let mut g = ResizeGesture::begin(&s, Handle::SE).unwrap();
    g.update(&mut s, Point::new(400.0, 200.0), Mods::default()).unwrap();
    let (a, b) = (props(&s, "a"), props(&s, "b"));
    assert_eq!((a.x, a.y, a.width, a.height), (0.0, 0.0, 100.0, 100.0));
    assert_eq!((b.x, b.y, b.width, b.height), (300.0, 100.0, 100.0, 100.0));
    s.undo().unwrap();
    // Rotate 180° about the selection centre (100, 50): a and b swap places.
    let mut r = RotateGesture::begin(&s, Point::new(300.0, 50.0)).unwrap();
    r.update(&mut s, Point::new(-100.0, 50.0), Mods::default()).unwrap();
    assert!(close(centre(&s, "a"), Point::new(175.0, 75.0)));
    assert!(close(centre(&s, "b"), Point::new(25.0, 25.0)));
    assert_eq!(props(&s, "a").rotation, -180.0);
}

#[test]
fn rotate_single_with_shift_snaps_to_15_degrees() {
    let mut s = session(json!([{"type": "rectangle", "name": "r", "x": 0, "y": 0, "width": 100, "height": 100, "fills": red()}]));
    s.select(vec![id(&s, "r")]);
    let c = centre(&s, "r");
    let mut r = RotateGesture::begin(&s, Point::new(200.0, 50.0)).unwrap();
    let at = |deg: f64| c + Vec2::from_angle(deg.to_radians()) * 100.0;
    r.update(&mut s, at(37.0), Mods::default()).unwrap();
    assert!((props(&s, "r").rotation - 37.0).abs() < 1e-6);
    r.update(&mut s, at(37.0), Mods { shift: true, ..Default::default() }).unwrap();
    assert!((props(&s, "r").rotation - 30.0).abs() < 1e-6);
    assert!(close(centre(&s, "r"), c));
}

fn row_frame(direction: &str, wrap: bool) -> Value {
    // Children are written where the layout puts them (gap 10, 50-wide items).
    let kids: Vec<Value> = (0..4)
        .map(|i| {
            let (x, y) = match (direction, wrap) {
                ("vertical", _) => (0.0, f64::from(i) * 60.0),
                (_, true) => (f64::from(i % 2) * 60.0, f64::from(i / 2) * 60.0),
                _ => (f64::from(i) * 60.0, 0.0),
            };
            json!({"type": "rectangle", "name": format!("k{i}"), "x": x, "y": y, "width": 50, "height": 50, "fills": red()})
        })
        .collect();
    let (w, h) = match (direction, wrap) {
        ("vertical", _) => (50, 230),
        (_, true) => (110, 110),
        _ => (230, 50),
    };
    json!([{"type": "frame", "name": "list", "width": w, "height": h,
            "autoLayout": {"direction": direction, "gap": 10, "crossGap": 10, "wrap": wrap}, "children": kids}])
}

fn order(s: &Session) -> Vec<String> {
    let doc = s.doc();
    doc.get(id(s, "list")).unwrap().children.iter().map(|&k| doc.node(k).unwrap().name.clone()).collect()
}

#[test]
fn reorder_slots_in_row_column_and_wrap() {
    for (dir, wrap, pointer, expect) in [
        ("horizontal", false, Point::new(215.0, 25.0), ["k1", "k2", "k3", "k0"]),
        ("horizontal", false, Point::new(130.0, 25.0), ["k1", "k0", "k2", "k3"]),
        ("vertical", false, Point::new(25.0, 140.0), ["k1", "k0", "k2", "k3"]),
        ("horizontal", true, Point::new(100.0, 80.0), ["k1", "k2", "k3", "k0"]),
        ("horizontal", true, Point::new(65.0, 80.0), ["k1", "k2", "k0", "k3"]),
    ] {
        let mut s = session(row_frame(dir, wrap));
        let k0 = id(&s, "k0");
        s.select(vec![k0]);
        let slot = reorder_slot(s.doc(), &s.scene.index, id(&s, "list"), &[k0], pointer);
        assert!(slot.is_some());
        let mut g = MoveGesture::begin(&mut s, Point::new(25.0, 25.0), false).unwrap().unwrap();
        let fb = g.update(&mut s, pointer, Mods::default()).unwrap().clone();
        assert!(fb.reorder.is_some());
        // Flow children don't move while dragging.
        assert_eq!(props(&s, "k0").x, 0.0);
        g.finish(&mut s).unwrap();
        assert_eq!(order(&s), expect, "{dir} wrap={wrap} at {pointer:?}");
        s.undo().unwrap();
        assert_eq!(order(&s), ["k0", "k1", "k2", "k3"]);
    }
}

#[test]
fn dragging_into_and_out_of_frames_reparents_keeping_the_world_position() {
    let mut s = session(json!([
        {"type": "frame", "name": "f", "x": 300, "y": 300, "width": 200, "height": 200, "rotation": 45, "fills": red()},
        {"type": "rectangle", "name": "r", "x": 0, "y": 0, "width": 20, "height": 20, "fills": red()},
    ]));
    s.snap_to_objects = false;
    s.snap_to_pixels = false;
    s.select(vec![id(&s, "r")]);
    let f = id(&s, "f");
    let mut g = MoveGesture::begin(&mut s, Point::new(10.0, 10.0), false).unwrap().unwrap();
    let fb = g.update(&mut s, Point::new(400.0, 400.0), Mods::default()).unwrap().clone();
    assert_eq!(fb.target, Some(f));
    g.finish(&mut s).unwrap();
    assert_eq!(select::parent_of(s.doc(), id(&s, "r")), Some(f));
    assert!(close(centre(&s, "r"), Point::new(400.0, 400.0)), "{:?}", centre(&s, "r"));
    assert!((props(&s, "r").rotation + 45.0).abs() < 1e-9);
    // Ctrl suppresses reparenting.
    let c = centre(&s, "r");
    drag_move(&mut s, c, Point::new(10.0, 10.0), Mods { ctrl: true, ..Default::default() });
    assert_eq!(select::parent_of(s.doc(), id(&s, "r")), Some(f));
    // Out again onto the page.
    drag_move(&mut s, Point::new(10.0, 10.0), Point::new(-50.0, -50.0), Mods::default());
    assert_eq!(select::parent_of(s.doc(), id(&s, "r")), Some(s.page()));
    assert!(close(centre(&s, "r"), Point::new(-50.0, -50.0)));
    assert!(props(&s, "r").rotation.abs() < 1e-9);
}

#[test]
fn alt_drag_duplicates_and_cancel_undoes() {
    let mut s = session(json!([{"type": "rectangle", "name": "r", "x": 0, "y": 0, "width": 20, "height": 20, "fills": red()}]));
    let before = json_of(&s);
    s.select(vec![id(&s, "r")]);
    let mut g = MoveGesture::begin(&mut s, Point::new(10.0, 10.0), true).unwrap().unwrap();
    g.update(&mut s, Point::new(110.0, 10.0), Mods { alt: true, ..Default::default() }).unwrap();
    assert_eq!(s.doc().len(), 3);
    assert_eq!(props(&s, "r").x, 0.0);
    let copy = s.selection()[0];
    assert_eq!(s.doc().get(copy).unwrap().props.x, 100.0);
    g.cancel(&mut s).unwrap();
    assert_eq!(json_of(&s), before);
}

#[test]
fn nudge_moves_by_world_delta() {
    let mut s = session(json!([{"type": "rectangle", "name": "r", "x": 0, "y": 0, "width": 20, "height": 20}]));
    s.select(vec![id(&s, "r")]);
    transform::nudge(&mut s, Vec2::new(10.0, -1.0)).unwrap();
    let p = props(&s, "r");
    assert_eq!((p.x, p.y), (10.0, -1.0));
}

#[test]
fn grips_on_a_rotated_box() {
    let mut s = session(json!([{"type": "rectangle", "name": "r", "x": 100, "y": 100, "width": 200, "height": 100, "rotation": 90}]));
    s.select(vec![id(&s, "r")]);
    let b = transform::selection_box(&s).unwrap();
    let cam = s.camera;
    let se = cam.to_screen(b.handle(Handle::SE));
    assert_eq!(transform::grip_at(&b, &cam, se), Some(editor::Grip::Resize(Handle::SE)));
    // Just outside the corner, away from the box: rotate.
    let out = se + (se - cam.to_screen(b.centre())).normalize() * 12.0;
    assert_eq!(transform::grip_at(&b, &cam, out), Some(editor::Grip::Rotate(Handle::SE)));
    // Middle of an edge: that edge's handle.
    let e = cam.to_screen(b.handle(Handle::E));
    assert_eq!(transform::grip_at(&b, &cam, e), Some(editor::Grip::Resize(Handle::E)));
    assert_eq!(transform::grip_at(&b, &cam, cam.to_screen(b.centre())), None);
}

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(48)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    /// Random gestures on random nodes, then undo everything: the document is restored exactly.
    #[test]
    fn gestures_then_undo_all_restore_the_document(steps in proptest::collection::vec((0u8..5, 0u8..32, -300i16..300, -300i16..300, 0u8..8), 1..12)) {
        let mut store = Store::new(Document::new());
        store.transact(TxMeta::user("setup"), |tx| doc::testing::render_test_page(tx).map(|_| ())).unwrap();
        let mut s = Session::new(store.into_doc(), None, FontDb::bundled_only()).unwrap();
        s.viewport = Vec2::new(1000.0, 1000.0);
        s.camera = editor::Camera { zoom: 1.0, origin: Point::new(-200.0, -200.0) };
        let before = json_of(&s);
        let ids: Vec<NodeId> = s.doc().iter().filter(|(_, n)| !matches!(n.kind, doc::NodeKind::Page { .. })).map(|(_, n)| n.id).collect();
        for (what, pick, dx, dy, m) in steps {
            let target = ids[pick as usize % ids.len()];
            if s.doc().get(target).is_none() { continue; }
            s.select(vec![target]);
            let mods = Mods { shift: m & 1 != 0, alt: m & 2 != 0, ctrl: m & 4 != 0 };
            let Some(b) = transform::selection_box(&s) else { continue };
            let start = b.centre();
            let to = start + Vec2::new(f64::from(dx), f64::from(dy));
            match what {
                0 => { if let Some(mut g) = MoveGesture::begin(&mut s, start, mods.alt).unwrap() { g.update(&mut s, to, mods).unwrap(); g.finish(&mut s).unwrap(); } }
                1 => { let h = Handle::ALL[usize::from(m) % 8]; if let Some(mut g) = ResizeGesture::begin(&s, h) { g.update(&mut s, b.handle(h) + Vec2::new(f64::from(dx) / 3.0, f64::from(dy) / 3.0), mods).unwrap(); } }
                2 => { if let Some(mut g) = RotateGesture::begin(&s, start + Vec2::new(50.0, 0.0)) { g.update(&mut s, to, mods).unwrap(); } }
                3 => transform::nudge(&mut s, Vec2::new(f64::from(dx.signum()), f64::from(dy.signum()))).unwrap(),
                _ => { if let Some(mut g) = MoveGesture::begin(&mut s, start, false).unwrap() { g.update(&mut s, to, mods).unwrap(); g.cancel(&mut s).unwrap(); } }
            }
            s.doc().check_invariants().unwrap();
        }
        while s.store().can_undo() { s.undo().unwrap(); }
        prop_assert_eq!(json_of(&s), before);
    }
}
