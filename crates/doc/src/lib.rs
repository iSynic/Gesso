//! Core document model: nodes, properties, variables, primitive ops, transactions and
//! undo/redo. No UI, layout, text or rendering types live here.

pub mod bind;
pub mod commands;
pub mod components;
pub mod document;
mod finite;
pub mod id;
pub mod model;
pub mod node;
pub mod op;
pub mod props;
pub mod store;
pub mod richtext;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod variables;

pub use document::{DocError, Document, DocumentData, Node, NodeKey, Virtual, VirtualSpec};
pub use id::{Id, IdGen, NodeId};
pub use node::{NodeKind, NodeTree, Override};
pub use op::{Change, ChangeSet, Op};
pub use props::{CommonProps, PropKey, PropValue};
pub use store::{Commit, Deriver, Error, Origin, Store, Tx, TxMeta};
pub use variables::Variables;
