//! Stage 7c scenarios: typing into a 2,000-character rich text inside the 10k-node auto-layout
//! tree, dragging a text selection, and drawing and editing a 200-point path, each timed from
//! the input event to a presented GPU frame (editor work, layout, scene update, frame build,
//! render and wait), driven through the controller as the app drives it.

use std::time::Instant;

use doc::testing::{GenConfig, STRESS_ROOT};
use doc::*;
use editor::commands::{Key, KeyMods};
use editor::{Camera, Controller, Pointer, Session, Tool};
use kurbo::{Point, Vec2};
use serde_json::{Value, json};
use text::FontDb;

use crate::harness::{Report, Scenario, ms, stats};
use crate::render::{Gpu, laid_out};

pub fn all() -> Vec<Box<dyn Scenario>> {
    vec![Box::new(CanvasEdit)]
}

/// About 2,000 characters in spans of mixed weight, style, size and colour.
fn rich_spans() -> Value {
    let words = ["design", "canvas", "layout", "vector", "paint", "type", "frame", "grid", "colour", "shape", "text", "edit"];
    let mut spans = Vec::new();
    let mut len = 0;
    let mut i = 0;
    while len < 2000 {
        let text: String = (0..8).map(|j| format!("{} ", words[(i * 7 + j * 3) % words.len()])).collect();
        len += text.len();
        let span = match i % 4 {
            0 => json!({"text": text}),
            1 => json!({"text": text, "fontWeight": 700}),
            2 => json!({"text": text, "italic": true, "color": "#2563eb"}),
            _ => json!({"text": text, "fontSize": 18}),
        };
        spans.push(span);
        i += 1;
    }
    Value::Array(spans)
}

/// Times one input step through to a presented frame (or just the editor work without a GPU).
fn step(gpu: &mut Option<Gpu>, s: &mut Session, ctl: &mut Controller, f: impl FnOnce(&mut Session, &mut Controller)) -> f64 {
    let t = Instant::now();
    f(s, ctl);
    // What the app draws on top: the caret and selection geometry.
    if let Some(e) = &ctl.text {
        std::hint::black_box(e.overlay(s));
    }
    if let Some(e) = &ctl.path {
        std::hint::black_box(e.overlay(s));
    }
    if let Some(g) = gpu {
        let cam = s.camera.affine();
        let (scene, doc) = s.scene_and_doc();
        g.frame(scene, doc, cam);
    }
    ms(t)
}

fn screen(s: &Session, world: Point) -> Pointer {
    Pointer { screen: s.camera.to_screen(world), ..Default::default() }
}

struct CanvasEdit;

