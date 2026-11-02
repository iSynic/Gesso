//! Text editing on the canvas, the pen tool and path edit mode, driven through the controller
//! the way the desktop shell drives it (screen points, keys, text and composition events).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use doc::model::TextSpan;
use doc::{Document, NodeId, NodeKind, Store, TxMeta};
use editor::commands::{self, Command, Key, KeyMods};
use editor::path_edit;
use editor::transform::Mods;
use editor::{Controller, Pointer, Session, Tool};
use kurbo::{Point, Vec2};
use serde_json::{Value, json};
use text::FontDb;

fn session(children: Value) -> Session {
    let mut s = Store::new(Document::new());
    s.transact(TxMeta::user("setup"), |tx| tx.insert_json(None, None, json!({"type": "page"}))).unwrap();
    let mut s = Session::new(s.into_doc(), None, FontDb::bundled_only()).unwrap();
    // Inserted through the session, so the layout deriver sizes hugging text.
    let page = s.page();
    s.transact(TxMeta::user("setup"), |tx| {
        for c in children.as_array().into_iter().flatten() {
            tx.insert_json(Some(page), None, c.clone())?;
        }
        Ok(())
    })
    .unwrap();
    s.viewport = Vec2::new(1000.0, 1000.0);
    // Screen = world + (100, 100) at 100 %.
    s.camera = editor::Camera { zoom: 1.0, origin: Point::new(-100.0, -100.0) };
    s.snap_to_objects = false;
    s.snap_to_pixels = false;
    s
}

fn id(s: &Session, name: &str) -> NodeId {
    s.doc().iter().find(|(_, n)| n.name == name).map(|(_, n)| n.id).unwrap_or_else(|| panic!("no node {name}"))
}

fn at(x: f64, y: f64) -> Pointer {
    Pointer { screen: Point::new(x + 100.0, y + 100.0), ..Default::default() }
}

fn spans(s: &Session, id: NodeId) -> Vec<TextSpan> {
    match &s.doc().get(id).unwrap().kind {
        NodeKind::Text { spans, .. } => spans.clone(),
        _ => panic!("not text"),
    }
}

fn plain(s: &Session, id: NodeId) -> String {
    doc::richtext::plain(&spans(s, id))
}

fn key(c: &mut Controller, s: &mut Session, k: Key, m: KeyMods) {
    if !c.key(s, k, m).unwrap()
        && let Some(cmd) = commands::shortcut(k, m)
    {
        commands::execute(s, cmd).unwrap();
    }
}

fn ctrl() -> KeyMods {
    KeyMods { ctrl: true, ..Default::default() }
}

fn shift() -> KeyMods {
    KeyMods { shift: true, ..Default::default() }
}

fn text_doc() -> Session {
    session(json!([{"type": "text", "name": "t", "x": 0, "y": 0, "sizingH": "hug", "sizingV": "hug",
        "style": {"fontFamily": "Inter", "fontSize": 20, "color": "#000000"},
        "spans": [{"text": "Hello "}, {"text": "world", "fontWeight": 700}]}]))
}

