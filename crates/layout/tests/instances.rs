//! Instance expansion: content, overrides, nested instances, constraints, modes and bindings,
//! propagation of component edits, and undo.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use doc::model::{Color, PaintKind};
use doc::*;
use layout::LayoutDeriver;
use layout::instances::virtual_id;
use serde_json::{Value, json};
use text::{FontDb, TextEngine};

fn store() -> (Store, NodeId) {
    let mut s = Store::default().with_ids(IdGen::seeded(5));
    s.set_deriver(Box::new(LayoutDeriver::new(TextEngine::new(FontDb::bundled_only()))));
    let page = s.transact(TxMeta::user("page"), |tx| tx.insert_json(None, None, json!({"type": "page"}))).unwrap().0;
    (s, page)
}

fn add(s: &mut Store, parent: NodeId, node: Value) -> NodeId {
    s.transact(TxMeta::user("add"), |tx| tx.insert_json(Some(parent), None, node)).unwrap().0
}

fn id(s: &str) -> NodeId {
    s.parse().unwrap()
}

/// The virtual content under `instance` as (id, parent, name, kind, props), depth-first.
fn content(s: &Store, instance: NodeId) -> Vec<(NodeId, NodeId, String, NodeKind, CommonProps)> {
    let d = s.doc();
    let k = d.key(instance).unwrap();
    d.descendants(k)
        .into_iter()
        .skip(1)
        .map(|k| {
            let n = d.node(k).unwrap();
            assert!(n.virt.is_some());
            (n.id, d.node(n.parent.unwrap()).unwrap().id, n.name.clone(), n.kind.clone(), n.props.clone())
        })
        .collect()
}

fn node(s: &Store, id: NodeId) -> &Node {
    s.doc().get(id).unwrap()
}

fn fill(n: &Node) -> Color {
    match n.props.fills[0].kind {
        PaintKind::Solid { color } => color,
        _ => panic!("not solid"),
    }
}

const COMP: &str = "0Cm00000000000000000C1";
const BOX: &str = "0Rc00000000000000000B1";
const LABEL: &str = "0Tx00000000000000000L1";

fn button(s: &mut Store, page: NodeId) -> NodeId {
    add(
        s,
        page,
        json!({"id": COMP, "type": "component", "name": "Button", "width": 100, "height": 40, "fills": [{"type": "solid", "color": "#0000ff"}], "radius": 6,
            "children": [
                {"id": BOX, "type": "rectangle", "name": "Box", "x": 70, "y": 10, "width": 20, "height": 20, "constraints": {"h": "right", "v": "top"}, "fills": [{"type": "solid", "color": "#ffffff"}]},
                {"id": LABEL, "type": "text", "name": "Label", "x": 8, "y": 10, "spans": [{"text": "Go"}], "autoResize": "widthAndHeight"}
            ]}),
    )
}

#[test]
fn instance_shows_component_with_overrides_at_its_size() {
    let (mut s, page) = store();
    let comp = button(&mut s, page);
    let inst = add(
        &mut s,
        page,
        json!({"type": "instance", "component": COMP, "x": 0, "y": 100, "width": 200, "height": 40, "overrides": [
            {"target": [], "prop": "fills", "value": [{"type": "solid", "color": "#00ff00"}]},
            {"target": [LABEL], "prop": "name", "value": "Buy label"}
        ]}),
    );
    let c = content(&s, inst);
    assert_eq!(c.len(), 3);
    let root = virtual_id(inst, comp);
    assert_eq!(c[0].0, root);
    let r = node(&s, root);
    assert!(matches!(r.kind, NodeKind::Frame { clip: true, .. }));
    assert_eq!((r.props.x, r.props.y, r.props.width, r.props.height), (0.0, 0.0, 200.0, 40.0));
    assert_eq!(fill(r), Color::rgb(0, 255, 0), "root override");
    assert_eq!(r.props.radius, doc::model::Radius::Uniform(6.0));
    // Right-pinned box follows the wider instance.
    let b = node(&s, virtual_id(inst, id(BOX)));
    assert_eq!(b.props.x, 170.0);
    assert_eq!(node(&s, virtual_id(inst, id(LABEL))).name, "Buy label");
    // Text inside got laid out (hugging).
    assert!(node(&s, virtual_id(inst, id(LABEL))).props.width > 0.0);
    // Virtual nodes stay out of the saved data and ops can't touch them.
    s.doc().check_invariants().unwrap();
    let saved = s.doc().to_data();
    let mut count = 0;
    saved.pages[0].walk(&mut |_| count += 1);
    assert_eq!(count, 5, "page, component and two children, instance");
    let e = s.transact(TxMeta::user("x"), |tx| tx.update_raw(root, PropValue::Opacity(0.5))).unwrap_err();
    assert!(e.to_string().contains("inside an instance"), "{e}");
    // Through `update` the edit becomes an override on the instance.
    s.transact(TxMeta::user("x"), |tx| tx.update(root, PropValue::Opacity(0.5))).unwrap();
    assert_eq!(node(&s, root).props.opacity, 0.5);
}

