//! Instance content is derived state, so it must not depend on history: after random edits to
//! components, overrides, sizes, variables and modes, every expansion equals what a fresh load
//! of the document builds, and a full pass from scratch (layout and bound values) writes nothing.
//! Undoing every edit restores the original expansions; redoing restores the final ones.
//! Case count: `PROPTEST_CASES` (default 256).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use doc::*;
use layout::LayoutDeriver;
use proptest::prelude::*;
use serde_json::json;
use text::{FontDb, TextEngine};

const COLL: &str = "0Co00000000000000000V1";
const LIGHT: &str = "0Mo00000000000000000ML";
const DARK: &str = "0Mo00000000000000000MD";
const ACCENT: &str = "0Va00000000000000000VC";
const GAP: &str = "0Va00000000000000000VG";
const CARD: &str = "0Cm00000000000000000C1";
const INNER: &str = "0Cm00000000000000000C2";
const DARK_FRAME: &str = "0Fr00000000000000000FD";

fn deriver() -> Box<LayoutDeriver> {
    Box::new(LayoutDeriver::new(TextEngine::new(FontDb::bundled_only())))
}

fn id(s: &str) -> NodeId {
    s.parse().unwrap()
}

fn variables(accent: &str, gap: f64) -> Variables {
    serde_json::from_value(json!([{"id": COLL, "name": "Theme", "defaultMode": LIGHT,
        "modes": [{"id": LIGHT, "name": "Light"}, {"id": DARK, "name": "Dark"}],
        "variables": [
            {"id": ACCENT, "name": "accent", "type": "color", "values": {LIGHT: accent, DARK: "#93c5fd"}},
            {"id": GAP, "name": "gap", "type": "number", "values": {LIGHT: gap, DARK: 2.0}}]}]))
    .unwrap()
}

fn start() -> (Store, NodeId) {
    let mut s = Store::default().with_ids(IdGen::seeded(3));
    s.set_deriver(deriver());
    let page = s.transact(TxMeta::user("page"), |tx| tx.insert_json(None, None, json!({"type": "page"}))).unwrap().0;
    s.transact(TxMeta::user("setup"), |tx| {
        tx.set_variables(variables("#2563eb", 8.0))?;
        tx.insert_json(Some(page), None, json!({"id": INNER, "type": "component", "name": "Inner", "y": 600, "width": 40, "height": 20,
            "children": [{"type": "rectangle", "name": "Dot", "x": 4, "y": 4, "width": 12, "height": 12, "constraints": {"h": "right", "v": "scale"},
                "fills": [{"type": "solid", "color": "#000000"}], "bindings": {"fills/0/color": ACCENT}}]}))?;
        tx.insert_json(Some(page), None, json!({"id": CARD, "type": "component", "name": "Card", "x": 100, "y": 600, "width": 160, "height": 100,
            "fills": [{"type": "solid", "color": "#ffffff"}], "children": [
                {"type": "rectangle", "name": "Bar", "width": 160, "height": 6, "constraints": {"h": "leftRight"}, "fills": [{"type": "solid", "color": "#000000"}], "bindings": {"fills/0/color": ACCENT}},
                {"type": "text", "name": "Title", "x": 8, "y": 12, "width": 120, "spans": [{"text": "Title words"}], "autoResize": "height"},
                {"type": "frame", "name": "Row", "x": 8, "y": 50, "sizingH": "hug", "sizingV": "hug", "autoLayout": {"gap": 4}, "bindings": {"autoLayout/gap": GAP},
                    "constraints": {"v": "bottom"}, "children": [
                    {"type": "rectangle", "name": "A", "width": 10, "height": 10},
                    {"type": "instance", "name": "Nested", "component": INNER, "width": 40, "height": 20}]}]}))?;
        tx.insert_json(Some(page), None, json!({"type": "instance", "name": "I1", "component": CARD, "width": 200, "height": 120}))?;
        tx.insert_json(Some(page), None, json!({"id": DARK_FRAME, "type": "frame", "name": "Dark", "x": 300, "width": 300, "height": 200, "modes": {COLL: DARK},
            "children": [{"type": "instance", "name": "I2", "component": CARD, "width": 160, "height": 100}]}))?;
        Ok(())
    })
    .unwrap();
    (s, page)
}

/// Every virtual node as text, by id: the whole derived content of every instance.
fn expansions(doc: &Document) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = doc
        .iter()
        .filter(|(_, n)| n.virt.is_some())
        .map(|(_, n)| {
            let parent = n.parent.and_then(|p| doc.node(p)).map(|p| p.id.to_string()).unwrap_or_default();
            let children: Vec<String> = n.children.iter().filter_map(|c| doc.node(*c)).map(|c| c.id.to_string()).collect();
            (n.id.to_string(), format!("{parent} {:?} {} {:?} {:?} {:?}", n.virt, n.name, n.kind, n.props, children))
        })
        .collect();
    v.sort();
    v
}

