//! A laid-out document survives save and open: re-opening and relaying out everything writes
//! nothing, and re-saving is byte-identical.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use doc::testing::{GenConfig, STRESS_ROOT, generate};
use doc::*;
use format::Bundle;
use layout::LayoutDeriver;
use text::{FontDb, TextEngine};

fn store(doc: Document) -> Store {
    let mut s = Store::new(doc);
    s.set_deriver(Box::new(LayoutDeriver::new(TextEngine::new(FontDb::bundled_only()))));
    s
}

fn relayout(s: &mut Store) -> ChangeSet {
    s.transact_with_deriver(TxMeta::user("relayout"), |tx, d| d.unwrap().derive_all(tx)).unwrap().1
}

fn check(name: &str, doc: Document) {
    let mut s = store(doc);
    relayout(&mut s);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(format!("{name}.canvas"));
    let bundle = Bundle::create(&path).unwrap();
    bundle.save(s.doc()).unwrap();
    let first = std::fs::read(bundle.document_path()).unwrap();

    let opened = Bundle::open(&path).unwrap();
    let mut again = store(opened.doc);
    let ch = relayout(&mut again);
    assert!(ch.is_empty(), "{name}: relayout after open changed {} props", ch.updated.len());
    opened.bundle.save(again.doc()).unwrap();
    assert_eq!(std::fs::read(opened.bundle.document_path()).unwrap(), first, "{name}: re-save differs");
}

#[test]
fn all_kinds_fixture_round_trips_after_layout() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/all-kinds.canvas");
    check("all-kinds", Bundle::open(&dir).unwrap().doc);
}

#[test]
fn generated_auto_layout_document_round_trips_after_layout() {
    let doc = Document::from_data(generate(&GenConfig { target_nodes: 3_000, auto_layout_heavy: true, ..Default::default() })).unwrap();
    let mut s = store(doc);
    relayout(&mut s);
    // Computed positions are in the file: the second section sits below the first.
    let d = s.doc();
    let root = d.iter().find(|(_, n)| n.name == STRESS_ROOT).unwrap().1;
    let [a, b] = [0, 1].map(|i| &d.node(root.children[i]).unwrap().props);
    assert!(a.height > 0.0 && b.y >= a.y + a.height, "sections not stacked: {} {} {}", a.y, a.height, b.y);
    check("generated", s.into_doc());
}
