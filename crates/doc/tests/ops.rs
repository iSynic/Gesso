//! Behaviour of the document, ops, transactions, undo/redo and JSON commands.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use doc::model::*;
use doc::testing::{GenConfig, generate};
use doc::variables::*;
use doc::*;
use serde_json::json;

fn store() -> (Store, NodeId) {
    let mut s = Store::default().with_ids(IdGen::seeded(42));
    let page = s
        .transact(TxMeta::user("page"), |tx| {
            let id = tx.new_id();
            tx.insert(None, None, NodeTree::new(id, NodeKind::Page { background: None }))
        })
        .unwrap()
        .0;
    (s, page)
}

#[test]
fn generated_document_round_trips_through_arena_and_json() {
    let data = generate(&GenConfig { target_nodes: 2_000, ..Default::default() });
    let doc = Document::from_data(data.clone()).unwrap();
    doc.check_invariants().unwrap();
    assert!(doc.len() >= 2_000);
    assert_eq!(doc.to_data(), data);
    let text = serde_json::to_string_pretty(&data).unwrap();
    let back: DocumentData = serde_json::from_str(&text).unwrap();
    assert_eq!(back, data);
}

#[test]
fn defaults_are_omitted_in_json() {
    let t = NodeTree::new("0000000000000000000001".parse().unwrap(), NodeKind::Rectangle {});
    let v = serde_json::to_value(&t).unwrap();
    assert_eq!(v, json!({"id": "0000000000000000000001", "type": "rectangle", "width": 0.0, "height": 0.0}));
}

#[test]
fn insert_update_move_delete_and_undo() {
    let (mut s, page) = store();
    let before = s.doc().to_data();
    let (ids, changes) = s
        .transact(TxMeta::user("build"), |tx| {
            let a = tx.insert_json(Some(page), None, json!({"type": "frame", "name": "A", "width": 100, "height": 100}))?;
            let b = tx.insert_json(Some(a), None, json!({"type": "rectangle", "fills": [{"type": "solid", "color": "#ff0000"}]}))?;
            let c = tx.insert_json(Some(page), None, json!({"type": "frame", "name": "C"}))?;
            tx.update_json(b, json!({"opacity": 0.5, "name": "Red"}).as_object().unwrap())?;
            tx.move_node(b, Some(c), None)?;
            Ok((a, b, c))
        })
        .unwrap();
    let (a, b, c) = ids;
    assert_eq!(changes.inserted.len(), 3);
    assert_eq!(changes.moved, vec![b]);
    let doc = s.doc();
    doc.check_invariants().unwrap();
    assert!(doc.node(doc.key(a).unwrap()).unwrap().children.is_empty());
    let bn = doc.get(b).unwrap();
    assert_eq!(bn.props.opacity, 0.5);
    assert_eq!(bn.name, "Red");
    assert_eq!(doc.get(bn.parent.and_then(|p| doc.node(p)).unwrap().id).unwrap().id, c);

    let after = s.doc().to_data();
    assert!(s.undo().unwrap().is_some());
    assert_eq!(s.doc().to_data(), before);
    s.doc().check_invariants().unwrap();
    assert!(s.redo().unwrap().is_some());
    assert_eq!(s.doc().to_data(), after);

    s.transact(TxMeta::user("delete"), |tx| tx.delete(c)).unwrap();
    assert!(s.doc().get(b).is_none());
    s.undo().unwrap();
    assert_eq!(s.doc().to_data(), after);
}

#[test]
fn failed_transaction_rolls_back_everything() {
    let (mut s, page) = store();
    let before = s.doc().to_data();
    let err = s
        .transact(TxMeta::user("bad"), |tx| {
            let a = tx.insert_json(Some(page), None, json!({"type": "frame"}))?;
            tx.update_json(a, json!({"opacity": 0.2}).as_object().unwrap())?;
            tx.insert_json(Some(page), Some(99), json!({"type": "rectangle"}))?;
            Ok(())
        })
        .unwrap_err();
    assert!(matches!(err, Error::Doc(DocError::IndexOutOfRange { index: 99, .. })), "{err}");
    assert_eq!(s.doc().to_data(), before);
    assert_eq!(s.undo_label(), Some("page"));
}

