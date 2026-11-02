//! Convenience commands on a transaction, shaped like the agent API (`insert`, `update`,
//! `replace`, `move`, `copy`, `delete`, `get`, `find`, `setVariables`). They take and return JSON
//! where an agent would, and reject unknown property names instead of ignoring them.

use std::collections::{BTreeSet, HashMap};
use std::sync::OnceLock;

use serde_json::{Map, Value};

use crate::document::{DocError, Node, NodeKey};
use crate::id::NodeId;
use crate::model::{AssetId, AssetMeta};
use crate::components::{ROOT_LOOK, override_target, remap_ids};
use crate::node::{NodeKind, NodeTree, Override};
use crate::op::Op;
use crate::props::{CommonProps, PropKey, PropValue};
use crate::store::{Error, Tx};
use crate::variables::Variables;

/// Keys every node object may have besides its common and kind-specific properties.
const NODE_KEYS: [&str; 4] = ["id", "type", "name", "children"];

/// Field names allowed per node type, read from the JSON Schema so they never drift from the
/// serde definitions.
fn kind_fields() -> &'static HashMap<String, BTreeSet<String>> {
    static FIELDS: OnceLock<HashMap<String, BTreeSet<String>>> = OnceLock::new();
    FIELDS.get_or_init(|| {
        let schema = serde_json::to_value(schemars::schema_for!(NodeKind)).unwrap_or_default();
        let mut out = HashMap::new();
        for variant in schema.get("oneOf").and_then(Value::as_array).into_iter().flatten() {
            let Some(props) = variant.get("properties").and_then(Value::as_object) else { continue };
            let Some(ty) = props.get("type").and_then(|t| t.get("const")).and_then(Value::as_str) else { continue };
            out.insert(ty.to_string(), props.keys().filter(|k| *k != "type").cloned().collect());
        }
        out
    })
}

fn common_fields() -> &'static BTreeSet<String> {
    static FIELDS: OnceLock<BTreeSet<String>> = OnceLock::new();
    FIELDS.get_or_init(|| PropKey::COMMON.iter().map(|k| k.as_str()).collect())
}

fn input(msg: impl Into<String>) -> Error {
    Error::Input(msg.into())
}

/// Checks a node object (recursively) for unknown keys and fills in missing ids.
fn prepare_node(tx: &mut Tx<'_>, v: &mut Value, path: &str) -> Result<(), Error> {
    let obj = v.as_object_mut().ok_or_else(|| input(format!("{path}: node must be an object")))?;
    let ty = obj.get("type").and_then(Value::as_str).ok_or_else(|| input(format!("{path}: missing \"type\"")))?.to_string();
    let kind_keys = kind_fields().get(&ty).ok_or_else(|| {
        let mut types: Vec<&str> = kind_fields().keys().map(String::as_str).collect();
        types.sort_unstable();
        input(format!("{path}: unknown node type {ty:?}; the types are {}", types.join(", ")))
    })?;
    for k in obj.keys() {
        if !NODE_KEYS.contains(&k.as_str()) && !common_fields().contains(k) && !kind_keys.contains(k) {
            return Err(input(format!("{path}: unknown property {k:?} for a {ty} node")));
        }
    }
    if !obj.contains_key("id") {
        obj.insert("id".into(), Value::String(tx.new_id().to_string()));
    }
    if let Some(children) = obj.get_mut("children").and_then(Value::as_array_mut) {
        for (i, c) in children.iter_mut().enumerate() {
            prepare_node(tx, c, &format!("{path}.children[{i}]"))?;
        }
    }
    Ok(())
}

