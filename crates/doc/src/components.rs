//! Component and instance commands on a transaction: creating components, writing and
//! resetting overrides, detaching instances, and remapping ids when subtrees are copied.
//!
//! An instance's content is derived (virtual nodes), so an edit to it is stored as an override
//! on the real instance. [`Tx::update`] does that redirection, so every editing path (panels,
//! gestures, the agent API) writes overrides without knowing about them.

use rustc_hash::FxHashMap;

use crate::document::{DocError, Document, NodeKey};
use crate::id::{Id, NodeId};
use crate::model::Sizing;
use crate::node::{NodeKind, NodeTree, Override};
use crate::props::{PropKey, PropValue};
use crate::store::{Error, Tx};

/// Props of a real instance that belong to its content root (component look), so writing them
/// on the instance writes a `[]` override.
pub const ROOT_LOOK: &[PropKey] = &[PropKey::Fills, PropKey::Strokes, PropKey::Radius];

fn input(msg: impl Into<String>) -> Error {
    Error::Input(msg.into())
}

/// Where an edit to the virtual node `id` is stored: the real instance and the override path.
/// `None` for real nodes.
pub fn override_target(doc: &Document, id: NodeId) -> Option<(NodeId, Vec<Id>)> {
    let start = doc.key(id)?;
    let instance = doc.node(start)?.virt?.instance;
    let mut steps = Vec::new();
    let mut cur = start;
    loop {
        let node = doc.node(cur)?;
        let Some(v) = node.virt else { break };
        let parent_key = node.parent?;
        let parent = doc.node(parent_key)?;
        // A content root stands for its instance; nested instances are path steps.
        let content_root = matches!(parent.kind, NodeKind::Instance { .. });
        if !content_root && (cur == start || matches!(node.kind, NodeKind::Instance { .. })) {
            steps.push(v.source);
        }
        cur = parent_key;
    }
    steps.reverse();
    Some((instance, steps))
}

/// Gives every node in `trees` a fresh id, and points instances and override paths that referred
/// to nodes inside `trees` at the copies.
pub fn remap_ids(trees: &mut [NodeTree], mut new_id: impl FnMut() -> NodeId) {
    let mut map: FxHashMap<NodeId, NodeId> = FxHashMap::default();
    fn assign(t: &mut NodeTree, map: &mut FxHashMap<NodeId, NodeId>, new_id: &mut dyn FnMut() -> NodeId) {
        let id = new_id();
        map.insert(t.id, id);
        t.id = id;
        for c in &mut t.children {
            assign(c, map, new_id);
        }
    }
    fn refs(t: &mut NodeTree, map: &FxHashMap<NodeId, NodeId>) {
        if let NodeKind::Instance { component, overrides } = &mut t.kind {
            *component = map.get(component).copied().unwrap_or(*component);
            for o in overrides {
                for step in &mut o.target {
                    *step = map.get(step).copied().unwrap_or(*step);
                }
            }
        }
        for c in &mut t.children {
            refs(c, map);
        }
    }
    for t in trees.iter_mut() {
        assign(t, &mut map, &mut new_id);
    }
    for t in trees.iter_mut() {
        refs(t, &map);
    }
}

/// Components that instances in `trees` use, transitively, that are not inside `trees`.
pub fn used_components(doc: &Document, trees: &[NodeTree]) -> Vec<NodeTree> {
    let mut inside = std::collections::BTreeSet::new();
    let mut wanted = Vec::new();
    for t in trees {
        t.walk(&mut |n| {
            inside.insert(n.id);
            if let NodeKind::Instance { component, .. } = &n.kind {
                wanted.push(*component);
            }
        });
    }
    let mut out: Vec<NodeTree> = Vec::new();
    while let Some(c) = wanted.pop() {
        if !inside.insert(c) {
            continue;
        }
        let Some(tree) = doc.key(c).and_then(|k| doc.to_tree(k)) else { continue };
        tree.walk(&mut |n| {
            inside.insert(n.id);
            if let NodeKind::Instance { component, .. } = &n.kind {
                wanted.push(*component);
            }
        });
        out.push(tree);
    }
    out
}

