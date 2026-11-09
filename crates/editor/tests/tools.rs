//! Tools through the controller, commands, clipboard and autosave.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::{Duration, Instant};

use doc::{Document, NodeId, NodeKind, Store, TxMeta};
use editor::commands::{self, Command, Key, KeyMods, shortcut};
use editor::transform::{self, Handle, Mods, ResizeGesture};
use editor::{Controller, Pointer, Session, Tool, autosave, clipboard, select};
use kurbo::{Point, Vec2};
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
    s.viewport = Vec2::new(1000.0, 1000.0);
    // Screen = world + (100, 100) at 100 %.
    s.camera = editor::Camera { zoom: 1.0, origin: Point::new(-100.0, -100.0) };
    s.snap_to_objects = false;
    s
}

fn id(s: &Session, name: &str) -> NodeId {
    s.doc().iter().find(|(_, n)| n.name == name).map(|(_, n)| n.id).unwrap_or_else(|| panic!("no node {name}"))
}

fn props(s: &Session, id: NodeId) -> doc::CommonProps {
    s.doc().get(id).unwrap().props.clone()
}

fn centre(s: &Session, id: NodeId) -> Point {
    let n = s.doc().get(id).unwrap();
    s.scene.index.world(id).unwrap() * Point::new(n.props.width / 2.0, n.props.height / 2.0)
}

fn at(x: f64, y: f64) -> Pointer {
    Pointer { screen: Point::new(x + 100.0, y + 100.0), ..Default::default() }
}

fn with(mut p: Pointer, mods: Mods) -> Pointer {
    p.mods = mods;
    p
}

fn drag(c: &mut Controller, s: &mut Session, path: &[Pointer]) {
    c.pointer_down(s, path[0], 1).unwrap();
    for p in &path[1..] {
        c.pointer_move(s, *p).unwrap();
    }
    c.pointer_up(s, *path.last().unwrap()).unwrap();
}

#[test]
fn drawing_shapes_by_drag_and_click() {
    let mut s = session(json!([]));
    let mut c = Controller::default();
    c.set_tool(Tool::Rectangle);
    drag(&mut c, &mut s, &[at(10.0, 20.0), at(50.0, 60.0), at(110.0, 70.0)]);
    let r = s.selection()[0];
    let p = props(&s, r);
    assert_eq!((p.x, p.y, p.width, p.height), (10.0, 20.0, 100.0, 50.0));
    assert_eq!(c.tool, Tool::Move);
    assert_eq!(s.store().undo_label(), Some("Draw rectangle"));
    // The whole drag is one undo step.
    s.undo().unwrap();
    assert_eq!(s.doc().len(), 1);

    // Shift makes a square; Alt draws from the centre.
    c.set_tool(Tool::Ellipse);
    drag(&mut c, &mut s, &[at(100.0, 100.0), with(at(140.0, 120.0), Mods { shift: true, alt: true, ctrl: false })]);
    let p = props(&s, s.selection()[0]);
    assert_eq!((p.x, p.y, p.width, p.height), (60.0, 60.0, 80.0, 80.0));

    // A click places the default size.
    c.set_tool(Tool::Frame);
    drag(&mut c, &mut s, &[at(300.0, 300.0)]);
    let p = props(&s, s.selection()[0]);
    assert_eq!((p.x, p.y, p.width, p.height), (300.0, 300.0, 100.0, 100.0));

    // Drawing inside that frame puts the shape in it, in the frame's coordinates.
    let frame = s.selection()[0];
    c.set_tool(Tool::Rectangle);
    drag(&mut c, &mut s, &[at(310.0, 320.0), at(330.0, 340.0)]);
    let r = s.selection()[0];
    assert_eq!(select::parent_of(s.doc(), r), Some(frame));
    let p = props(&s, r);
    assert_eq!((p.x, p.y, p.width, p.height), (10.0, 20.0, 20.0, 20.0));
}

#[test]
fn drawing_lines_and_text() {
    let mut s = session(json!([]));
    let mut c = Controller::default();
    c.set_tool(Tool::Line);
    drag(&mut c, &mut s, &[at(0.0, 0.0), with(at(100.0, 90.0), Mods { shift: true, ..Default::default() })]);
    let l = s.selection()[0];
    let p = props(&s, l);
    assert!((p.rotation - 45.0).abs() < 1e-9 && p.height == 0.0, "{p:?}");
    c.set_tool(Tool::Text);
    drag(&mut c, &mut s, &[at(0.0, 200.0)]);
    let t = s.selection()[0];
    assert!(matches!(s.doc().get(t).unwrap().kind, NodeKind::Text { .. }));
    assert_eq!(c.text.as_ref().map(|e| (e.node, e.range())), Some((t, (0, 4))), "a new text is edited with its placeholder selected");
    // Laid out by the deriver: hugging text got a size.
    assert!(props(&s, t).width > 0.0);
}

