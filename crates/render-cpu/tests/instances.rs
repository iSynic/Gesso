//! An instance draws exactly what its detached copy draws: random components (with constraints,
//! a nested instance and text) and random overrides, shown by an instance at a random size, render
//! pixel-identical before and after `detach`. Case count: `PROPTEST_CASES` (default 48).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;

use doc::*;
use kurbo::Affine;
use layout::LayoutDeriver;
use proptest::prelude::*;
use render::{NoAssets, Scene};
use serde_json::{Value, json};
use text::{FontDb, TextEngine};

const COLORS: [&str; 6] = ["#ef4444", "#22c55e", "#3b82f6", "#f59e0b80", "#111827", "#ffffff"];
const H: [&str; 5] = ["left", "right", "leftRight", "center", "scale"];
const V: [&str; 5] = ["top", "bottom", "topBottom", "center", "scale"];
const INNER: &str = "0Cm00000000000000000N1";
const INNER_DOT: &str = "0Rc00000000000000000D1";
const NESTED: &str = "0In00000000000000000N2";

/// One child of the random component: (kind, x, y, w, h, colour, h-constraint, v-constraint).
type Child = (u8, u8, u8, u8, u8, u8, u8, u8);
/// One override: (what, which child, value).
type Ov = (u8, u8, u8);

fn child_json(i: usize, (kind, x, y, w, h, c, ch, cv): Child) -> Value {
    let id = format!("0Rc0000000000000000{i:03}");
    let (x, y, w, h) = (f64::from(x % 100), f64::from(y % 60), f64::from(w % 50 + 4), f64::from(h % 30 + 4));
    let mut n = json!({"id": id, "name": format!("C{i}"), "x": x, "y": y, "width": w, "height": h,
        "fills": [{"type": "solid", "color": COLORS[c as usize % COLORS.len()]}],
        "constraints": {"h": H[ch as usize % H.len()], "v": V[cv as usize % V.len()]}});
    let o = n.as_object_mut().unwrap();
    match kind % 3 {
        0 => o.insert("type".into(), json!("rectangle")),
        1 => o.insert("type".into(), json!("ellipse")),
        _ => {
            o.insert("type".into(), json!("text"));
            o.remove("fills");
            o.insert("spans".into(), json!([{"text": "Ag"}]));
            o.insert("autoResize".into(), json!("widthAndHeight"))
        }
    };
    n
}

fn override_json(children: usize, (what, which, v): Ov) -> Value {
    let target = json!([format!("0Rc0000000000000000{:03}", which as usize % children)]);
    let color = COLORS[v as usize % COLORS.len()];
    match what % 6 {
        0 => json!({"target": [], "prop": "fills", "value": [{"type": "solid", "color": color}]}),
        1 => json!({"target": target, "prop": "visible", "value": v % 2 == 0}),
        2 => json!({"target": target, "prop": "opacity", "value": f64::from(v % 5) / 4.0}),
        3 => json!({"target": [NESTED, INNER_DOT], "prop": "fills", "value": [{"type": "solid", "color": color}]}),
        4 => json!({"target": [NESTED], "prop": "radius", "value": f64::from(v % 12)}),
        _ => json!({"target": target, "prop": "fills", "value": [{"type": "solid", "color": color}]}),
    }
}

fn build(children: &[Child], overrides: &[Ov], (w, h): (u8, u8)) -> (Store, NodeId, NodeId) {
    let mut s = Store::default().with_ids(IdGen::seeded(9));
    s.set_deriver(Box::new(LayoutDeriver::new(TextEngine::new(FontDb::bundled_only()))));
    let page = s.transact(TxMeta::user("page"), |tx| tx.insert_json(None, None, json!({"type": "page"}))).unwrap().0;
    let mut kids: Vec<Value> = children.iter().enumerate().map(|(i, c)| child_json(i, *c)).collect();
    kids.push(json!({"id": NESTED, "type": "instance", "name": "Nested", "component": INNER, "x": 40, "y": 20, "width": 30, "height": 20,
        "constraints": {"h": "center", "v": "bottom"}}));
    // One override per target and property, as the format requires.
    let mut ovs: Vec<Value> = Vec::new();
    for o in overrides.iter().map(|o| override_json(children.len(), *o)) {
        if !ovs.iter().any(|x| x["target"] == o["target"] && x["prop"] == o["prop"]) {
            ovs.push(o);
        }
    }
    let inst = s
        .transact(TxMeta::user("setup"), |tx| {
            tx.insert_json(Some(page), None, json!({"id": INNER, "type": "component", "name": "Inner", "y": 400, "width": 30, "height": 20, "fills": [{"type": "solid", "color": "#e5e7eb"}],
                "children": [{"id": INNER_DOT, "type": "ellipse", "name": "Dot", "x": 4, "y": 4, "width": 12, "height": 12, "constraints": {"h": "scale", "v": "scale"}, "fills": [{"type": "solid", "color": "#7c3aed"}]}]}))?;
            tx.insert_json(Some(page), None, json!({"type": "component", "name": "Main", "x": 300, "y": 400, "width": 120, "height": 80, "radius": 8,
                "fills": [{"type": "solid", "color": "#dbeafe"}], "strokes": [{"paint": {"type": "solid", "color": "#1e3a8a"}, "weight": 2}], "children": kids}))?;
            let main = tx.doc().iter().find(|(_, n)| n.name == "Main").map(|(_, n)| n.id).unwrap();
            tx.insert_json(Some(page), None, json!({"type": "instance", "name": "Use", "component": main.to_string(), "x": 10, "y": 10,
                "width": f64::from(w % 200 + 20), "height": f64::from(h % 120 + 20), "opacity": 0.9, "overrides": ovs}))
        })
        .unwrap()
        .0;
    (s, page, inst)
}

fn shot(doc: &Document, page: NodeId) -> Vec<u8> {
    let mut sc = Scene::new(doc, page, FontDb::bundled_only(), Arc::new(NoAssets));
    common::render_frame(&sc.frame(doc, Affine::IDENTITY, 240, 160, None), 0)
}

fn config() -> ProptestConfig {
    let cases = std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(48);
    ProptestConfig { cases, ..ProptestConfig::default() }
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn instance_renders_like_its_detached_copy(
        children in prop::collection::vec(any::<Child>(), 1..5),
        overrides in prop::collection::vec(any::<Ov>(), 0..6),
        size in any::<(u8, u8)>(),
    ) {
        let (mut s, page, inst) = build(&children, &overrides, size);
        let before = shot(s.doc(), page);
        // The instance is drawn (its stroke at least), so the comparison means something.
        prop_assert!(before.chunks(4).any(|p| p[..3] != before[..3]), "nothing drawn");
        s.transact(TxMeta::user("detach"), |tx| tx.detach(inst)).unwrap();
        s.doc().check_invariants().unwrap();
        let after = shot(s.doc(), page);
        let d = common::diff(&before, &after, 0);
        prop_assert!(d.over == 0, "{} pixels differ (max {}) after detach", d.over, d.max);
        // Undoing the detach brings the instance back, drawn the same.
        s.undo().unwrap();
        prop_assert_eq!(shot(s.doc(), page), before);
    }
}