#[test]
fn double_click_edits_text_and_typing_coalesces_by_word() {
    let mut s = text_doc();
    let t = id(&s, "t");
    let mut c = Controller::default();
    c.pointer_down(&mut s, at(5.0, 10.0), 1).unwrap();
    c.pointer_up(&mut s, at(5.0, 10.0)).unwrap();
    assert_eq!(s.selection(), &[t]);
    c.pointer_down(&mut s, at(5.0, 10.0), 2).unwrap();
    c.pointer_up(&mut s, at(5.0, 10.0)).unwrap();
    let e = c.text.as_ref().expect("editing");
    // The double click selected the word under the pointer.
    assert_eq!(e.range(), (0, 5));
    c.text_input(&mut s, "Goodbye").unwrap();
    assert_eq!(plain(&s, t), "Goodbye world");
    // Typing at the end of the bold word continues in bold.
    key(&mut c, &mut s, Key::End, KeyMods::default());
    c.text_input(&mut s, "s").unwrap();
    c.text_input(&mut s, " and").unwrap();
    c.text_input(&mut s, " more").unwrap();
    let sp = spans(&s, t);
    assert_eq!(sp[1].text, "worlds and more");
    assert_eq!(sp[1].font_weight, Some(700));
    // One undo step per word: " more", " and", "s", then "Goodbye".
    key(&mut c, &mut s, Key::Char('z'), ctrl());
    assert_eq!(plain(&s, t), "Goodbye worlds and");
    key(&mut c, &mut s, Key::Char('z'), ctrl());
    key(&mut c, &mut s, Key::Char('z'), ctrl());
    assert_eq!(plain(&s, t), "Goodbye world");
    key(&mut c, &mut s, Key::Char('z'), ctrl());
    assert_eq!(plain(&s, t), "Hello world");
    c.sync(&s);
    let e = c.text.as_ref().unwrap();
    assert!(e.cursor <= "Hello world".len());
}

#[test]
fn keys_move_select_and_delete() {
    let mut s = text_doc();
    let t = id(&s, "t");
    let mut c = Controller::default();
    s.select(vec![t]);
    key(&mut c, &mut s, Key::Enter, KeyMods::default());
    assert_eq!(c.text.as_ref().unwrap().range(), (0, 11), "Enter selects all");
    key(&mut c, &mut s, Key::Home, KeyMods::default());
    key(&mut c, &mut s, Key::Right, ctrl());
    assert_eq!(c.text.as_ref().unwrap().cursor, 5);
    key(&mut c, &mut s, Key::Right, KeyMods { ctrl: true, shift: true, alt: false });
    assert_eq!(c.text.as_ref().unwrap().range(), (5, 11));
    key(&mut c, &mut s, Key::Backspace, KeyMods::default());
    assert_eq!(plain(&s, t), "Hello");
    key(&mut c, &mut s, Key::Backspace, ctrl());
    assert_eq!(plain(&s, t), "");
    // Text typed into an emptied node keeps the style it had.
    c.text_input(&mut s, "x").unwrap();
    assert_eq!(spans(&s, t), vec![TextSpan { text: "x".into(), ..Default::default() }]);
    key(&mut c, &mut s, Key::Backspace, KeyMods::default());
    // Leaving an empty text deletes it.
    key(&mut c, &mut s, Key::Escape, KeyMods::default());
    assert!(c.text.is_none());
    assert!(s.doc().get(t).is_none());
}

#[test]
fn line_navigation_and_newlines() {
    let mut s = session(json!([{"type": "text", "name": "t", "x": 0, "y": 0, "sizingH": "hug", "sizingV": "hug",
        "style": {"fontFamily": "Inter", "fontSize": 20, "color": "#000000"}, "spans": [{"text": "abc\ndefgh\nij"}]}]));
    let t = id(&s, "t");
    let mut c = Controller::default();
    assert!(c.edit(&mut s, t, None));
    key(&mut c, &mut s, Key::Home, ctrl());
    key(&mut c, &mut s, Key::Right, KeyMods::default());
    key(&mut c, &mut s, Key::Down, KeyMods::default());
    // From after "a" down to after "d".
    assert_eq!(c.text.as_ref().unwrap().cursor, 5);
    key(&mut c, &mut s, Key::End, KeyMods::default());
    assert_eq!(c.text.as_ref().unwrap().cursor, 9);
    key(&mut c, &mut s, Key::Down, KeyMods::default());
    key(&mut c, &mut s, Key::Down, KeyMods::default());
    assert_eq!(c.text.as_ref().unwrap().cursor, 12);
    key(&mut c, &mut s, Key::Enter, KeyMods::default());
    assert_eq!(plain(&s, t), "abc\ndefgh\nij\n");
    // Ctrl+Enter leaves the edit, keeping the text selected.
    key(&mut c, &mut s, Key::Enter, ctrl());
    assert!(c.text.is_none());
    assert_eq!(s.selection(), &[t]);
}