/// An instance's fills, strokes and radius belong to its content root: move any given on the
/// instance itself into `[]` overrides.
fn normalize_instances(t: &mut NodeTree) {
    if let NodeKind::Instance { overrides, .. } = &mut t.kind {
        let p = &mut t.props;
        let mut look = Vec::new();
        if !p.fills.is_empty() {
            look.push(PropValue::Fills(std::mem::take(&mut p.fills)));
        }
        if !p.strokes.is_empty() {
            look.push(PropValue::Strokes(std::mem::take(&mut p.strokes)));
        }
        if p.radius != Default::default() {
            look.push(PropValue::Radius(std::mem::take(&mut p.radius)));
        }
        for v in look {
            if !overrides.iter().any(|o| o.target.is_empty() && o.value.key() == v.key()) {
                overrides.push(Override { target: Vec::new(), value: v });
            }
        }
    }
    t.children.iter_mut().for_each(normalize_instances);
}

impl Tx<'_> {
    fn key_of(&self, id: NodeId) -> Result<NodeKey, Error> {
        self.doc.key(id).ok_or(Error::Doc(DocError::UnknownNode(id)))
    }

    fn child_count(&self, parent: Option<NodeId>) -> Result<usize, Error> {
        Ok(match parent {
            None => self.doc.pages().len(),
            Some(p) => self.doc.node(self.key_of(p)?).map_or(0, |n| n.children.len()),
        })
    }

    /// Inserts `node` under `parent` at `index` (`None` = last). Returns the node's id.
    pub fn insert(&mut self, parent: Option<NodeId>, index: Option<usize>, mut node: NodeTree) -> Result<NodeId, Error> {
        normalize_instances(&mut node);
        let index = match index {
            Some(i) => i,
            None => self.child_count(parent)?,
        };
        let id = node.id;
        self.apply(Op::Insert { parent, index, node: Box::new(node) })?;
        Ok(id)
    }

    /// Inserts a node given as JSON; `id`s may be omitted and are generated.
    pub fn insert_json(&mut self, parent: Option<NodeId>, index: Option<usize>, mut node: Value) -> Result<NodeId, Error> {
        prepare_node(self, &mut node, "node")?;
        let tree: NodeTree = serde_json::from_value(node).map_err(|e| input(e.to_string()))?;
        self.insert(parent, index, tree)
    }

    /// Sets one property. On a node inside an instance this writes an override on the
    /// instance; on an instance, its fills, strokes and radius are its content root's, so they
    /// are written as `[]` overrides too.
    pub fn update(&mut self, id: NodeId, value: PropValue) -> Result<(), Error> {
        if let Some((instance, path)) = override_target(self.doc, id) {
            return self.set_override(instance, path, value);
        }
        if ROOT_LOOK.contains(&value.key()) && self.doc.get(id).is_some_and(|n| n.virt.is_none() && matches!(n.kind, NodeKind::Instance { .. })) {
            return self.set_override(id, Vec::new(), value);
        }
        self.update_raw(id, value)
    }

    /// Sets one property with no redirection to overrides.
    pub fn update_raw(&mut self, id: NodeId, value: PropValue) -> Result<(), Error> {
        Ok(self.apply(Op::Update { id, value })?)
    }

    /// Updates several properties from a JSON object, e.g. `{"opacity": 0.5, "spans": [...]}`.
    /// Common properties and `name` update directly; type-specific fields update the node's kind.
    pub fn update_json(&mut self, id: NodeId, props: &Map<String, Value>) -> Result<(), Error> {
        let key = self.key_of(id)?;
        let mut kind_patch = Map::new();
        for (k, v) in props {
            match k.as_str() {
                "id" | "type" | "children" => return Err(input(format!("{k:?} cannot be updated"))),
                "name" => {
                    let name = v.as_str().ok_or_else(|| input("name must be a string"))?;
                    self.update(id, PropValue::Name(name.into()))?;
                }
                _ if common_fields().contains(k) => {
                    let pk = PropKey::parse(k).ok_or_else(|| input(format!("unknown property {k:?}")))?;
                    let value = PropValue::from_json(pk, v.clone()).map_err(|e| input(format!("{k}: {e}")))?;
                    self.update(id, value)?;
                }
                _ => {
                    kind_patch.insert(k.clone(), v.clone());
                }
            }
        }
        if !kind_patch.is_empty() {
            let kind = &self.doc.node(key).map(|n| n.kind.clone()).ok_or(Error::Doc(DocError::UnknownNode(id)))?;
            let ty = kind.type_name();
            let allowed = kind_fields().get(ty).cloned().unwrap_or_default();
            if let Some(bad) = kind_patch.keys().find(|k| !allowed.contains(*k)) {
                return Err(input(format!("unknown property {bad:?} for a {ty} node")));
            }
            let mut v = serde_json::to_value(kind).map_err(|e| input(e.to_string()))?;
            if let Some(obj) = v.as_object_mut() {
                obj.extend(kind_patch);
            }
            let kind: NodeKind = serde_json::from_value(v).map_err(|e| input(e.to_string()))?;
            self.update(id, PropValue::Kind(kind))?;
        }
        Ok(())
    }

    pub fn delete(&mut self, id: NodeId) -> Result<(), Error> {
        Ok(self.apply(Op::Delete { id })?)
    }

    /// Moves `id` to `index` (`None` = last) under `parent`.
    pub fn move_node(&mut self, id: NodeId, parent: Option<NodeId>, index: Option<usize>) -> Result<(), Error> {
        let index = match index {
            Some(i) => i,
            None => {
                let n = self.child_count(parent)?;
                let same = self.doc.get(id).map(|n| n.parent) == Some(parent.and_then(|p| self.doc.key(p)));
                n - usize::from(same && n > 0)
            }
        };
        Ok(self.apply(Op::Move { id, parent, index })?)
    }

    /// Replaces node `id` with `node` (JSON) at the same position. Returns the new node's id.
    pub fn replace_json(&mut self, id: NodeId, node: Value) -> Result<NodeId, Error> {
        let key = self.key_of(id)?;
        let parent = self.doc.node(key).and_then(|n| n.parent).and_then(|p| self.doc.node(p)).map(|p| p.id);
        let index = self.doc.index_of(key);
        self.delete(id)?;
        self.insert_json(parent, index, node)
    }

    /// Deep-copies `id` (new ids throughout) to `parent` at `index`, then applies `overrides` to
    /// the copy's root. Returns the copy's id.
    pub fn copy(&mut self, id: NodeId, parent: Option<NodeId>, index: Option<usize>, overrides: Option<&Map<String, Value>>) -> Result<NodeId, Error> {
        let key = self.key_of(id)?;
        let mut tree = self.doc.to_tree(key).ok_or(Error::Doc(DocError::UnknownNode(id)))?;
        remap_ids(std::slice::from_mut(&mut tree), || self.new_id());
        let new_id = self.insert(parent, index, tree)?;
        if let Some(o) = overrides {
            self.update_json(new_id, o)?;
        }
        Ok(new_id)
    }

    pub fn get(&self, id: NodeId) -> Option<NodeTree> {
        self.doc.key(id).and_then(|k| self.doc.to_tree(k))
    }

    /// Ids of nodes matching `pred`, in document order.
    pub fn find(&self, mut pred: impl FnMut(&Node) -> bool) -> Vec<NodeId> {
        let mut out = Vec::new();
        for &p in self.doc.pages() {
            for k in self.doc.descendants(p) {
                if let Some(n) = self.doc.node(k)
                    && pred(n) {
                        out.push(n.id);
                    }
            }
        }
        out
    }

    pub fn set_variables(&mut self, variables: Variables) -> Result<(), Error> {
        Ok(self.apply(Op::SetVariables { variables })?)
    }

    pub fn put_asset(&mut self, id: AssetId, meta: AssetMeta) -> Result<(), Error> {
        Ok(self.apply(Op::PutAsset { id, meta: Some(meta) })?)
    }

    pub fn remove_asset(&mut self, id: AssetId) -> Result<(), Error> {
        Ok(self.apply(Op::PutAsset { id, meta: None })?)
    }
}

/// A node with default common properties, for building trees in code.
pub fn node(id: NodeId, kind: NodeKind, props: CommonProps) -> NodeTree {
    NodeTree { id, kind, name: String::new(), props, children: Vec::new() }
}
