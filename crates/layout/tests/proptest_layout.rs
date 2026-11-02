//! The key invariant: after any sequence of edits, the incrementally laid-out document is what a
//! from-scratch layout produces (a fresh deriver's full pass writes nothing), and undo/redo
//! restore every rect exactly. Case count: `PROPTEST_CASES` (default 256).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use doc::model::Sizing;
use doc::*;
use layout::LayoutDeriver;
use proptest::prelude::*;
use serde_json::{Value, json};
use text::{FontDb, TextEngine};

fn deriver() -> Box<LayoutDeriver> {
    Box::new(LayoutDeriver::new(TextEngine::new(FontDb::bundled_only())))
}

fn rect(w: f64, h: f64) -> Value {
    json!({"type": "rectangle", "width": w, "height": h})
}

fn start() -> (Store, NodeId) {
    let mut s = Store::default().with_ids(IdGen::seeded(1));
    s.set_deriver(deriver());
    let page = s.transact(TxMeta::user("page"), |tx| tx.insert_json(None, None, json!({"type": "page"}))).unwrap().0;
    let tree = json!({
        "type": "frame", "sizingH": "hug", "sizingV": "hug", "autoLayout": {"direction": "vertical", "gap": 8, "padding": [10, 10, 10, 10]},
        "children": [
            {"type": "frame", "width": 300, "sizingV": "hug", "autoLayout": {"gap": 4, "wrap": true}, "children": [rect(40.0, 20.0), rect(80.0, 30.0), {"type": "rectangle", "height": 10, "sizingH": "fill"}]},
            {"type": "text", "width": 120, "spans": [{"text": "Some wrapping body text here"}]},
            {"type": "frame", "width": 100, "height": 60, "children": [{"type": "rectangle", "x": 70, "y": 5, "width": 20, "height": 20, "constraints": {"h": "right", "v": "topBottom"}}]},
            {"type": "frame", "width": 200, "sizingV": "hug", "autoLayout": {"direction": "grid", "gap": 4, "columns": [{"fixed": 40}, {"fr": 1}]}, "children": [rect(10.0, 10.0), rect(10.0, 12.0), rect(10.0, 14.0)]}
        ]
    });
    s.transact(TxMeta::user("tree"), |tx| {
        tx.insert_json(Some(page), None, tree)?;
        tx.insert_json(Some(page), None, json!({"type": "group", "x": 500, "children": [rect(10.0, 10.0), {"type": "text", "x": 20, "spans": [{"text": "label"}], "autoResize": "widthAndHeight"}]}))
    })
    .unwrap();
    (s, page)
}

/// One random edit: `(what, a, b, v)`, with `a`/`b` picking nodes and `v` a value.
type Edit = (u8, u8, u8, u16);

fn apply(tx: &mut Tx<'_>, page: NodeId, (what, a, b, v): Edit) -> Result<(), Error> {
    let d = tx.doc();
    let pk = d.key(page).unwrap();
    let nodes: Vec<NodeId> = d.descendants(pk).into_iter().map(|k| d.node(k).unwrap().id).filter(|id| *id != page).collect();
    let containers: Vec<NodeId> = nodes.iter().copied().filter(|id| d.get(*id).unwrap().kind.is_container()).collect();
    let pick = |list: &[NodeId], i: u8| list[i as usize % list.len()];
    let n = pick(&nodes, a);
    let c = pick(&containers, b);
    let fv = (v % 200) as f64 + 1.0;
    let sizing = |i: u16| ["fixed", "hug", "fill"][i as usize % 3];
    let props = match what % 12 {
        0 => json!({"width": fv}),
        1 => json!({"height": fv}),
        2 => json!({"sizingH": sizing(v)}),
        3 => json!({"sizingV": sizing(v / 3)}),
        4 => json!({"visible": v % 4 != 0}),
        5 => json!({"layoutChild": {"absolute": v % 5 == 0, "grow": (v % 3) as f64}}),
        6 => {
            let c = tx.doc().get(c).unwrap();
            if !matches!(c.kind, NodeKind::Frame { .. }) {
                return Ok(());
            }
            let dir = ["horizontal", "vertical", "grid"][v as usize % 3];
            let main = ["start", "center", "end", "spaceBetween"][v as usize % 4];
            let cross = ["start", "center", "end", "stretch", "baseline"][v as usize % 5];
            let al = if v % 7 == 0 {
                Value::Null
            } else {
                json!({"direction": dir, "gap": (v % 13) as f64, "crossGap": (v % 5) as f64, "padding": [(v % 4) as f64, 2, 3, 1], "wrap": v % 2 == 0,
                       "mainAlign": main, "crossAlign": cross,
                       "columns": if v % 3 == 0 { json!([{"fixed": 30}, {"fr": 1}]) } else { json!([]) }})
            };
            return tx.update_json(c.id, json!({"autoLayout": al}).as_object().unwrap());
        }
        7 => {
            let resize = ["none", "height", "widthAndHeight"][v as usize % 3];
            let node = match v % 4 {
                0 => rect(fv, (v % 50) as f64 + 1.0),
                1 => json!({"type": "text", "width": fv, "spans": [{"text": "word ".repeat(v as usize % 9 + 1)}], "autoResize": resize}),
                2 => json!({"type": "frame", "sizingH": "hug", "sizingV": "hug", "autoLayout": {"gap": 3}, "children": [rect(10.0, 10.0), rect(fv, 5.0)]}),
                _ => json!({"type": "group", "children": [rect(10.0, 10.0), {"type": "rectangle", "x": fv, "y": 3, "width": 5, "height": 5}]}),
            };
            let len = tx.doc().get(c).unwrap().children.len();
            return tx.insert_json(Some(c), Some(v as usize % (len + 1)), node).map(|_| ());
        }
        8 => {
            if tx.doc().get(n).unwrap().parent.and_then(|p| tx.doc().node(p)).is_some_and(|p| p.kind.is_page()) {
                return Ok(());
            }
            return tx.delete(n);
        }
        9 => return tx.move_node(n, Some(c), None),
        10 => {
            if !matches!(tx.doc().get(n).unwrap().kind, NodeKind::Text { .. }) {
                return Ok(());
            }
            json!({"spans": [{"text": "abc ".repeat(v as usize % 12 + 1)}, {"text": "bold", "fontWeight": 700}]})
        }
        _ => json!({"x": fv, "y": (v % 37) as f64}),
    };
    tx.update_json(n, props.as_object().unwrap())
}