#[test]
fn bold_italic_underline_toggle_on_ranges_and_caret() {
    let mut s = text_doc();
    let t = id(&s, "t");
    let mut c = Controller::default();
    assert!(c.edit(&mut s, t, None));
    // All selected: mixed weights, so Ctrl+B makes everything bold, then plain again.
    key(&mut c, &mut s, Key::Char('b'), ctrl());
    assert_eq!(spans(&s, t), vec![TextSpan { text: "Hello world".into(), font_weight: Some(700), ..Default::default() }]);
    key(&mut c, &mut s, Key::Char('b'), ctrl());
    assert_eq!(spans(&s, t), vec![TextSpan { text: "Hello world".into(), ..Default::default() }]);
    key(&mut c, &mut s, Key::Char('z'), ctrl());
    key(&mut c, &mut s, Key::Char('z'), ctrl());
    assert_eq!(spans(&s, t)[1].font_weight, Some(700));
    // With a caret, Ctrl+I applies to what is typed next.
    key(&mut c, &mut s, Key::Home, ctrl());
    key(&mut c, &mut s, Key::Char('i'), ctrl());
    c.text_input(&mut s, "Oh ").unwrap();
    let sp = spans(&s, t);
    assert_eq!((sp[0].text.as_str(), sp[0].italic), ("Oh ", Some(true)));
    assert_eq!((sp[1].text.as_str(), sp[1].italic), ("Hello ", None));
    // Underline a range with Shift+arrows.
    key(&mut c, &mut s, Key::Right, shift());
    key(&mut c, &mut s, Key::Right, shift());
    key(&mut c, &mut s, Key::Char('u'), ctrl());
    let sp = spans(&s, t);
    assert_eq!((sp[1].text.as_str(), sp[1].decoration), ("He", Some(doc::model::TextDecoration::Underline)));
    let style = c.text.as_ref().unwrap().current_style(&s).unwrap();
    assert_eq!(style.decoration, Some(doc::model::TextDecoration::Underline));
    assert_eq!(style.font_size, Some(20.0));
}

#[test]
fn composition_reflows_and_commits_once() {
    let mut s = text_doc();
    let t = id(&s, "t");
    let mut c = Controller::default();
    assert!(c.edit(&mut s, t, None));
    key(&mut c, &mut s, Key::End, ctrl());
    let w0 = s.doc().get(t).unwrap().props.width;
    c.ime_preedit(&mut s, " にほ", Some(4)).unwrap();
    assert_eq!(plain(&s, t), "Hello world にほ");
    let o = c.text.as_ref().unwrap().overlay(&mut s).unwrap();
    assert!(!o.preedit.is_empty(), "the composition is underlined");
    // Keys belong to the input method while composing.
    key(&mut c, &mut s, Key::Backspace, KeyMods::default());
    assert_eq!(plain(&s, t), "Hello world にほ");
    c.ime_preedit(&mut s, " 日本", None).unwrap();
    assert_eq!(plain(&s, t), "Hello world 日本");
    c.ime_commit(&mut s, " 日本語").unwrap();
    assert_eq!(plain(&s, t), "Hello world 日本語");
    assert!(s.doc().get(t).unwrap().props.width > w0, "the hugging text grew");
    // Cancelling a composition leaves nothing behind.
    c.ime_preedit(&mut s, "x", None).unwrap();
    c.ime_preedit(&mut s, "", None).unwrap();
    assert_eq!(plain(&s, t), "Hello world 日本語");
    // The composition is not an undo step; the commit is one.
    key(&mut c, &mut s, Key::Char('z'), ctrl());
    assert_eq!(plain(&s, t), "Hello world");
}