#[test]
fn component_edits_reach_instances_and_undo_restores_them() {
    let (mut s, page) = store();
    button(&mut s, page);
    let a = add(&mut s, page, json!({"type": "instance", "component": COMP, "width": 100, "height": 40}));
    let b = add(&mut s, page, json!({"type": "instance", "component": COMP, "y": 50, "width": 150, "height": 40}));
    let before = (content(&s, a), content(&s, b));
    let (_, changes) = s
        .transact(TxMeta::user("recolour"), |tx| tx.update(id(BOX), PropValue::Fills(vec![doc::model::Paint::solid(Color::rgb(255, 0, 0))])))
        .unwrap();
    for inst in [a, b] {
        let v = virtual_id(inst, id(BOX));
        assert_eq!(fill(node(&s, v)), Color::rgb(255, 0, 0));
        assert!(changes.updated.contains(&(v, PropKey::Fills)), "virtual change reported");
    }
    // Structural edit: a new child appears in every instance.
    let dot = add(&mut s, id(COMP), json!({"type": "ellipse", "width": 4, "height": 4}));
    assert!(s.doc().get(virtual_id(b, dot)).is_some());
    s.doc().check_invariants().unwrap();
    let changes = s.undo().unwrap().unwrap();
    assert!(s.doc().get(virtual_id(b, dot)).is_none());
    assert!(changes.deleted.contains(&virtual_id(b, dot)), "undo reports the virtual deletion");
    s.undo().unwrap();
    assert_eq!((content(&s, a), content(&s, b)), before);
    s.redo().unwrap();
    assert_eq!(fill(node(&s, virtual_id(a, id(BOX)))), Color::rgb(255, 0, 0));
    s.doc().check_invariants().unwrap();
    // Deleting an instance removes its content.
    s.transact(TxMeta::user("del"), |tx| tx.delete(a)).unwrap();
    assert!(s.doc().get(virtual_id(a, id(BOX))).is_none());
    s.doc().check_invariants().unwrap();
}

#[test]
fn resizing_an_instance_reapplies_constraints() {
    let (mut s, page) = store();
    button(&mut s, page);
    let inst = add(&mut s, page, json!({"type": "instance", "component": COMP, "width": 100, "height": 40}));
    assert_eq!(node(&s, virtual_id(inst, id(BOX))).props.x, 70.0);
    s.transact(TxMeta::user("w"), |tx| tx.update(inst, PropValue::Width(300.0))).unwrap();
    assert_eq!(node(&s, virtual_id(inst, id(BOX))).props.x, 270.0);
    s.undo().unwrap();
    assert_eq!(node(&s, virtual_id(inst, id(BOX))).props.x, 70.0);
}