#[test]
fn structural_rules_are_enforced() {
    let (mut s, page) = store();
    let (a, r) = s
        .transact(TxMeta::user("x"), |tx| {
            let a = tx.insert_json(Some(page), None, json!({"type": "frame"}))?;
            let r = tx.insert_json(Some(a), None, json!({"type": "rectangle"}))?;
            Ok((a, r))
        })
        .unwrap()
        .0;
    let mut try_tx = |f: &dyn Fn(&mut Tx) -> Result<(), Error>| s.transact(TxMeta::user("t"), |tx| f(tx)).unwrap_err().to_string();
    assert!(try_tx(&|tx| tx.insert_json(Some(r), None, json!({"type": "rectangle"})).map(|_| ())).contains("cannot have children"));
    assert!(try_tx(&|tx| tx.move_node(a, Some(r), None)).contains("cannot have children"));
    let mut try_tx = |f: &dyn Fn(&mut Tx) -> Result<(), Error>| s.transact(TxMeta::user("t"), |tx| f(tx)).unwrap_err().to_string();
    let inner = "0000000000000000000009".parse::<NodeId>().unwrap();
    assert!(try_tx(&|tx| {
        tx.insert(Some(a), None, NodeTree::new(inner, NodeKind::Group {}))?;
        tx.move_node(a, Some(inner), None)
    })
    .contains("own descendant"));
    assert!(try_tx(&|tx| tx.insert_json(None, None, json!({"type": "frame"})).map(|_| ())).contains("top level"));
    assert!(try_tx(&|tx| tx.insert_json(Some(a), None, json!({"type": "page"})).map(|_| ())).contains("top level"));
    assert!(try_tx(&|tx| tx.insert_json(Some(a), None, json!({"type": "rectangle", "colour": "#fff"})).map(|_| ())).contains("unknown property \"colour\""));
    assert!(try_tx(&|tx| tx.update_json(r, json!({"spans": []}).as_object().unwrap())).contains("unknown property \"spans\""));
    assert!(try_tx(&|tx| tx.update_json(r, json!({"type": "ellipse"}).as_object().unwrap())).contains("cannot be updated"));
    assert!(try_tx(&|tx| tx.update(r, PropValue::Kind(NodeKind::Line {}))).contains("cannot change"));
    assert!(try_tx(&|tx| tx.update(r, PropValue::Width(f64::NAN))).contains("finite"));
    assert!(try_tx(&|tx| tx.insert_json(Some(a), None, json!({"type": "path", "paths": [{"d": "M0 0 Q"}]})).map(|_| ())).contains("bad path data"));
    assert!(try_tx(&|tx| tx.insert_json(Some(a), None, json!({"type": "polygon", "sides": 2})).map(|_| ())).contains("3 sides"));
    assert!(try_tx(&|tx| tx.insert_json(Some(a), None, json!({"type": "image", "asset": "ab".repeat(32)})).map(|_| ())).contains("unknown asset"));
    let dup = r.to_string();
    assert!(try_tx(&|tx| tx.insert_json(Some(a), None, json!({"type": "rectangle", "id": dup})).map(|_| ())).contains("already exists"));
    s.doc().check_invariants().unwrap();
}

#[test]
fn reorder_within_parent_and_index_semantics() {
    let (mut s, page) = store();
    let ids = s
        .transact(TxMeta::user("x"), |tx| (0..4).map(|i| tx.insert_json(Some(page), None, json!({"type": "rectangle", "name": format!("r{i}")}))).collect::<Result<Vec<_>, _>>())
        .unwrap()
        .0;
    let names = |s: &Store| s.doc().node(s.doc().key(page).unwrap()).unwrap().children.iter().map(|k| s.doc().node(*k).unwrap().name.clone()).collect::<Vec<_>>();
    s.transact(TxMeta::user("front"), |tx| tx.move_node(ids[0], Some(page), None)).unwrap();
    assert_eq!(names(&s), ["r1", "r2", "r3", "r0"]);
    s.transact(TxMeta::user("back"), |tx| tx.move_node(ids[0], Some(page), Some(0))).unwrap();
    assert_eq!(names(&s), ["r0", "r1", "r2", "r3"]);
    s.transact(TxMeta::user("mid"), |tx| tx.move_node(ids[3], Some(page), Some(1))).unwrap();
    assert_eq!(names(&s), ["r0", "r3", "r1", "r2"]);
    s.undo().unwrap();
    assert_eq!(names(&s), ["r0", "r1", "r2", "r3"]);
}