#[test]
fn clicking_and_dragging_select_text() {
    let mut s = text_doc();
    let t = id(&s, "t");
    let mut c = Controller::default();
    assert!(c.edit(&mut s, t, Some(Point::new(1.0, 10.0))));
    assert_eq!(c.text.as_ref().unwrap().range(), (0, 0));
    let w = s.doc().get(t).unwrap().props.width;
    c.pointer_down(&mut s, at(1.0, 10.0), 1).unwrap();
    c.pointer_move(&mut s, at(w - 1.0, 10.0)).unwrap();
    c.pointer_up(&mut s, at(w - 1.0, 10.0)).unwrap();
    assert_eq!(c.text.as_ref().unwrap().range(), (0, 11));
    let o = c.text.as_ref().unwrap().overlay(&mut s).unwrap();
    assert!(!o.selection.is_empty() && o.caret.is_none());
    // A click outside ends the edit.
    c.pointer_down(&mut s, at(500.0, 500.0), 1).unwrap();
    c.pointer_up(&mut s, at(500.0, 500.0)).unwrap();
    assert!(c.text.is_none());
}

#[test]
fn text_tool_creates_and_types() {
    let mut s = session(json!([]));
    let mut c = Controller::default();
    c.set_tool(Tool::Text);
    c.pointer_down(&mut s, at(0.0, 0.0), 1).unwrap();
    c.pointer_up(&mut s, at(0.0, 0.0)).unwrap();
    let t = s.selection()[0];
    c.text_input(&mut s, "Title").unwrap();
    assert_eq!(plain(&s, t), "Title");
    key(&mut c, &mut s, Key::Escape, KeyMods::default());
    // Creating and typing are separate steps.
    commands::execute(&mut s, Command::Undo).unwrap();
    assert_eq!(plain(&s, t), "Text");
}

#[test]
fn copy_cut_paste_text() {
    let mut s = text_doc();
    let t = id(&s, "t");
    let mut c = Controller::default();
    assert!(c.edit(&mut s, t, None));
    let e = c.text.as_mut().unwrap();
    assert_eq!(e.selected_text(&s), "Hello world");
    key(&mut c, &mut s, Key::Home, ctrl());
    key(&mut c, &mut s, Key::Right, KeyMods { ctrl: true, shift: true, alt: false });
    let e = c.text.as_mut().unwrap();
    assert_eq!(e.cut(&mut s).unwrap(), "Hello");
    e.paste(&mut s, "Bye\r\nnow").unwrap();
    assert_eq!(plain(&s, t), "Bye\nnow world");
}

fn path_d(s: &Session, id: NodeId) -> String {
    match &s.doc().get(id).unwrap().kind {
        NodeKind::Path { paths } => paths[0].d.clone(),
        _ => panic!("not a path"),
    }
}

#[test]
fn pen_draws_corners_and_curves_and_closes() {
    let mut s = session(json!([]));
    let mut c = Controller::default();
    key(&mut c, &mut s, Key::Char('p'), KeyMods::default());
    // shortcut() gives Tool(Pen); the shell sets it.
    c.set_tool(Tool::Pen);
    c.pointer_down(&mut s, at(10.0, 10.0), 1).unwrap();
    c.pointer_up(&mut s, at(10.0, 10.0)).unwrap();
    c.pointer_move(&mut s, at(100.0, 10.0)).unwrap();
    // Press and drag: a smooth point with mirrored handles.
    c.pointer_down(&mut s, at(110.0, 10.0), 1).unwrap();
    c.pointer_move(&mut s, at(110.0, 40.0)).unwrap();
    c.pointer_up(&mut s, at(110.0, 40.0)).unwrap();
    c.pointer_down(&mut s, at(10.0, 110.0), 1).unwrap();
    c.pointer_up(&mut s, at(10.0, 110.0)).unwrap();
    let p = c.pen.as_ref().unwrap().node.unwrap();
    // Close on the first point.
    c.pointer_down(&mut s, at(12.0, 11.0), 1).unwrap();
    c.pointer_up(&mut s, at(12.0, 11.0)).unwrap();
    assert!(c.pen.is_none());
    assert_eq!(c.tool, Tool::Move);
    assert_eq!(s.selection(), &[p]);
    let n = s.doc().get(p).unwrap();
    let o = Point::new(n.props.x, n.props.y) - Point::ZERO;
    let d = path_d(&s, p);
    assert!(d.starts_with("M") && d.ends_with(" Z"), "{d}");
    let c0 = &path_edit::parse(&d)[0];
    assert!(c0.closed && c0.anchors.len() == 3);
    let a1 = c0.anchors[1];
    assert!(a1.is_smooth());
    assert_eq!((c0.anchors[0].p + o, a1.p + o), (Point::new(10.0, 10.0), Point::new(110.0, 10.0)));
    assert_eq!((a1.h_out.unwrap() + o, a1.h_in.unwrap() + o), (Point::new(110.0, 40.0), Point::new(110.0, -20.0)));
    // The curve rises above the first point, so the box starts higher.
    assert!(n.props.y < 10.0);
    // The whole path is one undo step.
    commands::execute(&mut s, Command::Undo).unwrap();
    assert!(s.doc().get(p).is_none());
}

