//! Hit-testing and selection rules on small hand-built pages.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use doc::{Document, NodeId, Store, TxMeta};
use editor::commands::{self, Command};
use editor::{Camera, ClickMods, Session};
use kurbo::{Point, Vec2};
use serde_json::{Value, json};
use text::FontDb;

fn red() -> Value {
    json!([{"type": "solid", "color": "#e04040"}])
}

/// A session whose first page holds `children` (JSON nodes with string ids).
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
    s.camera.zoom = 1.0;
    s
}

fn id(s: &Session, name: &str) -> NodeId {
    s.doc().iter().find(|(_, n)| n.name == name).map(|(_, n)| n.id).unwrap_or_else(|| panic!("no node {name}"))
}

fn name(s: &Session, id: Option<NodeId>) -> Option<String> {
    id.and_then(|i| s.doc().get(i)).map(|n| n.name.clone())
}

fn click(s: &mut Session, x: f64, y: f64) -> Option<String> {
    let p = s.click(Point::new(x, y), ClickMods::default());
    name(s, p)
}

fn selected(s: &Session) -> Vec<String> {
    s.selection().iter().map(|&i| name(s, Some(i)).unwrap_or_default()).collect()
}

#[test]
fn overlap_picks_topmost_and_skips_hidden_and_locked() {
    let mut s = session(json!([
        {"type": "rectangle", "name": "below", "x": 0, "y": 0, "width": 100, "height": 100, "fills": red()},
        {"type": "rectangle", "name": "above", "x": 50, "y": 50, "width": 100, "height": 100, "fills": red()},
    ]));
    assert_eq!(click(&mut s, 75.0, 75.0).as_deref(), Some("above"));
    assert_eq!(click(&mut s, 25.0, 25.0).as_deref(), Some("below"));
    assert_eq!(click(&mut s, 300.0, 300.0), None);
    assert!(s.selection().is_empty());

    let above = id(&s, "above");
    s.transact(TxMeta::user("hide"), |tx| tx.update(above, doc::PropValue::Visible(false))).unwrap();
    assert_eq!(click(&mut s, 75.0, 75.0).as_deref(), Some("below"));
    s.transact(TxMeta::user("show, lock"), |tx| {
        tx.update(above, doc::PropValue::Visible(true))?;
        tx.update(above, doc::PropValue::Locked(true))
    })
    .unwrap();
    assert_eq!(click(&mut s, 75.0, 75.0).as_deref(), Some("below"));
    assert_eq!(click(&mut s, 125.0, 125.0), None);
}

#[test]
fn rotated_shapes_hit_their_outline_not_their_bounding_box() {
    let mut s = session(json!([
        {"type": "rectangle", "name": "bar", "x": 100, "y": 100, "width": 200, "height": 20, "rotation": 45, "fills": red()},
    ]));
    // The centre of the box (200, 110) stays put under rotation about the centre.
    assert_eq!(click(&mut s, 200.0, 110.0).as_deref(), Some("bar"));
    // A corner of the unrotated box is far outside the rotated bar but inside its AABB.
    assert_eq!(click(&mut s, 110.0, 105.0), None);
    // Along the 45° diagonal from the centre.
    assert_eq!(click(&mut s, 260.0, 170.0).as_deref(), Some("bar"));
}

#[test]
fn clipped_content_does_not_hit_outside_the_clip() {
    let mut s = session(json!([
        {"type": "frame", "name": "clipper", "x": 0, "y": 0, "width": 100, "height": 100, "children": [
            {"type": "rectangle", "name": "inside", "x": 50, "y": 10, "width": 200, "height": 30, "fills": red()}
        ]},
        {"type": "frame", "name": "open", "x": 0, "y": 200, "width": 100, "height": 100, "clip": false, "children": [
            {"type": "rectangle", "name": "spill", "x": 50, "y": 10, "width": 200, "height": 30, "fills": red()}
        ]},
    ]));
    assert_eq!(click(&mut s, 75.0, 20.0).as_deref(), Some("inside"));
    assert_eq!(click(&mut s, 150.0, 20.0), None);
    assert_eq!(click(&mut s, 150.0, 220.0).as_deref(), Some("spill"));
}