#[test]
fn components_and_instances() {
    let (mut s, page) = store();
    let (comp, inst) = s
        .transact(TxMeta::user("x"), |tx| {
            let comp = tx.insert_json(Some(page), None, json!({"type": "component", "name": "Btn", "children": [{"type": "text", "spans": [{"text": "Hi"}]}]}))?;
            let inst = tx.insert_json(Some(page), None, json!({"type": "instance", "component": comp.to_string()}))?;
            Ok((comp, inst))
        })
        .unwrap()
        .0;
    assert_eq!(s.doc().instances_of(comp).count(), 1);
    // Deleting a component with instances elsewhere is refused.
    let e = s.transact(TxMeta::user("del"), |tx| tx.delete(comp)).unwrap_err();
    assert!(e.to_string().contains("used by 1 instance"), "{e}");
    // An instance inside its own component is a cycle.
    let e = s.transact(TxMeta::user("mv"), |tx| tx.move_node(inst, Some(comp), None)).unwrap_err();
    assert!(e.to_string().contains("instance of itself"), "{e}");
    // Two components using each other.
    let e = s
        .transact(TxMeta::user("cyc"), |tx| {
            let c2 = tx.insert_json(Some(page), None, json!({"type": "component"}))?;
            tx.insert_json(Some(c2), None, json!({"type": "instance", "component": comp.to_string()}))?;
            tx.insert_json(Some(comp), None, json!({"type": "instance", "component": c2.to_string()}))?;
            Ok(())
        })
        .unwrap_err();
    assert!(e.to_string().contains("instance of itself"), "{e}");
    // Unknown component.
    let e = s.transact(TxMeta::user("u"), |tx| tx.insert_json(Some(page), None, json!({"type": "instance", "component": "0000000000000000000077"}))).unwrap_err();
    assert!(e.to_string().contains("not a component"), "{e}");
    // Deleting instance then component is fine, and undo restores the index.
    s.transact(TxMeta::user("del2"), |tx| {
        tx.delete(inst)?;
        tx.delete(comp)
    })
    .unwrap();
    assert_eq!(s.doc().instances_of(comp).count(), 0);
    s.undo().unwrap();
    assert_eq!(s.doc().instances_of(comp).count(), 1);
    s.doc().check_invariants().unwrap();
}

#[test]
fn copy_and_replace() {
    let (mut s, page) = store();
    let a = s
        .transact(TxMeta::user("x"), |tx| tx.insert_json(Some(page), None, json!({"type": "frame", "name": "Card", "children": [{"type": "rectangle"}, {"type": "ellipse"}]})))
        .unwrap()
        .0;
    let b = s.transact(TxMeta::user("copy"), |tx| tx.copy(a, Some(page), None, json!({"x": 400, "name": "Card 2"}).as_object())).unwrap().0;
    let (an, bn) = (s.doc().get(a).unwrap(), s.doc().get(b).unwrap());
    assert_eq!(bn.props.x, 400.0);
    assert_eq!(bn.children.len(), 2);
    assert_ne!(an.children[0], bn.children[0]);
    let r = s.transact(TxMeta::user("replace"), |tx| tx.replace_json(a, json!({"type": "ellipse", "name": "E"}))).unwrap().0;
    assert!(s.doc().get(a).is_none());
    assert_eq!(s.doc().index_of(s.doc().key(r).unwrap()), Some(0));
    s.doc().check_invariants().unwrap();
}