#[test]
fn press_drag_moves_and_marquee_selects() {
    let mut s = session(json!([
        {"type": "rectangle", "name": "a", "x": 0, "y": 0, "width": 50, "height": 50, "fills": red()},
        {"type": "rectangle", "name": "b", "x": 100, "y": 0, "width": 50, "height": 50, "fills": red()},
    ]));
    s.snap_to_pixels = false;
    let mut c = Controller::default();
    drag(&mut c, &mut s, &[at(10.0, 10.0), at(20.0, 10.0), at(30.0, 15.0)]);
    let a = id(&s, "a");
    assert_eq!(s.selection(), [a]);
    assert_eq!((props(&s, a).x, props(&s, a).y), (20.0, 5.0));
    // Marquee from empty canvas.
    drag(&mut c, &mut s, &[at(-20.0, -20.0), at(200.0, 30.0)]);
    assert_eq!(s.selection().len(), 2);
    // Dragging a member of the multi-selection moves both.
    drag(&mut c, &mut s, &[at(110.0, 10.0), at(110.0, 110.0)]);
    assert_eq!(props(&s, id(&s, "b")).y, 100.0);
    assert_eq!(props(&s, a).y, 105.0);
    // A click (no drag) on one member selects just it.
    drag(&mut c, &mut s, &[at(110.0, 110.0)]);
    assert_eq!(s.selection(), [id(&s, "b")]);
    // Esc during a drag cancels it.
    c.pointer_down(&mut s, at(110.0, 110.0), 1).unwrap();
    c.pointer_move(&mut s, at(300.0, 300.0)).unwrap();
    assert!(c.cancel(&mut s).unwrap());
    assert_eq!(props(&s, id(&s, "b")).y, 100.0);
}

#[test]
fn handles_resize_through_the_controller() {
    let mut s = session(json!([{"type": "rectangle", "name": "r", "x": 0, "y": 0, "width": 100, "height": 100, "fills": red()}]));
    let mut c = Controller::default();
    s.select(vec![id(&s, "r")]);
    drag(&mut c, &mut s, &[at(100.0, 100.0), at(150.0, 120.0)]);
    let p = props(&s, id(&s, "r"));
    assert_eq!((p.width, p.height), (150.0, 120.0));
}

#[test]
fn keyboard_map() {
    let ctrl = KeyMods { ctrl: true, ..Default::default() };
    let ctrl_shift = KeyMods { ctrl: true, shift: true, alt: false };
    assert_eq!(shortcut(Key::Char('z'), ctrl), Some(Command::Undo));
    assert_eq!(shortcut(Key::Char('Z'), ctrl_shift), Some(Command::Redo));
    assert_eq!(shortcut(Key::Char('g'), ctrl_shift), Some(Command::Ungroup));
    assert_eq!(shortcut(Key::Char('r'), KeyMods::default()), Some(Command::Tool(Tool::Rectangle)));
    assert_eq!(shortcut(Key::Char('!'), KeyMods { shift: true, ..Default::default() }), Some(Command::ZoomToFit));
    assert_eq!(shortcut(Key::Left, KeyMods { shift: true, ..Default::default() }), Some(Command::Nudge(Vec2::new(-10.0, 0.0))));
    assert_eq!(shortcut(Key::Char('q'), KeyMods::default()), None);
}

#[test]
fn delete_duplicate_and_restack() {
    let mut s = session(json!([
        {"type": "rectangle", "name": "a", "width": 10, "height": 10},
        {"type": "rectangle", "name": "b", "width": 10, "height": 10},
        {"type": "rectangle", "name": "c", "width": 10, "height": 10},
    ]));
    let names = |s: &Session| -> Vec<String> {
        let d = s.doc();
        d.get(s.page()).unwrap().children.iter().map(|&k| d.node(k).unwrap().name.clone()).collect()
    };
    s.select(vec![id(&s, "a")]);
    commands::execute(&mut s, Command::BringToFront).unwrap();
    assert_eq!(names(&s), ["b", "c", "a"]);
    commands::execute(&mut s, Command::SendBackward).unwrap();
    assert_eq!(names(&s), ["b", "a", "c"]);
    s.select(vec![id(&s, "b"), id(&s, "a")]);
    commands::execute(&mut s, Command::BringForward).unwrap();
    assert_eq!(names(&s), ["c", "b", "a"]);
    commands::execute(&mut s, Command::SendToBack).unwrap();
    assert_eq!(names(&s), ["b", "a", "c"]);
    commands::execute(&mut s, Command::Duplicate).unwrap();
    assert_eq!(names(&s), ["b", "b", "a", "a", "c"]);
    assert_eq!(props(&s, s.selection()[0]).x, 10.0);
    commands::execute(&mut s, Command::Delete).unwrap();
    assert_eq!(names(&s), ["b", "a", "c"]);
    assert!(s.selection().is_empty());
}

