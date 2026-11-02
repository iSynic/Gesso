//! Component commands through the editor: create, insert instance, override by editing inside,
//! detach, reset, go to main, and copy/paste of instances between documents.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use doc::{Document, NodeId, NodeKind, PropValue, Store, TxMeta};
use editor::commands::{self, Command, Key, KeyMods, shortcut};
use editor::{ClickMods, Session, clipboard, components};
use kurbo::{Point, Vec2};
use serde_json::{Value, json};
use text::FontDb;

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
    s.viewport = Vec2::new(1000.0, 1000.0);
    s.camera = editor::Camera { zoom: 1.0, origin: Point::new(0.0, 0.0) };
    s
}

fn real(s: &Session, name: &str) -> NodeId {
    s.doc().iter().find(|(_, n)| n.name == name && n.virt.is_none()).map(|(_, n)| n.id).unwrap_or_else(|| panic!("no node {name}"))
}

#[test]
fn create_insert_override_detach() {
    let mut s = session(json!([
        {"type": "rectangle", "name": "Box", "x": 10, "y": 10, "width": 40, "height": 20, "fills": [{"type": "solid", "color": "#ff0000"}]},
        {"type": "text", "name": "Label", "x": 60, "y": 10, "spans": [{"text": "Hi"}]},
    ]));
    assert_eq!(shortcut(Key::Char('k'), KeyMods { ctrl: true, alt: true, shift: false }), Some(Command::CreateComponent));
    s.select(vec![real(&s, "Box"), real(&s, "Label")]);
    commands::execute(&mut s, Command::CreateComponent).unwrap();
    let comp = s.selection()[0];
    assert!(matches!(s.doc().get(comp).unwrap().kind, NodeKind::Component { .. }));

    let inst = components::insert_instance(&mut s, comp, Some(Point::new(400.0, 400.0))).unwrap();
    assert_eq!(s.selection(), [inst]);
    // Enter the instance and recolour the box: an override, not an edit of the component.
    let b = s.scene.index.bounds(inst).unwrap();
    let inside = Point::new(b.x0 + 5.0, b.y0 + 5.0);
    s.click(inside, ClickMods::default());
    let boxed = s.double_click(inside).unwrap();
    assert!(s.doc().get(boxed).unwrap().virt.is_some());
    s.transact(TxMeta::user("Override fills"), |tx| tx.update(boxed, PropValue::Opacity(0.25))).unwrap();
    assert_eq!(s.doc().get(boxed).unwrap().props.opacity, 0.25);
    assert_eq!(s.doc().get(real(&s, "Box")).unwrap().props.opacity, 1.0);
    // Reset brings the component's value back.
    commands::execute(&mut s, Command::ResetOverrides).unwrap();
    assert_eq!(s.doc().get(boxed).unwrap().props.opacity, 1.0);
    // Deleting content inside an instance does nothing.
    commands::execute(&mut s, Command::Delete).unwrap();
    assert!(s.doc().get(boxed).is_some());

    s.select(vec![inst]);
    commands::execute(&mut s, Command::GoToMainComponent).unwrap();
    assert_eq!(s.selection(), [comp]);
    s.select(vec![inst]);
    commands::execute(&mut s, Command::DetachInstance).unwrap();
    let f = s.doc().get(inst).unwrap();
    assert!(matches!(f.kind, NodeKind::Frame { .. }));
    assert_eq!(f.children.len(), 2);
    s.undo().unwrap();
    assert!(matches!(s.doc().get(inst).unwrap().kind, NodeKind::Instance { .. }));
    s.doc().check_invariants().unwrap();
}

#[test]
fn pasting_instances_brings_their_components() {
    let mut a = session(json!([
        {"id": "0Cm00000000000000000C1", "type": "component", "name": "Btn", "width": 100, "height": 40, "children": [{"type": "rectangle", "name": "Dot", "width": 10, "height": 10}]},
        {"type": "instance", "name": "Use", "component": "0Cm00000000000000000C1", "y": 100, "width": 100, "height": 40},
    ]));
    a.select(vec![real(&a, "Use")]);
    let text = clipboard::copy(&a).unwrap();
    let mut b = session(json!([]));
    let pasted = clipboard::paste(&mut b, &text).unwrap().unwrap();
    let NodeKind::Instance { component, .. } = &b.doc().get(pasted[0]).unwrap().kind else { panic!() };
    assert_eq!(component.to_string(), "0Cm00000000000000000C1");
    assert_eq!(b.doc().get(*component).unwrap().name, "Btn");
    assert_eq!(b.doc().get(pasted[0]).unwrap().children.len(), 1, "expanded");
    // Pasting again reuses the component.
    clipboard::paste(&mut b, &text).unwrap().unwrap();
    assert_eq!(b.doc().iter().filter(|(_, n)| matches!(n.kind, NodeKind::Component { .. })).count(), 1);
    // Copying a component with its instance points the copy at the copied component.
    a.select(vec![real(&a, "Btn"), real(&a, "Use")]);
    let text = clipboard::copy(&a).unwrap();
    let both = clipboard::paste(&mut a, &text).unwrap().unwrap();
    let NodeKind::Instance { component, .. } = &a.doc().get(both[1]).unwrap().kind else { panic!() };
    assert_eq!(*component, both[0]);
    a.doc().check_invariants().unwrap();
}