#[test]
fn coalescing_merges_drag_steps() {
    let (mut s, page) = store();
    let r = s.transact(TxMeta::user("add"), |tx| tx.insert_json(Some(page), None, json!({"type": "rectangle"}))).unwrap().0;
    for i in 1..=60 {
        s.transact(TxMeta::user("Move").coalesce("drag-1"), |tx| tx.update(r, PropValue::X(i as f64))).unwrap();
    }
    assert_eq!(s.doc().get(r).unwrap().props.x, 60.0);
    s.undo().unwrap();
    assert_eq!(s.doc().get(r).unwrap().props.x, 0.0);
    assert_eq!(s.undo_label(), Some("add"));
    s.redo().unwrap();
    assert_eq!(s.doc().get(r).unwrap().props.x, 60.0);
}

#[test]
fn commit_observer_sees_every_change() {
    let (mut s, page) = store();
    let log = Rc::new(RefCell::new(Vec::new()));
    let l = log.clone();
    s.on_commit(move |c| l.borrow_mut().push((c.origin, c.label.to_string(), c.ops.len())));
    s.transact(TxMeta::new(Origin::Agent, "add two"), |tx| {
        tx.insert_json(Some(page), None, json!({"type": "rectangle"}))?;
        tx.insert_json(Some(page), None, json!({"type": "ellipse"}))
    })
    .unwrap();
    s.transact(TxMeta::user("noop"), |_| Ok(())).unwrap();
    s.undo().unwrap();
    assert_eq!(*log.borrow(), vec![(Origin::Agent, "add two".into(), 2), (Origin::Undo, "add two".into(), 2)]);
}

#[test]
fn ops_serialize_for_the_op_log() {
    let op = Op::Update { id: Id(5), value: PropValue::Opacity(0.5) };
    let v = serde_json::to_value(&op).unwrap();
    assert_eq!(v, json!({"op": "update", "id": "0000000000000000000005", "prop": "opacity", "value": 0.5}));
    assert_eq!(serde_json::from_value::<Op>(v).unwrap(), op);
}

#[test]
fn variables_resolve_with_modes_and_aliases() {
    let ids: Vec<Id> = (1..=6).map(Id).collect();
    let (coll, light, dark, bg, fg, alias) = (ids[0], ids[1], ids[2], ids[3], ids[4], ids[5]);
    let vars = Variables {
        collections: vec![Collection {
            id: coll,
            name: "Theme".into(),
            modes: vec![Mode { id: light, name: "Light".into() }, Mode { id: dark, name: "Dark".into() }],
            default_mode: light,
            variables: vec![
                Variable { id: bg, name: "bg".into(), ty: VariableType::Color, values: BTreeMap::from([(light, VariableValue::String("#ffffff".into())), (dark, VariableValue::String("#000000".into()))]) },
                Variable { id: fg, name: "fg".into(), ty: VariableType::Color, values: BTreeMap::from([(light, VariableValue::Alias { alias: bg })]) },
                Variable { id: alias, name: "n".into(), ty: VariableType::Number, values: BTreeMap::from([(light, VariableValue::Number(4.0))]) },
            ],
        }],
    };
    let (mut s, page) = store();
    s.transact(TxMeta::user("vars"), |tx| tx.set_variables(vars.clone())).unwrap();
    let v = s.doc().variables();
    assert_eq!(v.resolve(bg, &BTreeMap::from([(coll, dark)])), Ok(Resolved::Color(Color::BLACK)));
    // fg has no dark value: falls back to the default mode, then follows the alias in dark mode.
    assert_eq!(v.resolve(fg, &BTreeMap::from([(coll, dark)])), Ok(Resolved::Color(Color::BLACK)));
    assert_eq!(v.resolve(fg, &BTreeMap::new()), Ok(Resolved::Color(Color::WHITE)));

    // Bindings must point at existing variables, and variables in use cannot be removed.
    let r = s.transact(TxMeta::user("bind"), |tx| tx.insert_json(Some(page), None, json!({"type": "rectangle", "bindings": {"fills/0/color": bg.to_string()}}))).unwrap().0;
    assert!(s.transact(TxMeta::user("clear"), |tx| tx.set_variables(Variables::default())).is_err());
    assert!(s.doc().get(r).is_some());

    let mut cyclic = vars.clone();
    cyclic.collections[0].variables[0].values.insert(light, VariableValue::Alias { alias: fg });
    let e = s.transact(TxMeta::user("cyc"), |tx| tx.set_variables(cyclic)).unwrap_err();
    assert!(e.to_string().contains("cycle"), "{e}");
    let mut wrong = vars;
    wrong.collections[0].variables[2].values.insert(light, VariableValue::String("x".into()));
    assert!(s.transact(TxMeta::user("ty"), |tx| tx.set_variables(wrong)).is_err());
}

