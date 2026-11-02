//! Property tests: random transactions of random ops (valid and invalid) on a small document.
//! After every transaction the document invariants hold, failed transactions change nothing,
//! undoing everything restores the start state step by step, and redoing restores the end.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use doc::model::*;
use doc::*;
use proptest::prelude::*;
use serde_json::json;

#[derive(Debug, Clone)]
enum Action {
    Insert { parent: u16, index: u16, kind: u8 },
    Delete { pick: u16 },
    Update { pick: u16, prop: u8, v: i16 },
    Move { pick: u16, parent: u16, index: u16 },
    Copy { pick: u16, parent: u16 },
}

fn action() -> impl Strategy<Value = Action> {
    prop_oneof![
        4 => (any::<u16>(), any::<u16>(), 0u8..7).prop_map(|(parent, index, kind)| Action::Insert { parent, index, kind }),
        1 => any::<u16>().prop_map(|pick| Action::Delete { pick }),
        3 => (any::<u16>(), 0u8..6, any::<i16>()).prop_map(|(pick, prop, v)| Action::Update { pick, prop, v }),
        2 => (any::<u16>(), any::<u16>(), any::<u16>()).prop_map(|(pick, parent, index)| Action::Move { pick, parent, index }),
        1 => (any::<u16>(), any::<u16>()).prop_map(|(pick, parent)| Action::Copy { pick, parent }),
    ]
}

/// All node ids in document order (pages first).
fn all_ids(doc: &Document) -> Vec<NodeId> {
    doc.pages().iter().flat_map(|p| doc.descendants(*p)).filter_map(|k| doc.node(k).map(|n| n.id)).collect()
}

fn pick(ids: &[NodeId], i: u16) -> Option<NodeId> {
    (!ids.is_empty()).then(|| ids[i as usize % ids.len()])
}

fn run(tx: &mut Tx<'_>, a: &Action) -> Result<(), Error> {
    let ids = all_ids(tx.doc());
    let comps: Vec<NodeId> = ids.iter().copied().filter(|id| matches!(tx.doc().get(*id).map(|n| &n.kind), Some(NodeKind::Component { .. }))).collect();
    match *a {
        Action::Insert { parent, index, kind } => {
            let parent = if kind == 6 { None } else { pick(&ids, parent) };
            let node = match kind {
                0 => json!({"type": "rectangle", "width": 10, "fills": [{"type": "solid", "color": "#336699"}]}),
                1 => json!({"type": "frame", "children": [{"type": "ellipse"}, {"type": "text", "spans": [{"text": "hi"}]}]}),
                2 => json!({"type": "group"}),
                3 => json!({"type": "component", "children": [{"type": "rectangle"}]}),
                4 => match pick(&comps, index) {
                    Some(c) => json!({"type": "instance", "component": c.to_string()}),
                    None => json!({"type": "line"}),
                },
                5 => json!({"type": "path", "paths": [{"d": "M0 0 L10 10 Z"}]}),
                _ => json!({"type": "page"}),
            };
            let n = parent.and_then(|p| tx.doc().get(p)).map_or(tx.doc().pages().len(), |n| n.children.len());
            // Sometimes out of range on purpose.
            let index = index as usize % (n + 2);
            tx.insert_json(parent, Some(index), node).map(|_| ())
        }
        Action::Delete { pick: p } => match pick(&ids, p) {
            Some(id) => tx.delete(id),
            None => Ok(()),
        },
        Action::Update { pick: p, prop, v } => {
            let Some(id) = pick(&ids, p) else { return Ok(()) };
            let v = v as f64;
            let value = match prop {
                0 => PropValue::X(v),
                1 => PropValue::Width(v.abs()),
                2 => PropValue::Opacity((v.abs() % 100.0) / 100.0),
                3 => PropValue::Name(format!("n{v}")),
                4 => PropValue::Fills(vec![Paint::solid(Color::rgb(v as u8, 0, 0))]),
                // Invalid on purpose half the time (NaN).
                _ => PropValue::Rotation(if v < 0.0 { f64::NAN } else { v }),
            };
            tx.update(id, value)
        }
        Action::Move { pick: p, parent, index } => {
            let Some(id) = pick(&ids, p) else { return Ok(()) };
            let parent = pick(&ids, parent);
            let n = parent.and_then(|p| tx.doc().get(p)).map_or(tx.doc().pages().len(), |n| n.children.len());
            tx.move_node(id, parent, Some(index as usize % (n + 1)))
        }
        Action::Copy { pick: p, parent } => {
            let Some(id) = pick(&ids, p) else { return Ok(()) };
            tx.copy(id, pick(&ids, parent), None, None).map(|_| ())
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(256), ..ProptestConfig::default() })]

    #[test]
    fn transactions_keep_invariants_and_undo_round_trips(txs in prop::collection::vec(prop::collection::vec(action(), 1..5), 1..25), seed in any::<u64>()) {
        let mut s = Store::default().with_ids(IdGen::seeded(seed));
        s.transact(TxMeta::user("page"), |tx| tx.insert_json(None, None, json!({"type": "page"}))).unwrap();
        let mut states = vec![s.doc().to_data()];
        for actions in &txs {
            let before = s.doc().to_data();
            let r = s.transact(TxMeta::user("t"), |tx| {
                for a in actions {
                    run(tx, a)?;
                }
                Ok(())
            });
            s.doc().check_invariants().map_err(TestCaseError::fail)?;
            let after = s.doc().to_data();
            match r {
                Err(_) => prop_assert_eq!(&after, &before, "failed transaction changed the document"),
                // Any applied op makes an undo step, even if the net effect is nil.
                Ok(((), changes)) if !changes.is_empty() => states.push(after),
                Ok(_) => prop_assert_eq!(&after, &before),
            }
        }
        // Undo back to the start, checking each intermediate state.
        for expected in states.iter().rev().skip(1) {
            prop_assert!(s.undo().unwrap().is_some());
            s.doc().check_invariants().map_err(TestCaseError::fail)?;
            prop_assert_eq!(&s.doc().to_data(), expected);
        }
        // Then redo to the end.
        for expected in states.iter().skip(1) {
            prop_assert!(s.redo().unwrap().is_some());
            prop_assert_eq!(&s.doc().to_data(), expected);
        }
        prop_assert!(!s.can_redo());
    }

    #[test]
    fn json_round_trip_of_random_documents(txs in prop::collection::vec(action(), 1..40), seed in any::<u64>()) {
        let mut s = Store::default().with_ids(IdGen::seeded(seed));
        s.transact(TxMeta::user("page"), |tx| tx.insert_json(None, None, json!({"type": "page"}))).unwrap();
        for a in &txs {
            let _ = s.transact(TxMeta::user("t"), |tx| run(tx, a));
        }
        let data = s.doc().to_data();
        let text = serde_json::to_string_pretty(&data).unwrap();
        let back: DocumentData = serde_json::from_str(&text).unwrap();
        prop_assert_eq!(&back, &data);
        let rebuilt = Document::from_data(back).unwrap();
        rebuilt.check_invariants().map_err(TestCaseError::fail)?;
        prop_assert_eq!(serde_json::to_string_pretty(&rebuilt.to_data()).unwrap(), text);
    }
}
