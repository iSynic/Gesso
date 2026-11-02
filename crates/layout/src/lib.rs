//! Layout: auto layout (Taffy), text auto-resize, constraints and group bounds. Results are
//! written back into the document as `x`/`y`/`width`/`height` by [`LayoutDeriver`], inside the
//! transaction that caused them, so they share its undo step.

pub mod bindings;
mod constraints;
mod deriver;
mod flow;
pub mod instances;
pub mod semantics;

pub use constraints::{constrain, union};
pub use deriver::{LayoutDeriver, LayoutStats};
pub use flow::TaffyLayout;

use doc::{ChangeSet, Document, NodeId, PropKey, PropValue};
use text::TextEngine;

/// One computed property to write back.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Write {
    pub id: NodeId,
    pub key: PropKey,
    pub value: f64,
}

impl Write {
    /// Values are written rounded to 1/100 px so files stay readable.
    pub fn new(id: NodeId, key: PropKey, value: f64) -> Self {
        Write { id, key, value: (value * 100.0).round() / 100.0 }
    }

    pub fn prop(&self) -> PropValue {
        match self.key {
            PropKey::X => PropValue::X(self.value),
            PropKey::Y => PropValue::Y(self.value),
            PropKey::Width => PropValue::Width(self.value),
            _ => PropValue::Height(self.value),
        }
    }
}

/// Equal within one step of the write precision. Half a step isn't enough: the same layout
/// computed in a different order (incremental vs full) can land either side of a rounding
/// boundary, and it would then flip on every relayout.
pub(crate) fn same(a: f64, b: f64) -> bool {
    (a - b).abs() < 0.0101
}

/// The auto-layout engine behind the deriver.
pub trait LayoutEngine {
    fn reset(&mut self);
    /// Marks what `changes` affect. `old_parents` are the former parents of moved and deleted
    /// nodes.
    fn invalidate(&mut self, doc: &Document, changes: &ChangeSet, old_parents: &[NodeId]);
    /// Marks everything for a full layout.
    fn invalidate_all(&mut self, doc: &Document);
    /// Lays out what is marked and appends the props that differ from the document.
    fn compute(&mut self, doc: &Document, text: &mut TextEngine, out: &mut Vec<Write>);
}
