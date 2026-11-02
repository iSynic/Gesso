//! Component commands: create a component, insert an instance, detach, reset overrides, and go
//! to the main component. Edits to an instance's content need nothing here: `Tx::update` turns
//! them into overrides.

use doc::{NodeId, NodeKind, TxMeta};
use kurbo::Point;

use crate::session::{Error, Session};
use crate::transform::top_items;

/// The selection's top items that are real nodes (content inside instances can't be moved,
/// deleted or regrouped; it changes through overrides).
pub fn real_items(s: &Session) -> Vec<NodeId> {
    let doc = s.doc();
    top_items(doc, s.selection()).into_iter().filter(|id| doc.key(*id).is_some_and(|k| !doc.is_virtual(k))).collect()
}

/// The component an instance (real or inside another instance) shows.
pub fn component_of(s: &Session, id: NodeId) -> Option<NodeId> {
    match s.doc().get(id).map(|n| &n.kind) {
        Some(NodeKind::Instance { component, .. }) => Some(*component),
        _ => None,
    }
}

/// Ctrl+Alt+K: the selection becomes a component (a lone frame is converted in place).
pub fn create_component(s: &mut Session) -> Result<(), Error> {
    let ids = real_items(s);
    if ids.is_empty() {
        return Ok(());
    }
    let c = s.transact(TxMeta::user("Create component"), |tx| tx.create_component(&ids, "Component"))?;
    s.select(vec![c]);
    Ok(())
}

/// Places an instance of `component` centred at `at` (the view centre when `None`), in the
/// current scope or page.
pub fn insert_instance(s: &mut Session, component: NodeId, at: Option<Point>) -> Result<NodeId, Error> {
    let doc = s.doc();
    let c = doc.get(component).filter(|n| matches!(n.kind, NodeKind::Component { .. })).ok_or_else(|| Error::Other(format!("{component} is not a component")))?;
    let (w, h, name) = (c.props.width, c.props.height, c.name.clone());
    let parent = s.scope().filter(|p| doc.key(*p).is_some_and(|k| !doc.is_virtual(k))).unwrap_or(s.page());
    let parent_world = s.scene.index.world(parent).unwrap_or_default();
    let at = parent_world.inverse() * at.unwrap_or_else(|| s.view().center());
    let id = s.transact(TxMeta::user("Insert instance"), |tx| {
        tx.insert_json(Some(parent), None, serde_json::json!({"type": "instance", "name": name, "component": component.to_string(), "x": (at.x - w / 2.0).round(), "y": (at.y - h / 2.0).round(), "width": w, "height": h}))
    })?;
    s.select(vec![id]);
    Ok(id)
}

/// Detaches every selected instance; selects the frames that replace them.
pub fn detach(s: &mut Session) -> Result<(), Error> {
    let ids: Vec<NodeId> = real_items(s).into_iter().filter(|id| component_of(s, *id).is_some()).collect();
    if ids.is_empty() {
        return Ok(());
    }
    let frames = s.transact(TxMeta::user("Detach instance"), |tx| ids.iter().map(|id| tx.detach(*id)).collect::<Result<Vec<_>, _>>())?;
    s.select(frames);
    Ok(())
}

/// Resets the overrides of the selected instances, or of the selected nodes inside instances.
pub fn reset_overrides(s: &mut Session) -> Result<(), Error> {
    let doc = s.doc();
    let ids: Vec<NodeId> = s.selection().iter().copied().filter(|id| doc::components::override_target(doc, *id).is_some() || component_of(s, *id).is_some()).collect();
    if ids.is_empty() {
        return Ok(());
    }
    s.transact(TxMeta::user("Reset overrides"), |tx| {
        for id in &ids {
            tx.reset_overrides(*id, None)?;
        }
        Ok(())
    })?;
    Ok(())
}

/// Selects the main component of the selected instance, switching page if needed.
pub fn go_to_main(s: &mut Session) -> Result<(), Error> {
    let Some(c) = s.selection().first().and_then(|id| component_of(s, *id)) else { return Ok(()) };
    let Some(page) = crate::select::page_of(s.doc(), c) else { return Ok(()) };
    s.set_page(page);
    s.select(vec![c]);
    s.zoom_to_selection();
    Ok(())
}
