//! Primitive operations. Every mutation of a [`crate::Document`] is one of these; each one, when
//! applied, yields its inverse. Ops address nodes by stable id, so they can be logged, replayed
//! and later mapped onto a CRDT.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::id::NodeId;
use crate::model::{AssetId, AssetMeta};
use crate::node::NodeTree;
use crate::props::{PropKey, PropValue};
use crate::variables::Variables;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum Op {
    /// Insert `node` (with its subtree) as child `index` of `parent` (`None` = top level, pages only).
    Insert { parent: Option<NodeId>, index: usize, node: Box<NodeTree> },
    /// Delete a node and its subtree.
    Delete { id: NodeId },
    /// Replace one property.
    Update {
        id: NodeId,
        #[serde(flatten)]
        value: PropValue,
    },
    /// Move a node to child `index` of `parent`; `index` counts siblings after the node is removed
    /// from its old place. Reordering is a move within the same parent.
    Move { id: NodeId, parent: Option<NodeId>, index: usize },
    /// Replace all variable collections.
    SetVariables { variables: Variables },
    /// Add, replace (`Some`) or remove (`None`) an asset's metadata.
    PutAsset { id: AssetId, meta: Option<AssetMeta> },
}

/// What one op changed; collected into a [`ChangeSet`] per transaction.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    Inserted(NodeId),
    Deleted(NodeId),
    Updated(NodeId, PropKey),
    Moved(NodeId),
    Variables,
    Asset(AssetId),
}

/// Everything a transaction changed, for downstream invalidation (layout, index, render) and
/// change notifications. Entries are in op order and may repeat.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ChangeSet {
    pub inserted: Vec<NodeId>,
    pub deleted: Vec<NodeId>,
    pub updated: Vec<(NodeId, PropKey)>,
    pub moved: Vec<NodeId>,
    pub variables: bool,
    pub assets: Vec<AssetId>,
}

impl ChangeSet {
    pub fn push(&mut self, c: Change) {
        match c {
            Change::Inserted(id) => self.inserted.push(id),
            Change::Deleted(id) => self.deleted.push(id),
            Change::Updated(id, k) => self.updated.push((id, k)),
            Change::Moved(id) => self.moved.push(id),
            Change::Variables => self.variables = true,
            Change::Asset(a) => self.assets.push(a),
        }
    }

    /// Appends `other`'s changes.
    pub fn extend(&mut self, other: &ChangeSet) {
        self.inserted.extend_from_slice(&other.inserted);
        self.deleted.extend_from_slice(&other.deleted);
        self.updated.extend_from_slice(&other.updated);
        self.moved.extend_from_slice(&other.moved);
        self.variables |= other.variables;
        self.assets.extend_from_slice(&other.assets);
    }

    pub fn is_empty(&self) -> bool {
        self.inserted.is_empty()
            && self.deleted.is_empty()
            && self.updated.is_empty()
            && self.moved.is_empty()
            && !self.variables
            && self.assets.is_empty()
    }
}