#[test]
fn nested_instances_take_path_overrides_outermost_first() {
    let (mut s, page) = store();
    button(&mut s, page);
    let card = add(
        &mut s,
        page,
        json!({"id": "0Cm00000000000000000C2", "type": "component", "name": "Card", "x": 300, "width": 200, "height": 100, "children": [
            {"id": "0In00000000000000000N1", "type": "instance", "component": COMP, "width": 100, "height": 40, "overrides": [{"target": [LABEL], "prop": "name", "value": "Inner"}, {"target": [BOX], "prop": "opacity", "value": 0.5}]}
        ]}),
    );
    let nested = id("0In00000000000000000N1");
    let inst = add(
        &mut s,
        page,
        json!({"type": "instance", "component": card.to_string(), "y": 200, "width": 200, "height": 100, "overrides": [{"target": [nested.to_string(), LABEL], "prop": "name", "value": "Outer"}]}),
    );
    let n = virtual_id(inst, nested);
    assert!(matches!(node(&s, n).kind, NodeKind::Instance { .. }));
    assert_eq!(node(&s, virtual_id(n, id(LABEL))).name, "Outer", "outer override wins");
    assert_eq!(node(&s, virtual_id(n, id(BOX))).props.opacity, 0.5, "inner override kept");
    // The real nested instance inside the component is expanded too.
    assert_eq!(node(&s, virtual_id(nested, id(LABEL))).name, "Inner");
    // Editing the inner component reaches the outer instance through the card.
    s.transact(TxMeta::user("r"), |tx| tx.update(id(BOX), PropValue::Rotation(10.0))).unwrap();
    assert_eq!(node(&s, virtual_id(n, id(BOX))).props.rotation, 10.0);
    s.doc().check_invariants().unwrap();
}

#[test]
fn modes_pick_bound_values_inside_instances() {
    let (mut s, page) = store();
    let (coll, light, dark, var) = (id("0Co00000000000000000V1"), id("0Mo00000000000000000ML"), id("0Mo00000000000000000MD"), id("0Va00000000000000000V2"));
    let vars: Variables = serde_json::from_value(json!([{"id": coll.to_string(), "name": "Theme", "defaultMode": light.to_string(),
        "modes": [{"id": light.to_string(), "name": "Light"}, {"id": dark.to_string(), "name": "Dark"}],
        "variables": [{"id": var.to_string(), "name": "surface", "type": "color", "values": {light.to_string(): "#ffffff", dark.to_string(): "#111111"}}]}]))
    .unwrap();
    s.transact(TxMeta::user("vars"), |tx| tx.set_variables(vars)).unwrap();
    button(&mut s, page);
    s.transact(TxMeta::user("bind"), |tx| tx.update(id(BOX), PropValue::Bindings([("fills/0/color".to_string(), var)].into()))).unwrap();
    let inst = add(&mut s, page, json!({"type": "instance", "component": COMP, "width": 100, "height": 40, "modes": {coll.to_string(): dark.to_string()}}));
    let frame = add(&mut s, page, json!({"type": "frame", "x": 400, "width": 200, "height": 200, "children": [{"type": "instance", "component": COMP, "width": 100, "height": 40}]}));
    let inner = s.doc().get(frame).unwrap().children[0];
    let inner = s.doc().node(inner).unwrap().id;
    assert_eq!(fill(node(&s, virtual_id(inst, id(BOX)))), Color::rgb(0x11, 0x11, 0x11));
    assert_eq!(fill(node(&s, virtual_id(inner, id(BOX)))), Color::rgb(255, 255, 255));
    // Choosing a mode on an ancestor reaches the instance.
    s.transact(TxMeta::user("mode"), |tx| tx.update(frame, PropValue::Modes([(coll, dark)].into()))).unwrap();
    assert_eq!(fill(node(&s, virtual_id(inner, id(BOX)))), Color::rgb(0x11, 0x11, 0x11));
    s.undo().unwrap();
    assert_eq!(fill(node(&s, virtual_id(inner, id(BOX)))), Color::rgb(255, 255, 255));
}

#[test]
fn virtual_ids_are_deterministic_and_distinct() {
    let a = virtual_id(Id(1), Id(2));
    assert_eq!(a, virtual_id(Id(1), Id(2)));
    assert_ne!(a, virtual_id(Id(2), Id(1)));
    assert_ne!(a, virtual_id(Id(1), Id(3)));
}