#[test]
fn assets_cannot_be_removed_while_used() {
    let (mut s, page) = store();
    let a = AssetId("ab".repeat(32));
    s.transact(TxMeta::user("asset"), |tx| {
        tx.put_asset(a.clone(), AssetMeta { mime: "image/png".into(), width: 4, height: 4 })?;
        tx.insert_json(Some(page), None, json!({"type": "rectangle", "fills": [{"type": "image", "asset": a.0}]}))
    })
    .unwrap();
    let e = s.transact(TxMeta::user("rm"), |tx| tx.remove_asset(a.clone())).unwrap_err();
    assert!(e.to_string().contains("still used"), "{e}");
}

#[test]
fn seeded_store_gives_reproducible_ids() {
    let run = || {
        let (mut s, page) = store();
        s.transact(TxMeta::user("x"), |tx| tx.insert_json(Some(page), None, json!({"type": "frame", "children": [{"type": "rectangle"}]}))).unwrap();
        serde_json::to_string(&s.doc().to_data()).unwrap()
    };
    assert_eq!(run(), run());
}

/// Test deriver: keeps every rectangle's height at half its width; fails on a node named "boom".
struct HalfHeight {
    observed: Rc<RefCell<usize>>,
}

impl Deriver for HalfHeight {
    fn attach(&mut self, _: &mut Document) {}
    fn derive(&mut self, tx: &mut Tx<'_>, changes: &ChangeSet) -> Result<(), Error> {
        for (id, prop) in &changes.updated {
            let Some(n) = tx.doc().get(*id) else { continue };
            if n.name == "boom" {
                return Err(Error::Input("boom".into()));
            }
            if *prop == PropKey::Width && n.props.height != n.props.width / 2.0 {
                let h = n.props.width / 2.0;
                tx.update(*id, PropValue::Height(h))?;
            }
        }
        Ok(())
    }
    fn derive_all(&mut self, _: &mut Tx<'_>) -> Result<(), Error> {
        Ok(())
    }
    fn observe(&mut self, _: &mut Document, _: &ChangeSet) -> ChangeSet {
        *self.observed.borrow_mut() += 1;
        ChangeSet::default()
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

#[test]
fn deriver_ops_join_the_transaction() {
    let (mut s, page) = store();
    let observed = Rc::new(RefCell::new(0));
    s.set_deriver(Box::new(HalfHeight { observed: observed.clone() }));
    let r = s.transact(TxMeta::user("add"), |tx| tx.insert_json(Some(page), None, json!({"type": "rectangle"}))).unwrap().0;
    let (_, changes) = s.transact(TxMeta::user("widen"), |tx| tx.update(r, PropValue::Width(100.0))).unwrap();
    assert_eq!(s.doc().get(r).unwrap().props.height, 50.0);
    assert!(changes.updated.contains(&(r, PropKey::Height)), "derived change is reported");
    // One undo step restores both the edit and the derived value.
    s.undo().unwrap();
    let n = s.doc().get(r).unwrap();
    assert_eq!((n.props.width, n.props.height), (0.0, 0.0));
    s.redo().unwrap();
    assert_eq!(s.doc().get(r).unwrap().props.height, 50.0);
    assert_eq!(*observed.borrow(), 2);
    // A failing deriver rolls back the user's edit too.
    let before = s.doc().to_data();
    let e = s
        .transact(TxMeta::user("x"), |tx| {
            tx.update(r, PropValue::Name("boom".into()))?;
            tx.update(r, PropValue::Width(10.0))
        })
        .unwrap_err();
    assert!(e.to_string().contains("boom"));
    assert_eq!(s.doc().to_data(), before);
    // derive_all can be invoked explicitly through the store.
    s.transact_with_deriver(TxMeta::user("all"), |tx, d| d.map_or(Ok(()), |d| d.derive_all(tx))).unwrap();
}