#[test]
fn pen_with_one_point_leaves_nothing() {
    let mut s = session(json!([]));
    let mut c = Controller::default();
    c.set_tool(Tool::Pen);
    c.pointer_down(&mut s, at(10.0, 10.0), 1).unwrap();
    c.pointer_up(&mut s, at(10.0, 10.0)).unwrap();
    key(&mut c, &mut s, Key::Escape, KeyMods::default());
    assert!(c.pen.is_none());
    assert_eq!(s.doc().iter().filter(|(_, n)| matches!(n.kind, NodeKind::Path { .. })).count(), 0);
}

#[test]
fn path_edit_moves_adds_deletes_and_toggles_points() {
    let mut s = session(json!([{"type": "path", "name": "p", "x": 100, "y": 100, "width": 100, "height": 100,
        "paths": [{"d": "M0 0 L100 0 L100 100 L0 100 Z"}], "strokes": [{"paint": {"type": "solid", "color": "#000000"}, "weight": 1}]}]));
    let p = id(&s, "p");
    let mut c = Controller::default();
    s.select(vec![p]);
    key(&mut c, &mut s, Key::Enter, KeyMods::default());
    assert!(c.path.is_some());
    // Drag the corner at (200, 100) to (240, 80): the box grows and moves up.
    c.pointer_down(&mut s, at(200.0, 100.0), 1).unwrap();
    c.pointer_move(&mut s, at(220.0, 90.0)).unwrap();
    c.pointer_move(&mut s, at(240.0, 80.0)).unwrap();
    c.pointer_up(&mut s, at(240.0, 80.0)).unwrap();
    let n = s.doc().get(p).unwrap();
    assert_eq!((n.props.x, n.props.y, n.props.width, n.props.height), (100.0, 80.0, 140.0, 120.0));
    assert_eq!(path_d(&s, p), "M0 20 L140 0 L100 120 L0 120 Z");
    // One undo step for the drag.
    commands::execute(&mut s, Command::Undo).unwrap();
    c.sync(&s);
    assert_eq!(path_d(&s, p), "M0 0 L100 0 L100 100 L0 100 Z");
    commands::execute(&mut s, Command::Redo).unwrap();
    c.sync(&s);
    // A press on a segment adds a point there (the left edge, x = 100 on the canvas).
    c.pointer_down(&mut s, at(100.0, 170.0), 1).unwrap();
    c.pointer_up(&mut s, at(100.0, 170.0)).unwrap();
    let k = &path_edit::parse(&path_d(&s, p))[0];
    assert_eq!(k.anchors.len(), 5);
    assert_eq!(c.path.as_ref().unwrap().selected.len(), 1);
    // Double-click turns it smooth, then a corner again.
    c.pointer_down(&mut s, at(100.0, 170.0), 2).unwrap();
    c.pointer_up(&mut s, at(100.0, 170.0)).unwrap();
    let k = &path_edit::parse(&path_d(&s, p))[0];
    assert!(k.anchors[4].is_smooth(), "{:?}", k.anchors[4]);
    c.pointer_down(&mut s, at(100.0, 170.0), 2).unwrap();
    c.pointer_up(&mut s, at(100.0, 170.0)).unwrap();
    assert!(path_edit::parse(&path_d(&s, p))[0].anchors[4].h_in.is_none(), "{} {:?}", path_d(&s, p), s.doc().get(p).unwrap().props);
    // Delete removes the selected point; arrows nudge.
    key(&mut c, &mut s, Key::Delete, KeyMods::default());
    assert_eq!(path_edit::parse(&path_d(&s, p))[0].anchors.len(), 4);
    c.pointer_down(&mut s, at(240.0, 80.0), 1).unwrap();
    c.pointer_up(&mut s, at(240.0, 80.0)).unwrap();
    key(&mut c, &mut s, Key::Right, shift());
    assert_eq!(s.doc().get(p).unwrap().props.width, 150.0);
    key(&mut c, &mut s, Key::Escape, KeyMods::default());
    assert!(c.path.is_none());
    assert_eq!(s.selection(), &[p]);
}