#[test]
fn auto_layout_components_lay_out_at_the_instance_size() {
    let (mut s, page) = store();
    add(
        &mut s,
        page,
        json!({"id": COMP, "type": "component", "width": 100, "height": 40, "autoLayout": {"mainAlign": "end"},
            "children": [{"id": BOX, "type": "rectangle", "width": 20, "height": 20}]}),
    );
    assert_eq!(node(&s, id(BOX)).props.x, 80.0);
    let inst = add(&mut s, page, json!({"type": "instance", "component": COMP, "y": 50, "width": 200, "height": 40}));
    let b = virtual_id(inst, id(BOX));
    assert_eq!(node(&s, b).props.x, 180.0);
    // A component edit re-expands from the component's geometry and lays out again.
    s.transact(TxMeta::user("w"), |tx| tx.update(id(BOX), PropValue::Width(30.0))).unwrap();
    assert_eq!((node(&s, id(BOX)).props.x, node(&s, b).props.x), (70.0, 170.0));
    s.undo().unwrap();
    assert_eq!(node(&s, b).props.x, 180.0);
    s.transact(TxMeta::user("w"), |tx| tx.update(inst, PropValue::Width(120.0))).unwrap();
    assert_eq!(node(&s, b).props.x, 100.0);
    s.doc().check_invariants().unwrap();
}

/// Timing probe for component-edit fan-out; run with `--ignored --nocapture` in release.
#[test]
#[ignore]
fn fan_out_probe() {
    for n in [100usize, 1000, 5000] {
        let (mut s, page) = store();
        button(&mut s, page);
        s.transact(TxMeta::user("many"), |tx| {
            for i in 0..n {
                tx.insert_json(Some(page), None, json!({"type": "instance", "component": COMP, "x": (i % 100) as f64 * 210.0, "y": 100.0 + (i / 100) as f64 * 50.0, "width": 100 + (i % 3) * 50, "height": 40}))?;
            }
            Ok(())
        })
        .unwrap();
        let mut times = Vec::new();
        for k in 0..20 {
            let t = std::time::Instant::now();
            s.transact(TxMeta::user("edit"), |tx| tx.update(id(BOX), PropValue::Opacity(0.5 + k as f64 / 100.0))).unwrap();
            times.push(t.elapsed().as_secs_f64() * 1e3);
        }
        times.sort_by(f64::total_cmp);
        println!("{n} instances: component edit p50 {:.2} ms, max {:.2} ms, nodes {}", times[10], times[19], s.doc().len());
    }
}

#[test]
fn bound_values_are_written_into_real_nodes() {
    let (mut s, page) = store();
    let (coll, light, dark, var) = (id("0Co00000000000000000V1"), id("0Mo00000000000000000ML"), id("0Mo00000000000000000MD"), id("0Va00000000000000000V2"));
    let vars = |light_value: &str| -> Variables {
        serde_json::from_value(json!([{"id": coll.to_string(), "name": "Theme", "defaultMode": light.to_string(),
            "modes": [{"id": light.to_string(), "name": "Light"}, {"id": dark.to_string(), "name": "Dark"}],
            "variables": [{"id": var.to_string(), "name": "gap", "type": "number", "values": {light.to_string(): light_value.parse::<f64>().unwrap(), dark.to_string(): 4.0}}]}]))
        .unwrap()
    };
    s.transact(TxMeta::user("vars"), |tx| tx.set_variables(vars("12"))).unwrap();
    let frame = add(&mut s, page, json!({"type": "frame", "width": 100, "height": 100, "children": [{"type": "rectangle", "width": 10, "height": 10, "bindings": {"opacity": var.to_string()}}]}));
    let rect = s.doc().node(s.doc().get(frame).unwrap().children[0]).unwrap().id;
    // Opacity takes the number (12 clamps when drawn; the stored value is the variable's).
    assert_eq!(node(&s, rect).props.opacity, 12.0);
    s.transact(TxMeta::user("mode"), |tx| tx.update(frame, PropValue::Modes([(coll, dark)].into()))).unwrap();
    assert_eq!(node(&s, rect).props.opacity, 4.0);
    s.transact(TxMeta::user("vars"), |tx| tx.set_variables(vars("7"))).unwrap();
    assert_eq!(node(&s, rect).props.opacity, 4.0, "dark mode unchanged");
    s.transact(TxMeta::user("mode"), |tx| tx.update(frame, PropValue::Modes(Default::default()))).unwrap();
    assert_eq!(node(&s, rect).props.opacity, 7.0);
    // Writing a bound property directly goes back to the variable's value.
    s.transact(TxMeta::user("x"), |tx| tx.update(rect, PropValue::Opacity(0.3))).unwrap();
    assert_eq!(node(&s, rect).props.opacity, 7.0);
    s.undo().unwrap();
    s.undo().unwrap();
    assert_eq!(node(&s, rect).props.opacity, 4.0);
    // Moving out of the dark frame changes the context.
    s.transact(TxMeta::user("mode"), |tx| tx.update(frame, PropValue::Modes([(coll, dark)].into()))).unwrap();
    s.transact(TxMeta::user("mv"), |tx| tx.move_node(rect, Some(page), None)).unwrap();
    assert_eq!(node(&s, rect).props.opacity, 7.0);
}