impl Scenario for CanvasEdit {
    fn name(&self) -> &'static str {
        "canvas_edit"
    }
    fn describe(&self) -> &'static str {
        "Canvas editing to a GPU frame: typing in a 2,000-character rich text inside a 10k-node tree, text selection drag, pen and path edit on a 200-point path"
    }
    fn run(&self, r: &mut Report) {
        let mut gpu = Gpu::new();
        r.info("gpu_frames", gpu.is_some());
        let store = laid_out(&GenConfig { target_nodes: 10, auto_layout_heavy: true, ..Default::default() });
        let mut s = Session::new(store.into_doc(), None, FontDb::new(true)).expect("session");
        let root = s.doc().iter().find(|(_, n)| n.name == STRESS_ROOT).map(|(_, n)| n.id).expect("stress root");
        let page = editor::select::page_of(s.doc(), root).expect("page");
        s.set_page(page);
        s.viewport = Vec2::new(1920.0, 1080.0);
        let section = s.doc().get(root).and_then(|n| n.children.first()).and_then(|&k| s.doc().node(k)).map(|n| n.id).expect("section");
        let text = s
            .transact(TxMeta::user("rich text"), |tx| {
                tx.insert_json(Some(section), Some(0), json!({"type": "text", "name": "Rich", "sizingH": "fixed", "sizingV": "hug", "width": 640,
                    "style": {"fontFamily": "Inter", "fontSize": 14, "color": "#111111"}, "spans": rich_spans()}))
            })
            .expect("text");
        r.info("tree_nodes", s.doc().key(root).map_or(0, |k| s.doc().descendants(k).len()));
        let chars = match &s.doc().get(text).expect("text").kind {
            NodeKind::Text { spans, .. } => {
                r.info("text_spans", spans.len());
                richtext::plain(spans).chars().count()
            }
            _ => 0,
        };
        r.info("text_chars", chars);
        let b = s.scene.index.bounds(text).expect("text bounds");
        r.info("text_box", json!([b.width(), b.height()]));
        s.camera = Camera { zoom: 1.0, origin: b.origin() - Vec2::new(40.0, 40.0) };
        let mut ctl = Controller::default();

        // Typing: a caret in the middle of the text, then one character per keystroke (a word
        // per undo step), and Backspace.
        assert!(ctl.edit(&mut s, text, Some(b.center())));
        for _ in 0..3 {
            step(&mut gpu, &mut s, &mut ctl, |_, _| {});
        }
        let mut typing = Vec::new();
        for i in 0..240 {
            let ch = if i % 6 == 5 { " " } else { "e" };
            typing.push(step(&mut gpu, &mut s, &mut ctl, |s, c| drop(c.text_input(s, ch))));
        }
        r.metric("keystroke_to_frame_rich_text_10k_tree", stats(typing), Some(16.0), None);
        let mut back = Vec::new();
        for _ in 0..120 {
            back.push(step(&mut gpu, &mut s, &mut ctl, |s, c| drop(c.key(s, Key::Backspace, KeyMods::default()))));
        }
        r.metric("backspace_to_frame_rich_text_10k_tree", stats(back), Some(16.0), None);
        let mut comp = Vec::new();
        for i in 0..60 {
            let pre = ["に", "にほ", "にほん", "日本"][i % 4];
            comp.push(step(&mut gpu, &mut s, &mut ctl, |s, c| drop(c.ime_preedit(s, pre, None))));
            if i % 4 == 3 {
                comp.push(step(&mut gpu, &mut s, &mut ctl, |s, c| drop(c.ime_commit(s, "日本"))));
            }
        }
        r.metric("ime_composition_to_frame", stats(comp), Some(16.0), None);

        // Selection drag across the text, a frame per pointer move.
        let (p0, p1) = (Point::new(b.x0 + 2.0, b.y0 + 4.0), Point::new(b.x1 - 2.0, b.y1 - 4.0));
        let press = screen(&s, p0);
        ctl.pointer_down(&mut s, press, 1).expect("press");
        let mut drag = Vec::new();
        for i in 1..=120 {
            let p = p0.lerp(p1, i as f64 / 120.0);
            drag.push(step(&mut gpu, &mut s, &mut ctl, |s, c| drop(c.pointer_move(s, screen(s, p)))));
        }
        let release = screen(&s, p1);
        let _ = ctl.pointer_up(&mut s, release);
        r.metric("text_selection_drag_frame", stats(drag), Some(16.0), None);
        r.info("selection_bytes", ctl.text.as_ref().map_or(0, |t| t.range().1 - t.range().0));
        let _ = ctl.end_edits(&mut s);

        // The pen: 200 points (every fifth dragged out into a curve) beside the tree.
        let origin = Point::new(b.x1 + 200.0, b.y0);
        s.camera = Camera { zoom: 1.0, origin: origin - Vec2::new(100.0, 100.0) };
        s.snap_to_objects = false;
        ctl.set_tool(Tool::Pen);
        let at = |i: usize| origin + Vec2::new((i % 20) as f64 * 40.0, (i / 20) as f64 * 40.0 + if i.is_multiple_of(2) { 0.0 } else { 15.0 });
        let mut add = Vec::new();
        for i in 0..200 {
            let p = at(i);
            add.push(step(&mut gpu, &mut s, &mut ctl, |s, c| {
                let _ = c.pointer_down(s, screen(s, p), 1);
                if i.is_multiple_of(5) {
                    let _ = c.pointer_move(s, screen(s, p + Vec2::new(12.0, 8.0)));
                }
                let _ = c.pointer_up(s, screen(s, p));
            }));
        }
        r.metric("pen_point_add_to_frame", stats(add.clone()), Some(16.0), None);
        r.metric("pen_point_add_at_180_to_200_points", stats(add[180..].to_vec()), Some(16.0), None);
        let path = ctl.pen.as_ref().and_then(|p| p.node).expect("pen path");
        let _ = ctl.key(&mut s, Key::Enter, KeyMods::default());
        let d_len = match &s.doc().get(path).expect("path").kind {
            NodeKind::Path { paths } => paths.iter().map(|p| p.d.len()).sum::<usize>(),
            _ => 0,
        };
        r.info("pen_path_d_bytes", d_len);

        // Path edit: drag one point of the 200-point path, a frame per pointer move.
        assert!(ctl.edit(&mut s, path, None));
        let grab = at(57);
        let p = screen(&s, grab);
        let t = Instant::now();
        let _ = ctl.pointer_down(&mut s, p, 1);
        let _ = ctl.pointer_up(&mut s, p);
        r.info("path_edit_select_point_ms", ms(t));
        let _ = ctl.pointer_down(&mut s, p, 1);
        let mut moves = Vec::new();
        for i in 1..=120 {
            let q = grab + Vec2::new(i as f64 * 0.8, -(i as f64) * 0.5);
            moves.push(step(&mut gpu, &mut s, &mut ctl, |s, c| drop(c.pointer_move(s, screen(s, q)))));
        }
        let _ = ctl.pointer_up(&mut s, p);
        r.metric("path_point_drag_200_point_path_frame", stats(moves), Some(16.0), None);
        let undo = Instant::now();
        let _ = s.undo();
        r.info("path_drag_undo_ms", ms(undo));
    }
}