impl Tx<'_> {
    fn real_key(&self, id: NodeId) -> Result<NodeKey, Error> {
        let k = self.doc.key(id).ok_or(Error::Doc(DocError::UnknownNode(id)))?;
        if self.doc.is_virtual(k) {
            return Err(Error::Doc(DocError::VirtualNode(id)));
        }
        Ok(k)
    }

    fn instance_parts(&self, id: NodeId) -> Result<(NodeId, Vec<Override>), Error> {
        match self.doc.node(self.real_key(id)?).map(|n| &n.kind) {
            Some(NodeKind::Instance { component, overrides }) => Ok((*component, overrides.clone())),
            _ => Err(input(format!("{id} is not an instance"))),
        }
    }

    /// Sets the override of `value`'s property at `path` on `instance`, replacing any earlier one.
    pub fn set_override(&mut self, instance: NodeId, target: Vec<Id>, value: PropValue) -> Result<(), Error> {
        let (component, mut overrides) = self.instance_parts(instance)?;
        let key = value.key();
        overrides.retain(|o| !(o.target == target && o.value.key() == key));
        overrides.push(Override { target, value });
        self.update_raw(instance, PropValue::Kind(NodeKind::Instance { component, overrides }))
    }

    /// Removes overrides: on a real instance, all of them (`prop: None`) or that property of its
    /// look; on a node inside an instance, those for that node (all, or one property).
    pub fn reset_overrides(&mut self, id: NodeId, prop: Option<PropKey>) -> Result<usize, Error> {
        let (instance, path) = match override_target(self.doc, id) {
            Some(t) => (t.0, Some(t.1)),
            None => (id, prop.map(|_| Vec::new())),
        };
        let (component, mut overrides) = self.instance_parts(instance)?;
        let before = overrides.len();
        overrides.retain(|o| {
            let at = path.as_ref().is_none_or(|p| o.target == *p);
            let of = prop.is_none_or(|k| o.value.key() == k);
            !(at && of)
        });
        let removed = before - overrides.len();
        if removed > 0 {
            self.update_raw(instance, PropValue::Kind(NodeKind::Instance { component, overrides }))?;
        }
        Ok(removed)
    }

    /// Replaces an instance by a frame holding real copies of its content (new ids; the frame
    /// keeps the instance's id). Nested instances stay instances, keeping the overrides that
    /// reached them. Returns the frame's id.
    pub fn detach(&mut self, id: NodeId) -> Result<NodeId, Error> {
        let key = self.real_key(id)?;
        let doc = self.doc();
        let inst = doc.node(key).ok_or(Error::Doc(DocError::UnknownNode(id)))?;
        let NodeKind::Instance { overrides, .. } = &inst.kind else { return Err(input(format!("{id} is not an instance"))) };
        let root = inst.children.first().copied().ok_or_else(|| input(format!("instance {id} shows nothing to detach")))?;
        let mut tree = content_tree(doc, root, overrides).ok_or(Error::Doc(DocError::UnknownNode(id)))?;
        // The frame takes the instance's place, layer settings and name.
        let (p, rp) = (&inst.props, &mut tree.props);
        (rp.x, rp.y, rp.rotation, rp.flip_x, rp.flip_y, rp.width, rp.height) = (p.x, p.y, p.rotation, p.flip_x, p.flip_y, p.width, p.height);
        (rp.sizing_h, rp.sizing_v, rp.constraints, rp.layout_child) = (p.sizing_h, p.sizing_v, p.constraints, p.layout_child);
        (rp.visible, rp.locked, rp.opacity, rp.blend) = (p.visible, p.locked, p.opacity, p.blend);
        rp.effects.extend(p.effects.iter().cloned());
        rp.modes.extend(p.modes.iter().map(|(a, b)| (*a, *b)));
        rp.bindings.extend(p.bindings.iter().map(|(a, b)| (a.clone(), *b)));
        if !inst.name.is_empty() {
            tree.name = inst.name.clone();
        }
        let parent = inst.parent.and_then(|k| doc.node(k)).map(|n| n.id);
        let index = doc.index_of(key);
        remap_ids(std::slice::from_mut(&mut tree), || self.new_id());
        tree.id = id;
        self.delete(id)?;
        self.insert(parent, index, tree)
    }

    /// Makes a component from `ids`: one frame is converted in place (same id); otherwise the
    /// nodes, which must share a parent, are wrapped in a new component at their bounds.
    pub fn create_component(&mut self, ids: &[NodeId], name: &str) -> Result<NodeId, Error> {
        let first = *ids.first().ok_or_else(|| input("nothing to make a component from"))?;
        let keys: Vec<NodeKey> = ids.iter().map(|id| self.real_key(*id)).collect::<Result<_, _>>()?;
        let doc = self.doc();
        if let [k] = keys.as_slice()
            && let Some(n) = doc.node(*k)
            && let NodeKind::Frame { auto_layout, .. } = &n.kind
        {
            let mut tree = doc.to_tree(*k).ok_or(Error::Doc(DocError::UnknownNode(first)))?;
            tree.kind = NodeKind::Component { auto_layout: auto_layout.clone(), description: String::new() };
            if tree.name.is_empty() {
                tree.name = name.into();
            }
            let parent = n.parent.and_then(|p| doc.node(p)).map(|p| p.id);
            let index = doc.index_of(*k);
            self.delete(first)?;
            return self.insert(parent, index, tree);
        }
        let parent = doc.node(keys[0]).and_then(|n| n.parent).ok_or_else(|| input("pages cannot go in a component"))?;
        if keys.iter().any(|k| doc.node(*k).and_then(|n| n.parent) != Some(parent)) {
            return Err(input("the nodes must share a parent"));
        }
        let mut order: Vec<(usize, NodeId, [f64; 4])> = keys
            .iter()
            .filter_map(|k| doc.node(*k).map(|n| (doc.index_of(*k).unwrap_or(0), n.id, [n.props.x, n.props.y, n.props.width, n.props.height])))
            .collect();
        order.sort_by_key(|o| o.0);
        let [x0, y0, x1, y1] = order.iter().fold([f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY], |a, (_, _, r)| {
            [a[0].min(r[0]), a[1].min(r[1]), a[2].max(r[0] + r[2]), a[3].max(r[1] + r[3])]
        });
        let top = order.last().map_or(0, |o| o.0);
        let parent_id = doc.node(parent).map(|n| n.id);
        let comp_id = self.new_id();
        let mut comp = NodeTree::new(comp_id, NodeKind::Component { auto_layout: None, description: String::new() });
        comp.name = name.into();
        (comp.props.x, comp.props.y, comp.props.width, comp.props.height) = (x0, y0, x1 - x0, y1 - y0);
        (comp.props.sizing_h, comp.props.sizing_v) = (Sizing::Fixed, Sizing::Fixed);
        self.insert(parent_id, Some(top + 1), comp)?;
        for (_, id, r) in order {
            self.move_node(id, Some(comp_id), None)?;
            self.update(id, PropValue::X(r[0] - x0))?;
            self.update(id, PropValue::Y(r[1] - y0))?;
        }
        Ok(comp_id)
    }
}

/// Real copy of virtual content from `key` down. Nested instances become instance nodes with
/// the overrides that reached them; `outer` are the detached instance's overrides.
fn content_tree(doc: &Document, key: NodeKey, outer: &[Override]) -> Option<NodeTree> {
    let n = doc.node(key)?;
    let mut t = NodeTree { id: n.id, kind: n.kind.clone(), name: n.name.clone(), props: n.props.clone(), children: Vec::new() };
    if let NodeKind::Instance { overrides, .. } = &mut t.kind {
        let source = n.virt?.source;
        overrides.extend(outer.iter().filter_map(|o| match o.target.split_first() {
            Some((first, rest)) if *first == source => Some(Override { target: rest.to_vec(), value: o.value.clone() }),
            _ => None,
        }));
        return Some(t);
    }
    t.children = n.children.iter().filter_map(|c| content_tree(doc, *c, outer)).collect();
    Some(t)
}