#[test]
fn edits_inside_instances_become_overrides() {
    let (mut s, page) = store();
    button(&mut s, page);
    let card = add(
        &mut s,
        page,
        json!({"id": "0Cm00000000000000000C2", "type": "component", "x": 300, "width": 200, "height": 100, "children": [
            {"id": "0In00000000000000000N1", "type": "instance", "component": COMP, "width": 100, "height": 40}
        ]}),
    );
    let nested = id("0In00000000000000000N1");
    let inst = add(&mut s, page, json!({"type": "instance", "component": card.to_string(), "y": 200, "width": 200, "height": 100, "fills": [{"type": "solid", "color": "#ff0000"}]}));
    // Fills given on an instance are its content root's.
    assert!(node(&s, inst).props.fills.is_empty());
    assert_eq!(fill(node(&s, virtual_id(inst, card))), Color::rgb(255, 0, 0));
    let n = virtual_id(inst, nested);
    let label = virtual_id(n, id(LABEL));
    s.transact(TxMeta::user("Override name"), |tx| tx.update(label, PropValue::Name("Pay".into()))).unwrap();
    s.transact(TxMeta::user("Override fills"), |tx| tx.update(n, PropValue::Fills(vec![doc::model::Paint::solid(Color::rgb(1, 2, 3))]))).unwrap();
    let NodeKind::Instance { overrides, .. } = &node(&s, inst).kind else { panic!() };
    let targets: Vec<(Vec<Id>, PropKey)> = overrides.iter().map(|o| (o.target.clone(), o.value.key())).collect();
    assert_eq!(targets, [(vec![], PropKey::Fills), (vec![nested, id(LABEL)], PropKey::Name), (vec![nested], PropKey::Fills)]);
    assert_eq!(node(&s, label).name, "Pay");
    assert_eq!(fill(node(&s, virtual_id(n, id(COMP)))), Color::rgb(1, 2, 3), "a nested instance shows its look on its root");
    // Writing the same property again replaces the override.
    s.transact(TxMeta::user("Override name"), |tx| tx.update(label, PropValue::Name("Pay now".into()))).unwrap();
    let NodeKind::Instance { overrides, .. } = &node(&s, inst).kind else { panic!() };
    assert_eq!(overrides.len(), 3);
    // Reset one node's overrides, then everything.
    assert_eq!(s.transact(TxMeta::user("Reset"), |tx| tx.reset_overrides(label, None)).unwrap().0, 1);
    assert_eq!(node(&s, label).name, "Label");
    assert_eq!(s.transact(TxMeta::user("Reset all"), |tx| tx.reset_overrides(inst, None)).unwrap().0, 2);
    assert!(node(&s, virtual_id(inst, card)).props.fills.is_empty());
}