#[test]
fn smooth_handles_stay_opposite_unless_alt() {
    let mut s = session(json!([{"type": "path", "name": "p", "x": 0, "y": 0, "width": 200, "height": 0,
        "paths": [{"d": "M0 0 C0 0 50 0 100 0 C150 0 200 0 200 0"}]}]));
    let p = id(&s, "p");
    let mut c = Controller::default();
    assert!(c.edit(&mut s, p, None));
    // Select the middle point, then drag its out handle up.
    c.pointer_down(&mut s, at(100.0, 0.0), 1).unwrap();
    c.pointer_up(&mut s, at(100.0, 0.0)).unwrap();
    c.pointer_down(&mut s, at(150.0, 0.0), 1).unwrap();
    c.pointer_move(&mut s, at(150.0, -50.0)).unwrap();
    c.pointer_up(&mut s, at(150.0, -50.0)).unwrap();
    let a = path_edit::parse(&path_d(&s, p))[0].anchors[1];
    assert!(a.is_smooth(), "{a:?}");
    // With Alt the other handle stays.
    let before = a.h_in;
    let n = s.doc().get(p).unwrap();
    let origin = Point::new(n.props.x, n.props.y);
    let h = origin + (a.h_out.unwrap() - Point::ZERO);
    c.pointer_down(&mut s, at(h.x, h.y), 1).unwrap();
    c.pointer_move(&mut s, Pointer { mods: Mods { alt: true, ..Default::default() }, ..at(h.x + 30.0, h.y + 60.0) }).unwrap();
    c.pointer_up(&mut s, at(h.x + 30.0, h.y + 60.0)).unwrap();
    let n = s.doc().get(p).unwrap();
    let shift = Point::new(n.props.x, n.props.y) - origin;
    let a = path_edit::parse(&path_d(&s, p))[0].anchors[1];
    assert!(a.h_in.zip(before).is_some_and(|(q, b)| (q + shift).distance(b) < 1e-6), "{a:?} {before:?}");
    assert!(!a.is_smooth());
}

