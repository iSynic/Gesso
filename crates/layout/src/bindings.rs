//! The bindings pass: every real node with variable bindings holds the bound values for its
//! mode context, written as derived ops in the transaction that changed them. The file then
//! always stores what is drawn, so readers that don't evaluate variables still see it. Instance
//! content resolves its bindings when it is expanded.

use doc::{ChangeSet, Document, NodeKey, PropKey, PropValue};
use rustc_hash::FxHashSet;

use crate::instances::{apply_bindings, inherited_modes};

fn bound(doc: &Document, k: NodeKey) -> bool {
    doc.node(k).is_some_and(|n| n.virt.is_none() && !n.props.bindings.is_empty())
}

/// Real bound nodes whose values `changes` may affect (`None`: all of them).
pub fn targets(doc: &Document, changes: Option<&ChangeSet>) -> Vec<NodeKey> {
    let all = || doc.iter().filter(|(k, _)| bound(doc, *k)).map(|(k, _)| k).collect();
    let Some(ch) = changes else { return all() };
    if ch.variables {
        return all();
    }
    let mut out: FxHashSet<NodeKey> = FxHashSet::default();
    let subtree = |k: NodeKey, out: &mut FxHashSet<NodeKey>| out.extend(doc.descendants(k).into_iter().filter(|d| bound(doc, *d)));
    // New places can mean new mode contexts.
    for id in ch.inserted.iter().chain(&ch.moved) {
        if let Some(k) = doc.key(*id).filter(|k| !doc.is_virtual(*k)) {
            subtree(k, &mut out);
        }
    }
    for (id, prop) in &ch.updated {
        let Some(k) = doc.key(*id).filter(|k| bound(doc, *k) || *prop == PropKey::Modes) else { continue };
        match prop {
            PropKey::Modes => subtree(k, &mut out),
            // A bound property written directly goes back to the variable's value.
            _ => {
                if bound(doc, k) {
                    out.insert(k);
                }
            }
        }
    }
    let mut v: Vec<NodeKey> = out.into_iter().collect();
    v.sort_unstable();
    v
}

/// The updates that make the node at `k` show its bound values.
pub fn updates(doc: &Document, k: NodeKey) -> Vec<PropValue> {
    let Some(n) = doc.node(k) else { return Vec::new() };
    let mut modes = inherited_modes(doc, k);
    modes.extend(n.props.modes.iter().map(|(a, b)| (*a, *b)));
    let mut shown = n.clone();
    apply_bindings(doc, &mut shown, &modes);
    let mut keys = Vec::new();
    n.props.diff(&shown.props, &mut keys);
    let mut out: Vec<PropValue> = keys.into_iter().map(|key| shown.get(key)).collect();
    if shown.name != n.name {
        out.push(PropValue::Name(shown.name));
    }
    if shown.kind != n.kind {
        out.push(PropValue::Kind(shown.kind));
    }
    out
}