#[test]
fn strokes_lines_and_unfilled_shapes() {
    let mut s = session(json!([
        {"type": "ellipse", "name": "ring", "x": 0, "y": 0, "width": 100, "height": 100,
         "strokes": [{"paint": {"type": "solid", "color": "#000000"}, "weight": 10, "align": "outside"}]},
        {"type": "line", "name": "line", "x": 200, "y": 50, "width": 100, "height": 0,
         "strokes": [{"paint": {"type": "solid", "color": "#000000"}, "weight": 1}]},
    ]));
    // An unfilled, stroked shape is hit on its stroke, not inside.
    assert_eq!(click(&mut s, 50.0, 50.0), None);
    assert_eq!(click(&mut s, 50.0, -8.0).as_deref(), Some("ring"));
    // Lines hit within the tolerance (4 px at 100 %), which scales with zoom.
    assert_eq!(click(&mut s, 250.0, 53.0).as_deref(), Some("line"));
    assert_eq!(click(&mut s, 250.0, 56.0), None);
    s.camera.zoom = 0.5;
    assert_eq!(click(&mut s, 250.0, 56.0).as_deref(), Some("line"));
}

#[test]
fn ctrl_picks_deepest_and_shift_toggles() {
    let mut s = session(json!([
        {"type": "group", "name": "g", "x": 0, "y": 0, "width": 100, "height": 100, "children": [
            {"type": "rectangle", "name": "r", "x": 0, "y": 0, "width": 50, "height": 50, "fills": red()},
            {"type": "rectangle", "name": "q", "x": 50, "y": 50, "width": 50, "height": 50, "fills": red()},
        ]},
        {"type": "rectangle", "name": "z", "x": 200, "y": 0, "width": 50, "height": 50, "fills": red()},
    ]));
    assert_eq!(click(&mut s, 10.0, 10.0).as_deref(), Some("g"));
    let deep = s.click(Point::new(10.0, 10.0), ClickMods { deep: true, toggle: false });
    assert_eq!(name(&s, deep).as_deref(), Some("r"));
    // The scope is now the group, so a plain click picks inside it until the selection clears.
    assert_eq!(click(&mut s, 60.0, 60.0).as_deref(), Some("q"));
    s.clear_selection();
    s.click(Point::new(10.0, 10.0), ClickMods::default());
    s.click(Point::new(210.0, 10.0), ClickMods { toggle: true, deep: false });
    assert_eq!(selected(&s), ["g", "z"]);
    s.click(Point::new(10.0, 10.0), ClickMods { toggle: true, deep: false });
    assert_eq!(selected(&s), ["z"]);
    // Clicking empty canvas with Shift keeps the selection.
    s.click(Point::new(500.0, 500.0), ClickMods { toggle: true, deep: false });
    assert_eq!(selected(&s), ["z"]);
}

