//! The layers model stays equal to a fresh rebuild through random edits, undo and expansion.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use doc::{Document, NodeId, Store, TxMeta};
use editor::layers::Layers;
use editor::{Session, commands};
use proptest::prelude::*;
use serde_json::json;
use text::FontDb;

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(48)
}

fn fresh(s: &Session, model: &Layers) -> Vec<editor::Row> {
    let mut l = Layers::new(s.page());
    for (_, n) in s.doc().iter() {
        if model.is_expanded(n.id) {
            l.set_expanded(n.id, true);
        }
    }
    l.rows(s.doc()).to_vec()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    #[test]
    fn layers_match_a_fresh_rebuild(steps in proptest::collection::vec((0u8..8, 0u16..1000, 0u16..1000), 1..40)) {
        let mut store = Store::new(Document::new());
        store.transact(TxMeta::user("setup"), |tx| doc::testing::render_test_page(tx).map(|_| ())).unwrap();
        let mut s = Session::new(store.into_doc(), None, FontDb::bundled_only()).unwrap();
        for (kind, a, b) in steps {
            let ids: Vec<NodeId> = s.doc().key(s.page()).map(|pk| s.doc().descendants(pk).into_iter().filter_map(|k| s.doc().node(k)).map(|n| n.id).collect()).unwrap_or_default();
            if ids.is_empty() {
                break;
            }
            let x = ids[a as usize % ids.len()];
            let y = ids[b as usize % ids.len()];
            match kind {
                0 => s.layers.set_expanded(x, true),
                1 => s.layers.set_expanded(x, false),
                2 => s.reveal_layer(x),
                3 => { let _ = s.transact(TxMeta::user("rename"), |tx| tx.update_json(x, json!({"name": format!("n{b}")}).as_object().unwrap())); }
                4 => { s.select(vec![x]); let _ = commands::execute(&mut s, commands::Command::Delete); }
                5 => { s.select(vec![x]); let _ = commands::execute(&mut s, commands::Command::Duplicate); }
                // Moves that would put a node inside itself are refused by the document.
                6 => { let _ = commands::move_layers(&mut s, &[x], y, 0); }
                _ => { let _ = s.undo(); }
            }
            // Read the model after every step (as the panel does each frame).
            let model = s.layer_rows().1.to_vec();
            prop_assert_eq!(&model, &fresh(&s, &s.layers));
        }
    }
}