#[test]
fn group_and_ungroup_keep_positions() {
    let mut s = session(json!([
        {"type": "rectangle", "name": "a", "x": 40, "y": 50, "width": 10, "height": 10, "rotation": 20},
        {"type": "rectangle", "name": "b", "x": 100, "y": 80, "width": 30, "height": 10},
    ]));
    let (a, b) = (id(&s, "a"), id(&s, "b"));
    let (ca, cb) = (centre(&s, a), centre(&s, b));
    s.select(vec![a, b]);
    commands::execute(&mut s, Command::Group).unwrap();
    let g = s.selection()[0];
    assert!(matches!(s.doc().get(g).unwrap().kind, NodeKind::Group {}));
    assert_eq!(select::parent_of(s.doc(), a), Some(g));
    assert!(centre(&s, a).distance(ca) < 1e-9 && centre(&s, b).distance(cb) < 1e-9);
    // Resizing the group scales its children.
    let gp = props(&s, g);
    let mut r = ResizeGesture::begin(&s, Handle::E).unwrap();
    r.update(&mut s, Point::new(gp.x + gp.width * 2.0, gp.y), Mods::default()).unwrap();
    // The deriver fits a group to its children's unrotated boxes, so a rotated child makes
    // the result approximate.
    assert!((props(&s, g).width - gp.width * 2.0).abs() < 1.0, "{} vs {}", props(&s, g).width, gp.width * 2.0);
    s.undo().unwrap();
    // Rotate the group, then ungroup: children keep their place on the canvas.
    s.transact(TxMeta::user("rotate"), |tx| tx.update(g, doc::PropValue::Rotation(30.0))).unwrap();
    let (ca, cb) = (centre(&s, a), centre(&s, b));
    s.select(vec![g]);
    commands::execute(&mut s, Command::Ungroup).unwrap();
    assert!(s.doc().get(g).is_none());
    assert_eq!(select::parent_of(s.doc(), a), Some(s.page()));
    assert!(centre(&s, a).distance(ca) < 1e-6 && centre(&s, b).distance(cb) < 1e-6);
    assert!((props(&s, a).rotation - 50.0).abs() < 1e-9);
}

#[test]
fn copy_paste_in_place_or_centred() {
    let mut s = session(json!([
        {"type": "frame", "name": "f", "x": 0, "y": 0, "width": 200, "height": 200, "children": [
            {"type": "rectangle", "name": "r", "x": 10, "y": 10, "width": 30, "height": 30, "fills": red()}
        ]},
    ]));
    let r = id(&s, "r");
    s.select(vec![r]);
    let text = clipboard::copy(&s).unwrap();
    assert!(text.starts_with(clipboard::TAG));
    // Paste with nothing selected: page level, same place on the canvas.
    s.clear_selection();
    let pasted = clipboard::paste(&mut s, &text).unwrap().unwrap();
    assert_eq!(pasted.len(), 1);
    assert_ne!(pasted[0], r);
    assert_eq!(select::parent_of(s.doc(), pasted[0]), Some(s.page()));
    assert!(centre(&s, pasted[0]).distance(centre(&s, r)) < 1e-9);
    // Paste into the selected frame.
    s.select(vec![id(&s, "f")]);
    let into = clipboard::paste(&mut s, &text).unwrap().unwrap();
    assert_eq!(select::parent_of(s.doc(), into[0]), Some(id(&s, "f")));
    // Out of view: centred in the view.
    s.camera.origin = Point::new(5000.0, 5000.0);
    s.clear_selection();
    let away = clipboard::paste(&mut s, &text).unwrap().unwrap();
    assert!(centre(&s, away[0]).distance(s.view().center()) < 1e-9);
    // Foreign text is ignored.
    assert!(clipboard::paste(&mut s, "hello").unwrap().is_none());
}