#[test]
fn scope_drilling_climbing_and_see_through_frames() {
    let mut s = session(json!([
        {"type": "frame", "name": "board", "x": 0, "y": 0, "width": 400, "height": 400, "fills": [{"type": "solid", "color": "#ffffff"}], "children": [
            {"type": "group", "name": "g", "x": 10, "y": 10, "width": 200, "height": 100, "children": [
                {"type": "rectangle", "name": "r1", "x": 0, "y": 0, "width": 90, "height": 100, "fills": red()},
                {"type": "rectangle", "name": "r2", "x": 110, "y": 0, "width": 90, "height": 100, "fills": red()},
            ]},
        ]},
    ]));
    // The top-level frame is see-through: a click picks its child; its empty area picks nothing
    // (a drag there is a marquee), since the frame has content.
    assert_eq!(click(&mut s, 20.0, 20.0).as_deref(), Some("g"));
    assert_eq!(s.scope(), None);
    assert_eq!(click(&mut s, 300.0, 300.0), None);
    click(&mut s, 20.0, 20.0);
    // Double-click enters the group.
    let d = s.double_click(Point::new(20.0, 20.0));
    assert_eq!(name(&s, d).as_deref(), Some("r1"));
    assert_eq!(s.scope(), Some(id(&s, "g")));
    // Inside the scope, siblings are one click away.
    assert_eq!(click(&mut s, 150.0, 20.0).as_deref(), Some("r2"));
    // Clicking outside the scope goes back to page level.
    assert_eq!(click(&mut s, 300.0, 300.0), None);
    assert_eq!(s.scope(), None);
    // Esc climbs: r1 → g → board → nothing.
    s.select(vec![id(&s, "r1")]);
    s.escape();
    assert_eq!(selected(&s), ["g"]);
    s.escape();
    assert_eq!(selected(&s), ["board"]);
    s.escape();
    assert!(s.selection().is_empty());
}

#[test]
fn marquee_selects_in_the_container_it_starts_in() {
    let mut s = session(json!([
        {"type": "frame", "name": "a", "x": 0, "y": 0, "width": 300, "height": 300, "children": [
            {"type": "rectangle", "name": "a1", "x": 10, "y": 10, "width": 50, "height": 50, "fills": red()},
            {"type": "rectangle", "name": "a2", "x": 100, "y": 10, "width": 50, "height": 50, "fills": red()},
            {"type": "rectangle", "name": "a3", "x": 200, "y": 200, "width": 50, "height": 50, "fills": red(), "locked": true},
        ]},
        {"type": "frame", "name": "b", "x": 400, "y": 0, "width": 300, "height": 300},
        {"type": "rectangle", "name": "loose", "x": 0, "y": 400, "width": 50, "height": 50, "fills": red()},
    ]));
    // Starting on the canvas: top-level nodes it touches.
    s.marquee(Point::new(-10.0, 250.0), Point::new(450.0, 420.0), false);
    assert_eq!(selected(&s), ["a", "b", "loose"]);
    // Starting inside frame a: its unlocked children.
    s.marquee(Point::new(5.0, 5.0), Point::new(290.0, 290.0), false);
    assert_eq!(selected(&s), ["a1", "a2"]);
    // Shift adds.
    s.select(vec![id(&s, "loose")]);
    s.marquee(Point::new(5.0, 5.0), Point::new(70.0, 70.0), true);
    assert_eq!(selected(&s), ["loose", "a1"]);
}

#[test]
fn deleting_or_undoing_prunes_the_selection() {
    let mut s = session(json!([
        {"type": "rectangle", "name": "r", "x": 0, "y": 0, "width": 50, "height": 50, "fills": red()},
    ]));
    let page = s.page();
    let new = s.transact(TxMeta::user("add"), |tx| tx.insert_json(Some(page), None, json!({"type": "ellipse", "name": "e", "width": 10, "height": 10}))).unwrap();
    s.select(vec![new, id(&s, "r")]);
    s.undo().unwrap();
    assert_eq!(selected(&s), ["r"]);
    assert!(s.is_dirty());
    s.redo().unwrap();
    assert_eq!(s.pick(Point::new(5.0, 5.0), false), Some(new));
}