/// A full layout from scratch with a fresh deriver; returns what it would change.
fn full_layout_changes(doc: &Document) -> Vec<(NodeId, PropKey, f64, f64)> {
    let mut s = Store::new(Document::from_data(doc.to_data()).unwrap());
    s.set_deriver(deriver());
    let (_, ch) = s.transact_with_deriver(TxMeta::user("full"), |tx, d| d.unwrap().derive_all(tx)).unwrap();
    ch.updated
        .iter()
        .map(|(id, k)| {
            let old = doc.get(*id).unwrap().get(*k);
            let new = s.doc().get(*id).unwrap().get(*k);
            let num = |p: PropValue| match p {
                PropValue::X(v) | PropValue::Y(v) | PropValue::Width(v) | PropValue::Height(v) => v,
                _ => f64::NAN,
            };
            (*id, *k, num(old), num(new))
        })
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig { failure_persistence: None, ..ProptestConfig::default() })]

    #[test]
    fn incremental_layout_equals_full_layout(edits in prop::collection::vec(any::<Edit>(), 1..25)) {
        let (mut s, page) = start();
        prop_assert!(full_layout_changes(s.doc()).is_empty(), "initial: {:?}", full_layout_changes(s.doc()));
        for e in edits {
            let before = s.doc().to_data();
            let Ok((_, ch)) = s.transact(TxMeta::user("edit"), |tx| apply(tx, page, e)) else {
                prop_assert_eq!(s.doc().to_data(), before);
                continue;
            };
            let diff = full_layout_changes(s.doc());
            prop_assert!(diff.is_empty(), "after {:?}: full layout differs: {:?}", e, diff);
            if !ch.is_empty() {
                let after = s.doc().to_data();
                s.undo().unwrap();
                prop_assert_eq!(&s.doc().to_data(), &before, "undo of {:?}", e);
                s.redo().unwrap();
                prop_assert_eq!(&s.doc().to_data(), &after, "redo of {:?}", e);
            }
        }
        // Sizing values outside the enum never appear.
        prop_assert!(s.doc().iter().all(|(_, n)| matches!(n.props.sizing_h, Sizing::Fixed | Sizing::Hug | Sizing::Fill)));
    }
}

/// Runs `edits` and returns the full-layout diff after the last one.
fn replay(edits: &[Edit]) -> (Store, Vec<(NodeId, PropKey, f64, f64)>) {
    let (mut s, page) = start();
    for e in edits {
        let _ = s.transact(TxMeta::user("edit"), |tx| apply(tx, page, *e));
    }
    let diff = full_layout_changes(s.doc());
    (s, diff)
}

#[test]
fn regression_hidden_node_then_auto_layout_removed() {
    let (s, diff) = replay(&[(184, 255, 0, 4660), (54, 0, 1, 8), (66, 0, 55, 1302)]);
    if !diff.is_empty() {
        let page = s.doc().pages()[0];
        let tree = s.doc().to_tree(page).unwrap();
        println!("{}", serde_json::to_string_pretty(&tree).unwrap());
    }
    assert!(diff.is_empty(), "{diff:?}");
}

#[test]
fn regression_edited_while_hidden_then_shown() {
    let (_, diff) = replay(&[(92, 2, 0, 0), (7, 0, 0, 30708), (244, 91, 0, 56288), (35, 76, 0, 0), (220, 76, 0, 6793)]);
    assert!(diff.is_empty(), "{diff:?}");
}