#[test]
fn autosave_writes_after_quiet_time_and_recovers() {
    let dir = std::env::temp_dir().join(format!("aide-autosave-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut s = session(json!([{"type": "rectangle", "name": "r", "width": 10, "height": 10}]));
    let mut a = autosave::Autosave::new(dir.clone(), &mut s);
    let t0 = Instant::now();
    assert!(!a.tick(&mut s, t0));
    let r = id(&s, "r");
    s.transact(TxMeta::user("edit"), |tx| tx.update(r, doc::PropValue::X(42.0))).unwrap();
    assert!(!a.tick(&mut s, t0));
    assert!(!a.tick(&mut s, t0 + Duration::from_millis(1500)));
    assert!(a.tick(&mut s, t0 + Duration::from_millis(2100)));
    // A drag-like run of updates, a delete, an undo (re-insert) and a redo, in a second batch.
    for i in 0..50 {
        s.transact(TxMeta::user("move").coalesce("drag"), |tx| tx.update(r, doc::PropValue::X(f64::from(i)))).unwrap();
    }
    s.transact(TxMeta::user("delete"), |tx| tx.delete(r)).unwrap();
    s.undo().unwrap();
    s.redo().unwrap();
    s.undo().unwrap();
    a.snapshot(&mut s);
    assert!(a.last.compacted < a.last.ops, "{:?}", a.last);
    a.flush();
    let found = autosave::find(&dir);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].meta.source, None);
    let doc = autosave::load(&found[0]).unwrap();
    assert_eq!(format::to_json(&doc), format::to_json(s.doc()));
    // Nothing new to write.
    assert!(!a.tick(&mut s, t0 + Duration::from_secs(10)));
    a.saved(&mut s);
    a.flush();
    assert!(autosave::find(&dir).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn autosave_journal_on_a_bundle_replays_and_tolerates_a_torn_line() {
    let root = std::env::temp_dir().join(format!("aide-journal-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("recovery");
    let mut s = session(json!([{"type": "frame", "name": "f", "width": 100, "height": 100, "children": [
        {"type": "rectangle", "name": "r", "width": 10, "height": 10}]}]));
    s.save_as(root.join("doc.canvas")).unwrap();
    let mut a = autosave::Autosave::new(dir.clone(), &mut s);
    let (f, r) = (id(&s, "f"), id(&s, "r"));
    s.transact(TxMeta::user("edit"), |tx| {
        tx.update(r, doc::PropValue::Y(7.0))?;
        tx.insert_json(Some(f), None, json!({"type": "ellipse", "name": "e", "width": 5, "height": 5}))?;
        Ok(())
    })
    .unwrap();
    a.snapshot(&mut s);
    let expected = format::to_json(s.doc());
    s.transact(TxMeta::user("more"), |tx| tx.update(r, doc::PropValue::X(3.0))).unwrap();
    a.snapshot(&mut s);
    a.flush();
    let found = autosave::find(&dir);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].meta.base, autosave::Base::Bundle);
    // A crash mid-write leaves half a line: everything before it still recovers.
    let text = std::fs::read_to_string(&found[0].journal).unwrap();
    let first = text.lines().next().unwrap();
    std::fs::write(&found[0].journal, format!("{first}
{}", &text[first.len() + 1..first.len() + 10])).unwrap();
    assert_eq!(format::to_json(&autosave::load(&found[0]).unwrap()), expected);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn recovery_is_offered_only_while_the_bundle_is_unchanged() {
    let root = std::env::temp_dir().join(format!("aide-recover-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("recovery");
    let mut s = session(json!([{"type": "rectangle", "name": "r", "width": 10, "height": 10}]));
    s.save_as(root.join("doc.canvas")).unwrap();
    std::thread::sleep(Duration::from_millis(20));
    let mut a = autosave::Autosave::new(dir.clone(), &mut s);
    let r = id(&s, "r");
    s.transact(TxMeta::user("edit"), |tx| tx.update(r, doc::PropValue::X(1.0))).unwrap();
    a.snapshot(&mut s);
    a.flush();
    let found = autosave::find(&dir);
    assert_eq!(found.len(), 1);
    assert!(found[0].meta.source.is_some());
    // Saving the bundle afterwards (here without telling the autosave) makes the journal
    // inapplicable: it is no longer offered, and is cleaned up.
    std::thread::sleep(Duration::from_millis(20));
    s.save().unwrap();
    assert!(autosave::find(&dir).is_empty());
    assert!(std::fs::read_dir(&dir).unwrap().next().is_none());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn journal_compaction_keeps_the_last_update_per_run() {
    use doc::{Id, Op, PropValue};
    let (a, b) = (Id(1), Id(2));
    let up = |id, x: f64| Op::Update { id, value: PropValue::X(x) };
    let ops = vec![up(a, 1.0), up(b, 1.0), up(a, 2.0), Op::Delete { id: a }, up(b, 3.0), up(b, 4.0)];
    let out = autosave::compact(ops);
    assert_eq!(out, vec![up(b, 1.0), up(a, 2.0), Op::Delete { id: a }, up(b, 4.0)]);
}

#[test]
fn nudge_inside_auto_layout_moves_one_slot() {
    let mut s = session(json!([{"type": "frame", "name": "list", "width": 200, "height": 50, "autoLayout": {"gap": 10}, "children": [
        {"type": "rectangle", "name": "k0", "width": 50, "height": 50},
        {"type": "rectangle", "name": "k1", "width": 50, "height": 50},
    ]}]));
    s.select(vec![id(&s, "k0")]);
    transform::nudge(&mut s, Vec2::new(1.0, 0.0)).unwrap();
    let d = s.doc();
    let kids: Vec<String> = d.get(id(&s, "list")).unwrap().children.iter().map(|&k| d.node(k).unwrap().name.clone()).collect();
    assert_eq!(kids, ["k1", "k0"]);
}

#[test]
fn pressing_inside_a_selected_frame_drags_the_frame() {
    let mut s = session(json!([
        {"type": "frame", "name": "Screen", "x": 0, "y": 0, "width": 400, "height": 300, "fills": red(), "children": [
            {"type": "rectangle", "name": "Card", "x": 20, "y": 20, "width": 100, "height": 80, "fills": red()}
        ]}
    ]));
    let mut c = Controller::default();
    let screen = id(&s, "Screen");
    let card = id(&s, "Card");
    s.select(vec![screen]);
    // A drag starting on the card moves the selected screen, not the card.
    drag(&mut c, &mut s, &[at(50.0, 50.0), at(60.0, 60.0), at(80.0, 90.0)]);
    assert_eq!(s.selection(), &[screen]);
    assert_eq!((props(&s, screen).x, props(&s, screen).y), (30.0, 40.0));
    assert_eq!((props(&s, card).x, props(&s, card).y), (20.0, 20.0));
    // A click (no drag) on the card selects it.
    c.pointer_down(&mut s, at(80.0, 90.0), 1).unwrap();
    c.pointer_up(&mut s, at(80.0, 90.0)).unwrap();
    assert_eq!(s.selection(), &[card]);
}

#[test]
fn a_frames_empty_area_marquees_unless_the_frame_is_selected() {
    let mut s = session(json!([
        {"type": "frame", "name": "Screen", "x": 0, "y": 0, "width": 400, "height": 300, "fills": red(), "children": [
            {"type": "rectangle", "name": "Card", "x": 20, "y": 20, "width": 100, "height": 80, "fills": red()}
        ]}
    ]));
    let mut c = Controller::default();
    let screen = id(&s, "Screen");
    // Not selected: a drag from its empty area draws a marquee over its children.
    drag(&mut c, &mut s, &[at(200.0, 200.0), at(150.0, 150.0), at(100.0, 90.0)]);
    assert_eq!(s.selection(), &[id(&s, "Card")]);
    assert_eq!(props(&s, screen).x, 0.0);
    // Selected (by its label, or the layers panel): the same drag moves it.
    s.select(vec![screen]);
    drag(&mut c, &mut s, &[at(200.0, 200.0), at(210.0, 210.0), at(230.0, 240.0)]);
    assert_eq!(s.selection(), &[screen]);
    assert_eq!((props(&s, screen).x, props(&s, screen).y), (30.0, 40.0));
}

#[test]
fn right_click_selects_what_is_under_the_pointer() {
    let mut s = session(json!([
        {"type": "rectangle", "name": "a", "x": 0, "y": 0, "width": 50, "height": 50, "fills": red()},
        {"type": "rectangle", "name": "b", "x": 100, "y": 0, "width": 50, "height": 50, "fills": red()},
    ]));
    let mut c = Controller::default();
    let right = |p: Pointer| Pointer { button: editor::Button::Secondary, ..p };
    c.pointer_down(&mut s, right(at(10.0, 10.0)), 1).unwrap();
    assert_eq!(s.selection(), &[id(&s, "a")]);
    // Inside a selection it keeps the selection.
    s.select(vec![id(&s, "a"), id(&s, "b")]);
    c.pointer_down(&mut s, right(at(110.0, 10.0)), 1).unwrap();
    assert_eq!(s.selection().len(), 2);
    // On empty canvas it clears it.
    c.pointer_down(&mut s, right(at(500.0, 500.0)), 1).unwrap();
    assert!(s.selection().is_empty());
}
