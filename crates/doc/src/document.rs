//! The in-memory document: a slotmap arena of nodes indexed by stable id, plus variables and the
//! asset table. All mutation goes through [`Document::apply`] (one primitive op, returning its
//! inverse) so that transactions, undo and the op log see every change.

use std::collections::BTreeMap;

use rustc_hash::{FxHashMap, FxHashSet};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use slotmap::{SlotMap, new_key_type};

use crate::finite::Finite;
use crate::id::{Id, NodeId};
use crate::model::{AssetId, AssetMeta, Paint, PaintKind};
use crate::node::{NodeKind, NodeTree};
use crate::op::{Change, ChangeSet, Op};
use crate::props::{CommonProps, PropKey, PropValue};
use crate::variables::{VariableError, Variables};

new_key_type! {
    /// Arena key; only valid for the life of one `Document`. Use [`NodeId`] across sessions.
    pub struct NodeKey;
}

#[derive(Debug, Clone)]
pub struct Node {
    pub id: NodeId,
    pub parent: Option<NodeKey>,
    pub children: Vec<NodeKey>,
    pub name: String,
    pub kind: NodeKind,
    pub props: CommonProps,
    /// Set on nodes shown inside an instance: derived from the component, never saved.
    pub virt: Option<Virtual>,
}

/// Where a node shown inside an instance comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Virtual {
    /// The real instance node whose content this is (the outermost one for nested instances).
    pub instance: NodeId,
    /// The component node this one shows.
    pub source: NodeId,
}

impl Node {
    /// The kind a path needs now that its box has changed from `old` (width, height); `None`
    /// if nothing has to change. A zero size on either side leaves the shape alone, because
    /// scaling to or from zero can't be undone by a later resize.
    pub fn resized_path(&self, old: (f64, f64)) -> Option<NodeKind> {
        let ratio = |new: f64, old: f64| if new > 1e-9 && old > 1e-9 { new / old } else { 1.0 };
        let (kx, ky) = (ratio(self.props.width, old.0), ratio(self.props.height, old.1));
        (kx != 1.0 || ky != 1.0).then(|| self.kind.scaled_path(kx, ky)).flatten()
    }

    /// Sets one property without validation, for derived copies (instance content). A `Kind`
    /// of another type is ignored, as an op would reject it.
    pub fn set(&mut self, value: PropValue) {
        match value {
            PropValue::Name(name) => self.name = name,
            PropValue::Kind(kind) if kind.same_type(&self.kind) => self.kind = kind,
            PropValue::Kind(_) => {}
            other => {
                let _ = self.props.set(other);
            }
        }
    }

    pub fn get(&self, key: PropKey) -> PropValue {
        match key {
            PropKey::Name => PropValue::Name(self.name.clone()),
            PropKey::Kind => PropValue::Kind(self.kind.clone()),
            // `CommonProps::get` covers every other key.
            k => self.props.get(k).unwrap_or(PropValue::Name(String::new())),
        }
    }
}

/// One node of an instance's content, for [`Document::set_expansion`].
#[derive(Debug, Clone)]
pub struct VirtualSpec {
    pub id: NodeId,
    /// The instance, or an earlier spec.
    pub parent: NodeId,
    pub name: String,
    pub kind: NodeKind,
    pub props: CommonProps,
    /// The component node this one shows.
    pub source: NodeId,
}