#[test]
fn dragging_inside_an_instance_overrides_position() {
    let mut s = session(json!([
        {"id": "0Cm00000000000000000C1", "type": "component", "name": "Btn", "width": 200, "height": 100, "children": [
            {"type": "rectangle", "name": "Dot", "x": 10, "y": 10, "width": 40, "height": 40, "fills": [{"type": "solid", "color": "#ff0000"}]}]},
        {"type": "instance", "name": "Use", "component": "0Cm00000000000000000C1", "x": 300, "y": 300, "width": 200, "height": 100},
    ]));
    let inst = real(&s, "Use");
    s.select(vec![inst]);
    let dot = s.double_click(Point::new(330.0, 330.0)).unwrap();
    assert!(s.doc().get(dot).unwrap().virt.is_some());
    let start = Point::new(330.0, 330.0);
    let mut g = editor::transform::MoveGesture::begin(&mut s, start, false).unwrap().unwrap();
    let _ = g.update(&mut s, start + Vec2::new(30.0, 20.0), editor::transform::Mods::default());
    g.finish(&mut s).unwrap();
    let NodeKind::Instance { overrides, .. } = &s.doc().get(inst).unwrap().kind else { panic!() };
    assert!(!overrides.is_empty(), "the drag is an override");
    let d = s.doc().get(dot).unwrap();
    assert_eq!((d.props.x, d.props.y), (40.0, 30.0));
    assert_eq!(s.doc().get(real(&s, "Dot")).unwrap().props.x, 10.0, "component unchanged");
    s.doc().check_invariants().unwrap();
}

fn path_d(s: &Session, id: NodeId) -> String {
    match &s.doc().get(id).unwrap().kind {
        NodeKind::Path { paths } => paths[0].d.clone(),
        k => panic!("not a path: {k:?}"),
    }
}

#[test]
fn resizing_a_path_inside_an_instance_is_an_override() {
    let mut s = session(json!([
        {"id": "0Cm00000000000000000C1", "type": "component", "name": "Btn", "width": 200, "height": 100, "children": [
            {"type": "path", "name": "Tick", "x": 10, "y": 10, "width": 40, "height": 40, "paths": [{"d": "M0 0 L40 40"}], "strokes": [{"paint": {"type": "solid", "color": "#000000"}, "weight": 1}]}]},
        {"type": "instance", "name": "Use", "component": "0Cm00000000000000000C1", "x": 300, "y": 300, "width": 200, "height": 100},
    ]));
    s.snap_to_objects = false;
    s.snap_to_pixels = false;
    let inst = real(&s, "Use");
    s.select(vec![inst]);
    let tick = s.double_click(Point::new(330.0, 330.0)).unwrap();
    assert!(s.doc().get(tick).unwrap().virt.is_some());
    let mut g = editor::transform::ResizeGesture::begin(&s, editor::transform::Handle::SE).unwrap();
    g.update(&mut s, Point::new(390.0, 350.0), editor::transform::Mods::default()).unwrap();
    assert_eq!(path_d(&s, tick), "M0 0L80 40");
    assert_eq!(path_d(&s, real(&s, "Tick")), "M0 0 L40 40", "component unchanged");
    s.undo().unwrap();
    assert_eq!(path_d(&s, tick), "M0 0 L40 40");
    s.doc().check_invariants().unwrap();
}

#[test]
fn a_path_filling_an_instance_scales_with_it() {
    let s = session(json!([
        {"id": "0Cm00000000000000000C1", "type": "component", "name": "Btn", "width": 100, "height": 40, "autoLayout": {"gap": 0}, "children": [
            {"type": "path", "name": "Wave", "width": 100, "height": 40, "sizingH": "fill", "paths": [{"d": "M0 0 L100 40"}], "strokes": [{"paint": {"type": "solid", "color": "#000000"}, "weight": 1}]}]},
        {"type": "instance", "name": "Use", "component": "0Cm00000000000000000C1", "y": 100, "width": 300, "height": 40},
    ]));
    let wave = s.doc().iter().find(|(_, n)| n.name == "Wave" && n.virt.is_some()).map(|(_, n)| n.id).unwrap();
    assert_eq!(s.doc().get(wave).unwrap().props.width, 300.0);
    assert_eq!(path_d(&s, wave), "M0 0L300 40");
    assert_eq!(path_d(&s, real(&s, "Wave")), "M0 0 L100 40", "component unchanged");
    s.doc().check_invariants().unwrap();
}