#[test]
fn detach_keeps_the_look_with_real_nodes() {
    let (mut s, page) = store();
    button(&mut s, page);
    let card = add(
        &mut s,
        page,
        json!({"id": "0Cm00000000000000000C2", "type": "component", "x": 300, "width": 200, "height": 100, "children": [
            {"id": "0In00000000000000000N1", "type": "instance", "component": COMP, "width": 100, "height": 40}
        ]}),
    );
    let nested = id("0In00000000000000000N1");
    let inst = add(
        &mut s,
        page,
        json!({"type": "instance", "name": "Mine", "component": card.to_string(), "y": 200, "width": 250, "height": 100, "opacity": 0.5,
            "overrides": [{"target": [nested.to_string(), LABEL], "prop": "name", "value": "Deep"}]}),
    );
    let n = virtual_id(inst, nested);
    let before: Vec<(String, [f64; 4])> = content(&s, n).iter().map(|c| (c.2.clone(), [c.4.x, c.4.y, c.4.width, c.4.height])).collect();
    s.transact(TxMeta::user("Detach"), |tx| tx.detach(inst)).unwrap();
    let f = node(&s, inst);
    assert!(matches!(f.kind, NodeKind::Frame { .. }) && f.virt.is_none());
    assert_eq!((f.name.as_str(), f.props.opacity, f.props.width), ("Mine", 0.5, 250.0));
    let kid = s.doc().node(f.children[0]).unwrap();
    assert!(kid.virt.is_none());
    let NodeKind::Instance { component, overrides } = &kid.kind else { panic!("nested stays an instance") };
    assert_eq!(*component, id(COMP));
    assert_eq!(overrides[0].target, vec![id(LABEL)], "outer override carried, one step shorter");
    let kid = kid.id;
    assert_eq!(node(&s, virtual_id(kid, id(LABEL))).name, "Deep");
    let after: Vec<(String, [f64; 4])> = content(&s, kid).iter().map(|c| (c.2.clone(), [c.4.x, c.4.y, c.4.width, c.4.height])).collect();
    assert_eq!(after, before, "same content under the kept nested instance");
    s.undo().unwrap();
    assert!(matches!(node(&s, inst).kind, NodeKind::Instance { .. }));
    s.doc().check_invariants().unwrap();
}

#[test]
fn create_component_wraps_or_converts() {
    let (mut s, page) = store();
    let a = add(&mut s, page, json!({"type": "rectangle", "x": 10, "y": 20, "width": 10, "height": 10}));
    let b = add(&mut s, page, json!({"type": "ellipse", "x": 40, "y": 30, "width": 20, "height": 20}));
    let c = s.transact(TxMeta::user("Create component"), |tx| tx.create_component(&[b, a], "Icon")).unwrap().0;
    let comp = node(&s, c);
    assert!(matches!(comp.kind, NodeKind::Component { .. }));
    assert_eq!([comp.props.x, comp.props.y, comp.props.width, comp.props.height], [10.0, 20.0, 50.0, 30.0]);
    assert_eq!([node(&s, a).props.x, node(&s, a).props.y, node(&s, b).props.x], [0.0, 0.0, 30.0]);
    let f = add(&mut s, page, json!({"type": "frame", "name": "Card", "width": 10, "height": 10, "children": [{"type": "rectangle"}]}));
    let same = s.transact(TxMeta::user("Create component"), |tx| tx.create_component(&[f], "x")).unwrap().0;
    assert_eq!(same, f);
    assert!(matches!(node(&s, f).kind, NodeKind::Component { .. }));
    assert_eq!(node(&s, f).name, "Card");
}

#[test]
fn copies_point_at_copied_components() {
    let (mut s, page) = store();
    let frame = add(&mut s, page, json!({"type": "frame", "width": 500, "height": 500}));
    s.transact(TxMeta::user("x"), |tx| {
        tx.insert_json(Some(frame), None, json!({"id": COMP, "type": "component", "width": 100, "height": 40, "children": [{"id": LABEL, "type": "text", "spans": [{"text": "Go"}]}]}))?;
        tx.insert_json(Some(frame), None, json!({"type": "instance", "component": COMP, "y": 100, "width": 100, "height": 40, "overrides": [{"target": [LABEL], "prop": "name", "value": "Copy me"}]}))
    })
    .unwrap();
    let copy = s.transact(TxMeta::user("copy"), |tx| tx.copy(frame, Some(page), None, None)).unwrap().0;
    let kids: Vec<NodeId> = node(&s, copy).children.iter().map(|k| s.doc().node(*k).unwrap().id).collect();
    let NodeKind::Instance { component, overrides } = &node(&s, kids[1]).kind else { panic!() };
    assert_eq!(*component, kids[0], "points at the copied component");
    let label = s.doc().node(node(&s, kids[0]).children[0]).unwrap().id;
    assert_eq!(overrides[0].target, vec![label]);
    assert_eq!(node(&s, virtual_id(kids[1], label)).name, "Copy me");
}