/// The same document loaded fresh: no history, every instance expanded from scratch.
fn fresh(doc: &Document) -> Store {
    let mut s = Store::new(Document::from_data(doc.to_data()).unwrap());
    s.set_deriver(deriver());
    s
}

type Edit = (u8, u8, u16);

fn apply(tx: &mut Tx<'_>, (what, a, v): Edit) -> Result<(), Error> {
    let d = tx.doc();
    let under = |c: &str| -> Vec<NodeId> { d.descendants(d.key(id(c)).unwrap()).into_iter().skip(1).map(|k| d.node(k).unwrap().id).collect() };
    let in_card = under(CARD);
    let virtuals: Vec<NodeId> = d.iter().filter(|(_, n)| n.virt.is_some()).map(|(_, n)| n.id).collect();
    let instances: Vec<NodeId> = d.iter().filter(|(_, n)| n.virt.is_none() && matches!(n.kind, NodeKind::Instance { .. }) && n.name.starts_with('I')).map(|(_, n)| n.id).collect();
    // Everything in the card may have been deleted: then the edit does nothing.
    let pick = |l: &[NodeId]| l.get(a as usize % l.len().max(1)).copied().unwrap_or(id(CARD));
    let fv = f64::from(v % 150) + 2.0;
    let colour = ["#ef4444", "#22c55e", "#f59e0b"][v as usize % 3];
    let props = match what % 11 {
        0 => (pick(&in_card), json!({"x": fv / 2.0})),
        1 => (pick(&in_card), json!({"width": fv})),
        2 => (pick(&in_card), json!({"fills": [{"type": "solid", "color": colour}]})),
        3 => (pick(&in_card), json!({"visible": v % 3 != 0})),
        4 => {
            let containers: Vec<NodeId> = in_card.iter().copied().chain([id(CARD)]).filter(|c| matches!(d.get(*c).unwrap().kind, NodeKind::Frame { .. } | NodeKind::Component { .. })).collect();
            let c = pick(&containers);
            return tx.insert_json(Some(c), None, json!({"type": "rectangle", "name": format!("New{v}"), "x": fv / 3.0, "width": 8, "height": fv / 4.0, "constraints": {"h": "scale"}})).map(|_| ());
        }
        5 if !in_card.is_empty() => return tx.delete(pick(&in_card)),
        6 => (pick(&instances), json!({"width": fv + 20.0, "height": fv / 2.0 + 20.0})),
        // An edit inside an instance: an override.
        7 if !virtuals.is_empty() => (pick(&virtuals), json!({"opacity": f64::from(v % 4) / 4.0, "fills": [{"type": "solid", "color": colour}]})),
        8 => return tx.set_variables(variables(colour, f64::from(v % 20))),
        9 => (id(DARK_FRAME), json!({"modes": if v % 2 == 0 { json!({COLL: DARK}) } else { json!({COLL: LIGHT}) }})),
        _ => (pick(&under(INNER)), json!({"height": fv / 10.0 + 1.0})),
    };
    tx.update_json(props.0, props.1.as_object().unwrap())
}

fn config() -> ProptestConfig {
    let cases = std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(256);
    ProptestConfig { cases, failure_persistence: None, ..ProptestConfig::default() }
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn expansions_follow_edits_and_undo(edits in prop::collection::vec(any::<Edit>(), 1..16)) {
        let (mut s, _) = start();
        let initial = expansions(s.doc());
        prop_assert!(!initial.is_empty());
        let mut done = 0;
        for e in edits {
            if s.transact(TxMeta::user("edit"), |tx| apply(tx, e)).is_ok() {
                done += 1;
            }
            s.doc().check_invariants().unwrap();
            let now = expansions(s.doc());
            let mut f = fresh(s.doc());
            prop_assert_eq!(&now, &expansions(f.doc()), "after {:?}: incremental vs fresh", e);
            // A full pass from scratch writes nothing: layout and bound values are settled.
            // (Re-expansion may record a write that layout then puts back; compare values.)
            let before = f.doc().clone();
            let (_, ch) = f.transact_with_deriver(TxMeta::user("full"), |tx, d| d.unwrap().derive_all(tx)).unwrap();
            let changed: Vec<_> = ch.updated.iter().filter(|(i, k)| before.get(*i).map(|n| n.get(*k)) != f.doc().get(*i).map(|n| n.get(*k))).collect();
            prop_assert!(changed.is_empty(), "after {:?}: a full pass changed {:?}", e, changed);
        }
        let last = expansions(s.doc());
        for _ in 0..done {
            s.undo().unwrap();
        }
        prop_assert_eq!(expansions(s.doc()), initial);
        for _ in 0..done {
            s.redo().unwrap();
        }
        prop_assert_eq!(expansions(s.doc()), last);
    }
}