#[test]
fn layers_rows_are_topmost_first_and_follow_structure() {
    let mut s = session(json!([
        {"type": "frame", "name": "a", "width": 100, "height": 100, "children": [
            {"type": "rectangle", "name": "a1", "width": 10, "height": 10},
            {"type": "rectangle", "name": "a2", "width": 10, "height": 10},
        ]},
        {"type": "rectangle", "name": "b", "width": 10, "height": 10},
    ]));
    let names = |s: &mut Session| {
        let doc = s.doc().clone();
        s.layers.rows(&doc).iter().map(|r| (doc.get(r.id).map(|n| n.name.clone()).unwrap_or_default(), r.depth)).collect::<Vec<_>>()
    };
    assert_eq!(names(&mut s), [("b".to_string(), 0), ("a".to_string(), 0)]);
    let a1 = id(&s, "a1");
    let doc = s.doc().clone();
    s.layers.reveal(&doc, a1);
    assert_eq!(names(&mut s), [("b".into(), 0), ("a".into(), 0), ("a2".into(), 1), ("a1".into(), 1)]);
    s.transact(TxMeta::user("rename"), |tx| tx.update(a1, doc::PropValue::Name("first".into()))).unwrap();
    assert_eq!(names(&mut s)[3], ("first".into(), 1));
    let b = id(&s, "b");
    s.transact(TxMeta::user("delete"), |tx| tx.delete(b)).unwrap();
    assert_eq!(names(&mut s).len(), 3);
}

#[test]
fn instances_select_whole_and_can_be_entered() {
    let mut s = session(json!([
        {"id": "0Cm00000000000000000C1", "type": "component", "name": "Btn", "x": 0, "y": 0, "width": 100, "height": 40, "fills": red(),
            "children": [{"type": "rectangle", "name": "Dot", "x": 10, "y": 10, "width": 20, "height": 20, "fills": red()}]},
        {"type": "instance", "name": "Inst", "component": "0Cm00000000000000000C1", "x": 0, "y": 100, "width": 100, "height": 40},
    ]));
    // Content and its root both select the instance.
    assert_eq!(click(&mut s, 20.0, 120.0).as_deref(), Some("Inst"));
    assert_eq!(click(&mut s, 80.0, 120.0).as_deref(), Some("Inst"));
    let deep = s.click(Point::new(80.0, 120.0), ClickMods { deep: true, ..Default::default() });
    assert_eq!(name(&s, deep).as_deref(), Some("Inst"));
    // Double-click enters it; the content node is virtual.
    s.click(Point::new(20.0, 120.0), ClickMods::default());
    let dot = s.double_click(Point::new(20.0, 120.0)).unwrap();
    assert_eq!(name(&s, Some(dot)).as_deref(), Some("Dot"));
    assert!(s.doc().get(dot).unwrap().virt.is_some());
    // Clicking again inside stays in the instance; Esc goes back to it.
    assert_eq!(click(&mut s, 20.0, 120.0).as_deref(), Some("Dot"));
    s.escape();
    assert_eq!(selected(&s), ["Inst"]);
}

#[test]
fn interface_scale_leaves_the_canvas_alone() {
    let mut s = session(json!([{"type": "rectangle", "name": "A", "x": 0, "y": 0, "width": 100, "height": 100, "fills": red()}]));
    s.viewport = Vec2::new(800.0, 600.0);
    s.camera = Camera { zoom: 2.0, origin: Point::new(10.0, 20.0) };
    s.set_view_scale(1.5);
    // Same device size: fewer points per canvas unit, more pixels per point.
    assert!((s.zoom() - 2.0).abs() < 1e-9);
    assert!((s.camera.zoom * 1.5 - 2.0).abs() < 1e-9);
    assert_eq!(s.camera.origin, Point::new(10.0, 20.0));
    commands::execute(&mut s, Command::ZoomTo100).unwrap();
    assert!((s.zoom() - 1.0).abs() < 1e-9);
    commands::execute(&mut s, Command::ZoomTo(0.25)).unwrap();
    assert!((s.zoom() - 0.25).abs() < 1e-9);
    // Fitting a small page stops at 100 % as seen by the user.
    s.zoom_to_fit();
    assert!((s.zoom() - 1.0).abs() < 1e-9);
}

#[test]
fn an_empty_top_level_frame_is_picked_anywhere() {
    let mut s = session(json!([
        {"type": "frame", "name": "blank", "x": 0, "y": 0, "width": 400, "height": 400, "fills": [{"type": "solid", "color": "#ffffff"}]},
    ]));
    assert_eq!(click(&mut s, 300.0, 300.0).as_deref(), Some("blank"));
}