#[test]
fn tab_and_enter_navigate_the_tree() {
    let mut s = session(json!([
        {"type": "frame", "name": "f", "x": 0, "y": 0, "width": 300, "height": 300, "children": [
            {"type": "rectangle", "name": "a", "x": 0, "y": 0, "width": 10, "height": 10},
            {"type": "rectangle", "name": "b", "x": 20, "y": 0, "width": 10, "height": 10}
        ]},
        {"type": "rectangle", "name": "c", "x": 400, "y": 0, "width": 10, "height": 10}
    ]));
    let (f, a, b, c_) = (id(&s, "f"), id(&s, "a"), id(&s, "b"), id(&s, "c"));
    let mut c = Controller::default();
    key(&mut c, &mut s, Key::Tab, KeyMods::default());
    assert_eq!(s.selection(), &[f]);
    key(&mut c, &mut s, Key::Tab, KeyMods::default());
    assert_eq!(s.selection(), &[c_]);
    key(&mut c, &mut s, Key::Tab, shift());
    assert_eq!(s.selection(), &[f]);
    key(&mut c, &mut s, Key::Enter, KeyMods::default());
    assert_eq!(s.selection(), &[a, b]);
    key(&mut c, &mut s, Key::Tab, KeyMods::default());
    assert_eq!(s.selection(), &[b]);
    key(&mut c, &mut s, Key::Tab, KeyMods::default());
    assert_eq!(s.selection(), &[a]);
    key(&mut c, &mut s, Key::Enter, shift());
    assert_eq!(s.selection(), &[f]);
    key(&mut c, &mut s, Key::Enter, shift());
    assert_eq!(s.selection(), &[f], "never the page");
}

#[test]
fn text_inside_an_instance_edits_as_an_override() {
    let mut s = session(json!([
        {"id": "0Cm00000000000000000C1", "type": "component", "name": "Btn", "width": 120, "height": 40, "children": [
            {"type": "text", "name": "Label", "x": 10, "y": 10, "sizingH": "hug", "sizingV": "hug", "spans": [{"text": "Buy"}]}
        ]},
        {"type": "instance", "name": "Use", "component": "0Cm00000000000000000C1", "y": 100, "width": 120, "height": 40}
    ]));
    let label = s.doc().iter().find(|(_, n)| n.name == "Label" && n.virt.is_some()).map(|(_, n)| n.id).unwrap();
    let main = s.doc().iter().find(|(_, n)| n.name == "Label" && n.virt.is_none()).map(|(_, n)| n.id).unwrap();
    let mut c = Controller::default();
    assert!(c.edit(&mut s, label, None));
    c.text_input(&mut s, "Sell").unwrap();
    assert_eq!(plain(&s, label), "Sell");
    assert_eq!(plain(&s, main), "Buy", "the component is unchanged");
    let inst = id(&s, "Use");
    let NodeKind::Instance { overrides, .. } = &s.doc().get(inst).unwrap().kind else { panic!() };
    assert_eq!(overrides.len(), 1);
    // The virtual node keeps its id, so editing goes on.
    c.sync(&s);
    key(&mut c, &mut s, Key::Backspace, KeyMods::default());
    assert_eq!(plain(&s, label), "Sel");
}

#[test]
fn accessible_nodes_follow_the_selection() {
    let mut s = session(json!([
        {"type": "frame", "name": "f", "x": 0, "y": 0, "width": 300, "height": 300, "children": [
            {"type": "text", "name": "Title", "x": 0, "y": 0, "spans": [{"text": "Hello"}]},
            {"type": "ellipse", "name": "", "x": 20, "y": 40, "width": 10, "height": 10}
        ]},
        {"type": "rectangle", "name": "c", "x": 4000, "y": 0, "width": 10, "height": 10}
    ]));
    let nodes = editor::a11y::canvas_nodes(&s);
    assert_eq!(nodes.iter().map(|n| n.label.as_str()).collect::<Vec<_>>(), ["Frame \u{201c}f\u{201d}", "Rectangle \u{201c}c\u{201d}"]);
    assert!(nodes[0].bounds.is_some() && nodes[1].bounds.is_none(), "off-screen layers have no bounds");
    assert!(nodes[0].has_children && !nodes[0].selected);
    let t = id(&s, "Title");
    s.select(vec![t]);
    let nodes = editor::a11y::canvas_nodes(&s);
    assert_eq!(nodes.len(), 2);
    assert_eq!((nodes[0].role, nodes[0].value.as_deref(), nodes[0].selected), (editor::a11y::Role::Text, Some("Hello"), true));
    assert_eq!((nodes[1].role, nodes[1].label.as_str()), (editor::a11y::Role::Shape, "Ellipse"));
}