/// The document contents as plain data: what the file stores (minus the format header).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DocumentData {
    pub pages: Vec<NodeTree>,
    #[serde(default, skip_serializing_if = "Variables::is_empty")]
    pub variables: Variables,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub assets: BTreeMap<AssetId, AssetMeta>,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum DocError {
    #[error("unknown node {0}")]
    UnknownNode(NodeId),
    #[error("id {0} already exists")]
    DuplicateId(NodeId),
    #[error("{ty} node {id} cannot have children")]
    NotContainer { id: NodeId, ty: &'static str },
    #[error("page {0} must be at the top level")]
    PageNotTopLevel(NodeId),
    #[error("{ty} node {id} cannot be at the top level; only pages can")]
    TopLevelNotPage { id: NodeId, ty: &'static str },
    #[error("cannot move {id} into its own descendant {parent}")]
    Cycle { id: NodeId, parent: NodeId },
    #[error("index {index} out of range (0..={len})")]
    IndexOutOfRange { index: usize, len: usize },
    #[error("instance {instance} refers to {component}, which is not a component in this document")]
    UnknownComponent { instance: NodeId, component: NodeId },
    #[error("component {0} would contain an instance of itself")]
    ComponentCycle(NodeId),
    #[error("component {component} is used by {count} instance(s) outside the deleted nodes")]
    ComponentInUse { component: NodeId, count: usize },
    #[error("node {id} refers to unknown asset {asset:?}")]
    UnknownAsset { id: NodeId, asset: AssetId },
    #[error("asset {0:?} is still used by node {1}")]
    AssetInUse(AssetId, NodeId),
    #[error("asset id {0:?} is not a lowercase SHA-256 hex string")]
    InvalidAssetId(AssetId),
    #[error("cannot change node {id} from {from} to {to}")]
    KindChange { id: NodeId, from: &'static str, to: &'static str },
    #[error("node {id} binds {path} to unknown variable {var}")]
    UnknownVariable { id: NodeId, path: String, var: NodeId },
    #[error("node {id} uses mode {mode} of collection {collection}, which doesn't exist")]
    UnknownMode { id: NodeId, collection: Id, mode: Id },
    #[error("node {0} is shown inside an instance; edit its component or override it on the instance")]
    VirtualNode(NodeId),
    #[error("node {id}: {msg}")]
    Invalid { id: NodeId, msg: String },
    #[error("variables: {0}")]
    Variables(#[from] VariableError),
}

#[derive(Debug, Clone, Default)]
pub struct Document {
    nodes: SlotMap<NodeKey, Node>,
    ids: FxHashMap<NodeId, NodeKey>,
    pages: Vec<NodeKey>,
    variables: Variables,
    assets: BTreeMap<AssetId, AssetMeta>,
    /// Component id → instances that refer to it.
    instances: FxHashMap<NodeId, FxHashSet<NodeKey>>,
}

impl Document {
    pub fn new() -> Self {
        Self::default()
    }

    // ---- reading ----------------------------------------------------------------------------

    pub fn len(&self) -> usize {
        self.nodes.len()
    }
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
    pub fn key(&self, id: NodeId) -> Option<NodeKey> {
        self.ids.get(&id).copied()
    }
    pub fn node(&self, key: NodeKey) -> Option<&Node> {
        self.nodes.get(key)
    }
    pub fn get(&self, id: NodeId) -> Option<&Node> {
        self.key(id).and_then(|k| self.nodes.get(k))
    }
    pub fn pages(&self) -> &[NodeKey] {
        &self.pages
    }
    pub fn variables(&self) -> &Variables {
        &self.variables
    }
    pub fn assets(&self) -> &BTreeMap<AssetId, AssetMeta> {
        &self.assets
    }
    pub fn iter(&self) -> impl Iterator<Item = (NodeKey, &Node)> {
        self.nodes.iter()
    }
    /// Instances that refer to the component `id`.
    pub fn instances_of(&self, id: NodeId) -> impl Iterator<Item = NodeKey> + '_ {
        self.instances.get(&id).into_iter().flatten().copied()
    }

    /// Ancestors of `key`, nearest first (excluding `key`).
    pub fn ancestors(&self, key: NodeKey) -> impl Iterator<Item = NodeKey> + '_ {
        std::iter::successors(self.nodes.get(key).and_then(|n| n.parent), |k| self.nodes.get(*k).and_then(|n| n.parent))
    }

    pub fn is_descendant_of(&self, key: NodeKey, ancestor: NodeKey) -> bool {
        key == ancestor || self.ancestors(key).any(|a| a == ancestor)
    }

    /// `key` and all its descendants, parents first.
    pub fn descendants(&self, key: NodeKey) -> Vec<NodeKey> {
        let mut out = Vec::new();
        let mut stack = vec![key];
        while let Some(k) = stack.pop() {
            if let Some(n) = self.nodes.get(k) {
                out.push(k);
                stack.extend(n.children.iter().rev());
            }
        }
        out
    }

    fn siblings(&self, parent: Option<NodeKey>) -> Option<&Vec<NodeKey>> {
        match parent {
            None => Some(&self.pages),
            Some(p) => self.nodes.get(p).map(|n| &n.children),
        }
    }

    /// Position of `key` among its siblings.
    pub fn index_of(&self, key: NodeKey) -> Option<usize> {
        let parent = self.nodes.get(key)?.parent;
        self.siblings(parent)?.iter().position(|k| *k == key)
    }

    /// Copies a node and its subtree out as owned data. Instance content is left out, unless
    /// `key` is itself inside an instance (copying content out makes real nodes of it).
    pub fn to_tree(&self, key: NodeKey) -> Option<NodeTree> {
        let n = self.nodes.get(key)?;
        let keep_virtual = n.virt.is_some() && !matches!(n.kind, NodeKind::Instance { .. });
        Some(NodeTree {
            id: n.id,
            kind: n.kind.clone(),
            name: n.name.clone(),
            props: n.props.clone(),
            children: n.children.iter().filter(|c| keep_virtual || self.nodes.get(**c).is_some_and(|c| c.virt.is_none())).filter_map(|c| self.to_tree(*c)).collect(),
        })
    }

    pub fn to_data(&self) -> DocumentData {
        DocumentData {
            pages: self.pages.iter().filter_map(|k| self.to_tree(*k)).collect(),
            variables: self.variables.clone(),
            assets: self.assets.clone(),
        }
    }

    // ---- loading ----------------------------------------------------------------------------

    /// Builds a document from plain data and validates all of it.
    pub fn from_data(data: DocumentData) -> Result<Self, DocError> {
        let mut doc = Document { variables: data.variables, assets: data.assets, ..Default::default() };
        doc.variables.validate()?;
        for a in doc.assets.keys() {
            if !a.is_valid() {
                return Err(DocError::InvalidAssetId(a.clone()));
            }
        }
        let n: usize = data.pages.iter().map(NodeTree::count).sum();
        doc.nodes.reserve(n);
        doc.ids.reserve(n);
        for page in data.pages {
            doc.check_tree(&page, None)?;
            let k = doc.attach(page, None);
            doc.pages.push(k);
        }
        let comps: Vec<NodeKey> = doc.nodes.iter().filter(|(_, n)| matches!(n.kind, NodeKind::Component { .. })).map(|(k, _)| k).collect();
        doc.check_instances(doc.nodes.keys().filter(|k| matches!(doc.nodes[*k].kind, NodeKind::Instance { .. })).collect())?;
        doc.check_component_cycles(&comps)?;
        Ok(doc)
    }

    // ---- validation -------------------------------------------------------------------------

    /// Checks everything about a subtree that does not depend on where it is in the document,
    /// plus id uniqueness and container rules. `parent` is the kind of the node it goes under.
    fn check_tree(&self, tree: &NodeTree, parent: Option<&NodeKind>) -> Result<(), DocError> {
        let mut seen = FxHashSet::default();
        self.check_subtree(tree, parent, &mut seen)
    }

    fn check_subtree(&self, t: &NodeTree, parent: Option<&NodeKind>, seen: &mut FxHashSet<NodeId>) -> Result<(), DocError> {
        if self.ids.contains_key(&t.id) || !seen.insert(t.id) {
            return Err(DocError::DuplicateId(t.id));
        }
        match parent {
            None if !t.kind.is_page() => return Err(DocError::TopLevelNotPage { id: t.id, ty: t.kind.type_name() }),
            Some(_) if t.kind.is_page() => return Err(DocError::PageNotTopLevel(t.id)),
            _ => {}
        }
        if !t.children.is_empty() && !t.kind.is_container() {
            return Err(DocError::NotContainer { id: t.id, ty: t.kind.type_name() });
        }
        self.check_kind(t.id, &t.kind)?;
        self.check_props(t.id, &t.props)?;
        check_modes_allowed(t.id, &t.kind, &t.props.modes)?;
        for c in &t.children {
            self.check_subtree(c, Some(&t.kind), seen)?;
        }
        Ok(())
    }

    fn check_kind(&self, id: NodeId, kind: &NodeKind) -> Result<(), DocError> {
        let invalid = |msg: String| DocError::Invalid { id, msg };
        if !kind.finite() {
            return Err(invalid("numbers must be finite".into()));
        }
        match kind {
            NodeKind::Polygon { sides, .. } if *sides < 3 => return Err(invalid(format!("polygon needs at least 3 sides, got {sides}"))),
            NodeKind::Path { paths } => {
                for p in paths {
                    kurbo::BezPath::from_svg(&p.d).map_err(|e| invalid(format!("bad path data {:?}: {e}", p.d)))?;
                }
            }
            NodeKind::Image { asset, .. } => self.check_asset(id, asset)?,
            NodeKind::Instance { overrides, .. } => {
                // Targets that no longer resolve are allowed (they stay dormant), but one node's
                // property may only be overridden once.
                let mut seen = FxHashSet::default();
                for o in overrides {
                    if !seen.insert((o.target.as_slice(), o.value.key())) {
                        return Err(invalid(format!("override of {} on {:?} is given twice", o.value.key().as_str(), o.target)));
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn check_props(&self, id: NodeId, p: &CommonProps) -> Result<(), DocError> {
        if !p.finite() {
            return Err(DocError::Invalid { id, msg: "numbers must be finite".into() });
        }
        for paint in p.fills.iter().chain(p.strokes.iter().map(|s| &s.paint)) {
            self.check_paint(id, paint)?;
        }
        check_bindings(id, &p.bindings, &self.variables)?;
        check_modes(id, &p.modes, &self.variables)
    }

    fn check_paint(&self, id: NodeId, paint: &Paint) -> Result<(), DocError> {
        if let PaintKind::Image { asset, .. } = &paint.kind {
            self.check_asset(id, asset)?;
        }
        Ok(())
    }

    fn check_asset(&self, id: NodeId, asset: &AssetId) -> Result<(), DocError> {
        if self.assets.contains_key(asset) { Ok(()) } else { Err(DocError::UnknownAsset { id, asset: asset.clone() }) }
    }

    /// Every instance must point at an existing component.
    fn check_instances(&self, keys: Vec<NodeKey>) -> Result<(), DocError> {
        for k in keys {
            let n = &self.nodes[k];
            if let NodeKind::Instance { component, .. } = &n.kind {
                let ok = self.get(*component).is_some_and(|c| matches!(c.kind, NodeKind::Component { .. }));
                if !ok {
                    return Err(DocError::UnknownComponent { instance: n.id, component: *component });
                }
            }
        }
        Ok(())
    }

    /// No component may (directly or through other components) contain an instance of itself.
    /// `start` lists the components whose content may have changed.
    fn check_component_cycles(&self, start: &[NodeKey]) -> Result<(), DocError> {
        let mut done: FxHashSet<NodeKey> = FxHashSet::default();
        for &c in start {
            // Iterative DFS over component → components used by instances inside it.
            let mut path: Vec<(NodeKey, Vec<NodeKey>)> = vec![(c, self.used_components(c))];
            let mut on_path: FxHashSet<NodeKey> = FxHashSet::from_iter([c]);
            while let Some((_, next)) = path.last_mut() {
                match next.pop() {
                    Some(d) if on_path.contains(&d) => return Err(DocError::ComponentCycle(self.nodes[d].id)),
                    Some(d) if done.contains(&d) => {}
                    Some(d) => {
                        on_path.insert(d);
                        path.push((d, self.used_components(d)));
                    }
                    None => {
                        if let Some((k, _)) = path.pop() {
                            on_path.remove(&k);
                            done.insert(k);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Components referenced by instances inside `component`'s subtree.
    fn used_components(&self, component: NodeKey) -> Vec<NodeKey> {
        let mut out = Vec::new();
        for k in self.descendants(component) {
            if let NodeKind::Instance { component: c, .. } = &self.nodes[k].kind
                && let Some(ck) = self.key(*c) {
                    out.push(ck);
                }
        }
        out
    }

    /// Components that contain `key` (inclusive), whose content changes when `key` changes.
    fn enclosing_components(&self, key: NodeKey) -> Vec<NodeKey> {
        std::iter::once(key)
            .chain(self.ancestors(key))
            .filter(|k| matches!(self.nodes[*k].kind, NodeKind::Component { .. }))
            .collect()
    }

    // ---- virtual nodes ------------------------------------------------------------------------
    //
    // An instance's content lives in the arena as virtual nodes under it, so everything that
    // walks children (layout, rendering, hit-testing) sees it. They are derived state: ops reject
    // them, `to_tree`/`to_data` leave them out, and undo never records them.

    pub fn is_virtual(&self, key: NodeKey) -> bool {
        self.nodes.get(key).is_some_and(|n| n.virt.is_some())
    }

    /// Makes the virtual content under the real instance `instance` match `specs` (depth-first,
    /// each parent before its children). Nodes whose id survives are updated in place, so an
    /// unchanged expansion costs comparisons only. What changed goes into `changes` as for ops:
    /// the topmost inserted and deleted nodes, every updated prop, and moved nodes.
    pub fn set_expansion(&mut self, instance: NodeKey, specs: Vec<VirtualSpec>, changes: &mut ChangeSet) {
        let Some(inst) = self.nodes.get(instance) else { return };
        if inst.virt.is_some() {
            return;
        }
        let inst_id = inst.id;
        let old: FxHashSet<NodeKey> = self.descendants(instance).into_iter().skip(1).collect();
        let mut kids: FxHashMap<NodeKey, Vec<NodeKey>> = FxHashMap::default();
        kids.insert(instance, Vec::new());
        let mut fresh: FxHashSet<NodeKey> = FxHashSet::default();
        let mut keep: FxHashSet<NodeKey> = FxHashSet::default();
        for spec in specs {
            let Some(&pk) = self.ids.get(&spec.parent) else { continue };
            if pk != instance && !keep.contains(&pk) && !fresh.contains(&pk) {
                continue;
            }
            let virt = Some(Virtual { instance: inst_id, source: spec.source });
            let key = match self.ids.get(&spec.id).copied() {
                Some(k) if old.contains(&k) && !keep.contains(&k) => {
                    let n = &mut self.nodes[k];
                    if n.parent != Some(pk) {
                        n.parent = Some(pk);
                        changes.moved.push(spec.id);
                    }
                    if n.name != spec.name {
                        n.name = spec.name;
                        changes.updated.push((spec.id, PropKey::Name));
                    }
                    if n.kind != spec.kind {
                        n.kind = spec.kind;
                        changes.updated.push((spec.id, PropKey::Kind));
                    }
                    if n.props != spec.props {
                        let mut keys = Vec::new();
                        n.props.diff(&spec.props, &mut keys);
                        changes.updated.extend(keys.into_iter().map(|k| (spec.id, k)));
                        n.props = spec.props;
                    }
                    n.virt = virt;
                    keep.insert(k);
                    k
                }
                // Taken by a real node or an earlier spec: astronomically unlikely, skipped.
                Some(_) => continue,
                None => {
                    let node = Node { id: spec.id, parent: Some(pk), children: Vec::new(), name: spec.name, kind: spec.kind, props: spec.props, virt };
                    let k = self.nodes.insert(node);
                    self.ids.insert(spec.id, k);
                    if !fresh.contains(&pk) {
                        changes.inserted.push(spec.id);
                    }
                    fresh.insert(k);
                    k
                }
            };
            kids.entry(pk).or_default().push(key);
            kids.entry(key).or_default();
        }
        for (parent, list) in kids {
            let n = &mut self.nodes[parent];
            if n.children == list {
                continue;
            }
            // Survivors that changed place among their siblings count as moved.
            let before = std::mem::replace(&mut n.children, list);
            let pos: FxHashMap<NodeKey, usize> = before.iter().enumerate().map(|(i, k)| (*k, i)).collect();
            for (i, k) in self.nodes[parent].children.iter().enumerate() {
                if keep.contains(k) && pos.get(k).is_some_and(|p| *p != i) {
                    changes.moved.push(self.nodes[*k].id);
                }
            }
        }
        for k in old {
            if keep.contains(&k) {
                continue;
            }
            if let Some(n) = self.nodes.remove(k) {
                self.ids.remove(&n.id);
                if n.parent.is_some_and(|p| p == instance || keep.contains(&p)) {
                    changes.deleted.push(n.id);
                }
            }
        }
    }

    /// Sets a prop on a virtual node (layout results for instance content). Returns the change,
    /// or `None` for a real or unknown node.
    pub fn set_virtual(&mut self, id: NodeId, value: PropValue) -> Option<Change> {
        let key = self.key(id)?;
        let n = self.nodes.get_mut(key)?;
        n.virt?;
        let prop = value.key();
        let old = (n.props.width, n.props.height);
        n.set(value);
        if let Some(kind) = n.resized_path(old) {
            n.kind = kind;
        }
        Some(Change::Updated(id, prop))
    }

    // ---- structural primitives (no validation) ----------------------------------------------

    fn attach(&mut self, tree: NodeTree, parent: Option<NodeKey>) -> NodeKey {
        let NodeTree { id, kind, name, props, children } = tree;
        let key = self.nodes.insert(Node { id, parent, children: Vec::with_capacity(children.len()), name, kind, props, virt: None });
        self.ids.insert(id, key);
        if let NodeKind::Instance { component, .. } = &self.nodes[key].kind {
            self.instances.entry(*component).or_default().insert(key);
        }
        for c in children {
            let ck = self.attach(c, Some(key));
            self.nodes[key].children.push(ck);
        }
        key
    }

    fn detach(&mut self, key: NodeKey) -> Option<NodeTree> {
        let n = self.nodes.remove(key)?;
        self.ids.remove(&n.id);
        if let NodeKind::Instance { component, .. } = &n.kind
            && let Some(set) = self.instances.get_mut(component) {
                set.remove(&key);
                if set.is_empty() {
                    self.instances.remove(component);
                }
            }
        let children = n.children.iter().filter_map(|c| self.detach(*c)).collect();
        if n.virt.is_some() {
            return None;
        }
        Some(NodeTree { id: n.id, kind: n.kind, name: n.name, props: n.props, children })
    }

    fn siblings_mut(&mut self, parent: Option<NodeKey>) -> Option<&mut Vec<NodeKey>> {
        match parent {
            None => Some(&mut self.pages),
            Some(p) => self.nodes.get_mut(p).map(|n| &mut n.children),
        }
    }

    fn resolve_parent(&self, parent: Option<NodeId>) -> Result<Option<NodeKey>, DocError> {
        parent.map(|p| self.node_key(p)).transpose()
    }

    /// The key of a real node: ops never touch virtual ones.
    fn node_key(&self, id: NodeId) -> Result<NodeKey, DocError> {
        let key = self.key(id).ok_or(DocError::UnknownNode(id))?;
        if self.nodes[key].virt.is_some() {
            return Err(DocError::VirtualNode(id));
        }
        Ok(key)
    }

    // ---- ops --------------------------------------------------------------------------------

    /// Validates and applies one primitive op. On success returns the op that undoes it and a
    /// description of what changed; on error the document is unchanged.
    pub fn apply(&mut self, op: Op) -> Result<(Op, Change), DocError> {
        match op {
            Op::Insert { parent, index, node } => self.op_insert(parent, index, *node),
            Op::Delete { id } => self.op_delete(id),
            Op::Update { id, value } => self.op_update(id, value),
            Op::Move { id, parent, index } => self.op_move(id, parent, index),
            Op::SetVariables { variables } => self.op_set_variables(variables),
            Op::PutAsset { id, meta } => self.op_put_asset(id, meta),
        }
    }

    fn op_insert(&mut self, parent: Option<NodeId>, index: usize, tree: NodeTree) -> Result<(Op, Change), DocError> {
        let pk = self.resolve_parent(parent)?;
        let len = self.siblings(pk).map_or(0, Vec::len);
        if index > len {
            return Err(DocError::IndexOutOfRange { index, len });
        }
        let parent_kind = pk.map(|k| &self.nodes[k].kind);
        if let (Some(pk), Some(kind)) = (pk, parent_kind)
            && !kind.is_container() {
                return Err(DocError::NotContainer { id: self.nodes[pk].id, ty: kind.type_name() });
            }
        self.check_tree(&tree, parent_kind)?;
        let id = tree.id;
        let key = self.attach(tree, pk);
        if let Some(s) = self.siblings_mut(pk) {
            s.insert(index, key);
        }
        // Instances may point into the new subtree, so check them after attaching.
        let new_keys = self.descendants(key);
        let instances: Vec<NodeKey> = new_keys.iter().copied().filter(|k| matches!(self.nodes[*k].kind, NodeKind::Instance { .. })).collect();
        let mut comps: Vec<NodeKey> = new_keys.iter().copied().filter(|k| matches!(self.nodes[*k].kind, NodeKind::Component { .. })).collect();
        if !instances.is_empty() {
            comps.extend(self.enclosing_components(key));
        }
        let checked = self.check_instances(instances).and_then(|_| self.check_component_cycles(&comps));
        if let Err(e) = checked {
            if let Some(s) = self.siblings_mut(pk) {
                s.remove(index);
            }
            self.detach(key);
            return Err(e);
        }
        Ok((Op::Delete { id }, Change::Inserted(id)))
    }

    fn op_delete(&mut self, id: NodeId) -> Result<(Op, Change), DocError> {
        let key = self.node_key(id)?;
        // Components inside the subtree must not be used from outside it.
        for k in self.descendants(key) {
            if let NodeKind::Component { .. } = self.nodes[k].kind {
                let cid = self.nodes[k].id;
                let outside = self.instances_of(cid).filter(|i| !self.is_descendant_of(*i, key)).count();
                if outside > 0 {
                    return Err(DocError::ComponentInUse { component: cid, count: outside });
                }
            }
        }
        let parent = self.nodes[key].parent;
        let index = self.index_of(key).unwrap_or(0);
        if let Some(s) = self.siblings_mut(parent) {
            s.remove(index);
        }
        let tree = self.detach(key).ok_or(DocError::UnknownNode(id))?;
        let parent = parent.map(|p| self.nodes[p].id);
        Ok((Op::Insert { parent, index, node: Box::new(tree) }, Change::Deleted(id)))
    }

    fn op_update(&mut self, id: NodeId, value: PropValue) -> Result<(Op, Change), DocError> {
        let key = self.node_key(id)?;
        let prop = value.key();
        match &value {
            PropValue::Name(_) => {}
            PropValue::Kind(kind) => {
                let old = &self.nodes[key].kind;
                if !old.same_type(kind) {
                    return Err(DocError::KindChange { id, from: old.type_name(), to: kind.type_name() });
                }
                self.check_kind(id, kind)?;
            }
            other => {
                let mut scratch = CommonProps::default();
                let _ = scratch.set(other.clone());
                // Only the field being set is non-default, so this checks just the new value.
                self.check_props(id, &scratch)?;
                check_modes_allowed(id, &self.nodes[key].kind, &scratch.modes)?;
            }
        }
        let old = match value {
            PropValue::Name(name) => PropValue::Name(std::mem::replace(&mut self.nodes[key].name, name)),
            PropValue::Kind(kind) => {
                let old = std::mem::replace(&mut self.nodes[key].kind, kind);
                if let Err(e) = self.reindex_instance(key, &old) {
                    let new = std::mem::replace(&mut self.nodes[key].kind, old.clone());
                    let _ = self.reindex_instance(key, &new);
                    return Err(e);
                }
                PropValue::Kind(old)
            }
            other => self.nodes[key].props.set(other).unwrap_or_else(|v| v),
        };
        Ok((Op::Update { id, value: old }, Change::Updated(id, prop)))
    }

    /// After an instance's kind changed from `old`, updates the instance index and validates.
    fn reindex_instance(&mut self, key: NodeKey, old: &NodeKind) -> Result<(), DocError> {
        let new_comp = match &self.nodes[key].kind {
            NodeKind::Instance { component, .. } => Some(*component),
            _ => None,
        };
        let old_comp = match old {
            NodeKind::Instance { component, .. } => Some(*component),
            _ => None,
        };
        if new_comp == old_comp {
            return Ok(());
        }
        if let Some(c) = old_comp
            && let Some(set) = self.instances.get_mut(&c) {
                set.remove(&key);
                if set.is_empty() {
                    self.instances.remove(&c);
                }
            }
        if let Some(c) = new_comp {
            self.instances.entry(c).or_default().insert(key);
        }
        self.check_instances(vec![key])?;
        self.check_component_cycles(&self.enclosing_components(key))
    }

    fn op_move(&mut self, id: NodeId, parent: Option<NodeId>, index: usize) -> Result<(Op, Change), DocError> {
        let key = self.node_key(id)?;
        let new_parent = self.resolve_parent(parent)?;
        let kind = &self.nodes[key].kind;
        match new_parent {
            None if !kind.is_page() => return Err(DocError::TopLevelNotPage { id, ty: kind.type_name() }),
            Some(_) if kind.is_page() => return Err(DocError::PageNotTopLevel(id)),
            Some(p) => {
                let pk = &self.nodes[p].kind;
                if !pk.is_container() {
                    return Err(DocError::NotContainer { id: self.nodes[p].id, ty: pk.type_name() });
                }
                if self.is_descendant_of(p, key) {
                    return Err(DocError::Cycle { id, parent: self.nodes[p].id });
                }
            }
            None => {}
        }
        let old_parent = self.nodes[key].parent;
        let old_index = self.index_of(key).unwrap_or(0);
        let len_after = self.siblings(new_parent).map_or(0, Vec::len) - usize::from(old_parent == new_parent);
        if index > len_after {
            return Err(DocError::IndexOutOfRange { index, len: len_after });
        }
        let mut before = self.enclosing_components(key);
        self.relink(key, old_parent, old_index, new_parent, index);
        // Moving content into a component can create a component cycle.
        let has_instances = self.descendants(key).iter().any(|k| matches!(self.nodes[*k].kind, NodeKind::Instance { .. }));
        if has_instances {
            before.extend(self.enclosing_components(key));
            if let Err(e) = self.check_component_cycles(&before) {
                self.relink(key, new_parent, index, old_parent, old_index);
                return Err(e);
            }
        }
        let old_parent_id = old_parent.map(|p| self.nodes[p].id);
        Ok((Op::Move { id, parent: old_parent_id, index: old_index }, Change::Moved(id)))
    }

    fn relink(&mut self, key: NodeKey, from: Option<NodeKey>, from_index: usize, to: Option<NodeKey>, to_index: usize) {
        if let Some(s) = self.siblings_mut(from) {
            s.remove(from_index);
        }
        if let Some(s) = self.siblings_mut(to) {
            s.insert(to_index, key);
        }
        self.nodes[key].parent = to;
    }

    fn op_set_variables(&mut self, variables: Variables) -> Result<(Op, Change), DocError> {
        variables.validate()?;
        for (_, n) in self.nodes.iter().filter(|(_, n)| n.virt.is_none()) {
            check_bindings(n.id, &n.props.bindings, &variables)?;
            check_modes(n.id, &n.props.modes, &variables)?;
        }
        let old = std::mem::replace(&mut self.variables, variables);
        Ok((Op::SetVariables { variables: old }, Change::Variables))
    }

    fn op_put_asset(&mut self, id: AssetId, meta: Option<AssetMeta>) -> Result<(Op, Change), DocError> {
        if !id.is_valid() {
            return Err(DocError::InvalidAssetId(id));
        }
        if meta.is_none()
            && let Some(user) = self.asset_user(&id) {
                return Err(DocError::AssetInUse(id, user));
            }
        let old = match meta {
            Some(m) => self.assets.insert(id.clone(), m),
            None => self.assets.remove(&id),
        };
        Ok((Op::PutAsset { id: id.clone(), meta: old }, Change::Asset(id)))
    }

    /// First node that uses `asset`, if any.
    fn asset_user(&self, asset: &AssetId) -> Option<NodeId> {
        let uses = |p: &Paint| matches!(&p.kind, PaintKind::Image { asset: a, .. } if a == asset);
        self.nodes.values().filter(|n| n.virt.is_none()).find_map(|n| {
            let direct = matches!(&n.kind, NodeKind::Image { asset: a, .. } if a == asset);
            let paints = n.props.fills.iter().chain(n.props.strokes.iter().map(|s| &s.paint)).any(uses);
            (direct || paints).then_some(n.id)
        })
    }

    /// Internal consistency check used by tests: parent/child links agree, the id map is a
    /// bijection, the instance index is exact, and there are no cycles.
    pub fn check_invariants(&self) -> Result<(), String> {
        if self.ids.len() != self.nodes.len() {
            return Err(format!("id map has {} entries for {} nodes", self.ids.len(), self.nodes.len()));
        }
        let mut reached = 0usize;
        for (i, &p) in self.pages.iter().enumerate() {
            let n = self.nodes.get(p).ok_or(format!("page {i} dangling"))?;
            if n.parent.is_some() || !n.kind.is_page() {
                return Err(format!("page {} has a parent or is not a page", n.id));
            }
            for k in self.descendants(p) {
                reached += 1;
                let n = &self.nodes[k];
                if self.ids.get(&n.id) != Some(&k) {
                    return Err(format!("id map wrong for {}", n.id));
                }
                for &c in &n.children {
                    if self.nodes.get(c).and_then(|c| c.parent) != Some(k) {
                        return Err(format!("child of {} has wrong parent", n.id));
                    }
                }
                if let NodeKind::Instance { component, .. } = &n.kind
                    && n.virt.is_none()
                    && !self.instances.get(component).is_some_and(|s| s.contains(&k)) {
                        return Err(format!("instance {} missing from index", n.id));
                    }
                let parent_virtual = n.parent.is_some_and(|p| self.nodes[p].virt.is_some());
                let parent_instance = n.parent.is_some_and(|p| matches!(self.nodes[p].kind, NodeKind::Instance { .. }));
                if n.virt.is_some() != (parent_virtual || parent_instance) {
                    return Err(format!("node {} is virtual: {}, but its parent disagrees", n.id, n.virt.is_some()));
                }
            }
        }
        if reached != self.nodes.len() {
            return Err(format!("{} nodes reachable of {}", reached, self.nodes.len()));
        }
        let indexed: usize = self.instances.values().map(|s| s.len()).sum();
        let actual = self.nodes.values().filter(|n| n.virt.is_none() && matches!(n.kind, NodeKind::Instance { .. })).count();
        if indexed != actual {
            return Err(format!("instance index has {indexed}, document has {actual}"));
        }
        Ok(())
    }
}

/// Every bound variable exists and every path starts at a property the node format has.
fn check_bindings(id: NodeId, bindings: &BTreeMap<String, Id>, variables: &Variables) -> Result<(), DocError> {
    for (path, var) in bindings {
        if variables.find(*var).is_none() {
            return Err(DocError::UnknownVariable { id, path: path.clone(), var: *var });
        }
        if !crate::bind::known_path(path) {
            return Err(DocError::Invalid { id, msg: format!("binding path {path:?} doesn't name a property") });
        }
    }
    Ok(())
}

/// Every chosen mode belongs to an existing collection.
fn check_modes(id: NodeId, modes: &BTreeMap<Id, Id>, variables: &Variables) -> Result<(), DocError> {
    for (&collection, &mode) in modes {
        let ok = variables.collections.iter().any(|c| c.id == collection && c.modes.iter().any(|m| m.id == mode));
        if !ok {
            return Err(DocError::UnknownMode { id, collection, mode });
        }
    }
    Ok(())
}

fn check_modes_allowed(id: NodeId, kind: &NodeKind, modes: &BTreeMap<Id, Id>) -> Result<(), DocError> {
    if modes.is_empty() || kind.takes_modes() {
        Ok(())
    } else {
        Err(DocError::Invalid { id, msg: format!("a {} can't choose modes; set them on an enclosing frame", kind.type_name()) })
    }
}
