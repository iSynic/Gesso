//! The layers-tree model: the flattened rows a layers panel shows for one page, topmost layer
//! first, with collapsed subtrees left out. Rebuilt lazily, and only after a change to the
//! tree's structure or to a name; property edits (drags, panel edits) never touch it.

use doc::{ChangeSet, Document, NodeId, NodeKey, PropKey};
use rustc_hash::{FxHashMap, FxHashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub id: NodeId,
    /// 0 for the page's children.
    pub depth: u16,
    pub has_children: bool,
    pub expanded: bool,
}

#[derive(Debug, Default)]
pub struct Layers {
    page: Option<NodeId>,
    expanded: FxHashSet<NodeId>,
    rows: Vec<Row>,
    row_of: FxHashMap<NodeId, usize>,
    stale: bool,
}

impl Layers {
    pub fn new(page: NodeId) -> Self {
        Layers { page: Some(page), stale: true, ..Default::default() }
    }

    pub fn set_page(&mut self, page: NodeId) {
        self.page = Some(page);
        self.stale = true;
    }

    /// Marks the rows stale if `changes` touched the tree's shape or a name.
    pub fn apply(&mut self, changes: &ChangeSet) {
        if !changes.inserted.is_empty() || !changes.deleted.is_empty() || !changes.moved.is_empty() || changes.updated.iter().any(|(_, k)| *k == PropKey::Name) {
            self.stale = true;
        }
    }

    pub fn is_expanded(&self, id: NodeId) -> bool {
        self.expanded.contains(&id)
    }

    pub fn set_expanded(&mut self, id: NodeId, on: bool) {
        let changed = if on { self.expanded.insert(id) } else { self.expanded.remove(&id) };
        self.stale |= changed;
    }

    /// Expands every ancestor of `id` so its row is shown.
    pub fn reveal(&mut self, doc: &Document, id: NodeId) {
        let Some(key) = doc.key(id) else { return };
        for a in doc.ancestors(key).filter_map(|k| doc.node(k)) {
            if Some(a.id) != self.page {
                self.stale |= self.expanded.insert(a.id);
            }
        }
    }

    /// The rows, rebuilt first if stale.
    pub fn rows(&mut self, doc: &Document) -> &[Row] {
        if self.stale {
            self.rebuild(doc);
        }
        &self.rows
    }

    /// The row index of `id`, if shown.
    pub fn row_of(&mut self, doc: &Document, id: NodeId) -> Option<usize> {
        if self.stale {
            self.rebuild(doc);
        }
        self.row_of.get(&id).copied()
    }

    fn rebuild(&mut self, doc: &Document) {
        self.rows.clear();
        self.row_of.clear();
        self.expanded.retain(|id| doc.get(*id).is_some());
        if let Some(page) = self.page.and_then(|p| doc.key(p)) {
            let mut stack: Vec<(NodeKey, u16)> = Vec::new();
            push_children(doc, page, 0, &mut stack);
            while let Some((k, depth)) = stack.pop() {
                let Some(n) = doc.node(k) else { continue };
                let expanded = self.expanded.contains(&n.id);
                self.row_of.insert(n.id, self.rows.len());
                self.rows.push(Row { id: n.id, depth, has_children: !n.children.is_empty(), expanded });
                if expanded {
                    push_children(doc, k, depth + 1, &mut stack);
                }
            }
        }
        self.stale = false;
    }
}

/// Pushes children so the last child (topmost) pops first.
fn push_children(doc: &Document, key: NodeKey, depth: u16, stack: &mut Vec<(NodeKey, u16)>) {
    if let Some(n) = doc.node(key) {
        stack.extend(n.children.iter().map(|&c| (c, depth)));
    }
}
