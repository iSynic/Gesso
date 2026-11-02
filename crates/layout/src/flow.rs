//! Auto-layout with Taffy. Each auto-layout root (a container with auto layout whose parent has
//! none) gets a mirror tree of its participating nodes. Property edits restyle single mirror
//! nodes and let Taffy's cache redo only the affected path; structural edits rebuild one root's
//! mirror. Mirrors are built lazily, on the first edit that touches them.

use doc::model::{AutoLayout, Sizing};
use doc::{ChangeSet, Document, NodeId, NodeKey, NodeKind, PropKey};
use rustc_hash::{FxHashMap, FxHashSet};
use taffy::prelude::{AvailableSpace, Size, TaffyTree};
use taffy::{LayoutInput, LayoutOutput, compute_leaf_layout};
use text::{TextEngine, TextInput};

use crate::semantics::{self, auto_layout};
use crate::{LayoutEngine, Write, same};

/// Measure context of a text leaf. Taffy measures a leaf several times per layout (min, max
/// and final width); `memo` answers repeats without touching the document or hashing the text.
#[derive(Debug, Clone)]
struct TextLeaf {
    id: NodeId,
    wrap: bool,
    /// `(wrap width bits or u32::MAX, [width, height, first baseline])`; cleared on text edits.
    memo: Vec<(u32, [f32; 3])>,
}

struct Entry {
    id: NodeId,
    tid: taffy::NodeId,
    /// Index of the parent entry; `u32::MAX` for the root.
    parent: u32,
    absolute: bool,
    container: bool,
    hidden: bool,
    /// One past the last entry of this node's subtree (entries are in depth-first order).
    end: u32,
    /// Rect `[x, y, w, h]` as last computed (or as read from the document when built).
    last: [f32; 4],
}

struct Mirror {
    tree: TaffyTree<TextLeaf>,
    /// Entries in depth-first order; index 0 is the root.
    nodes: Vec<Entry>,
    index: FxHashMap<NodeId, u32>,
}

#[derive(Default)]
pub struct TaffyLayout {
    mirrors: FxHashMap<NodeId, Mirror>,
    /// Participating node → its root.
    owner: FxHashMap<NodeId, NodeId>,
    rebuild: FxHashSet<NodeId>,
    dirty: FxHashSet<NodeId>,
    /// Nodes whose computed props were edited directly; their result is re-emitted.
    checks: FxHashSet<NodeId>,
}

fn is_al(doc: &Document, key: NodeKey) -> bool {
    doc.node(key).is_some_and(|n| auto_layout(&n.kind).is_some())
}

fn parent_al(doc: &Document, key: NodeKey) -> Option<NodeKey> {
    doc.node(key).and_then(|n| n.parent).filter(|p| is_al(doc, *p))
}

/// The auto-layout root that `key` participates in, if any.
pub(crate) fn root_of(doc: &Document, key: NodeKey) -> Option<NodeKey> {
    let mut c = if is_al(doc, key) { key } else { parent_al(doc, key)? };
    while let Some(p) = parent_al(doc, c) {
        c = p;
    }
    Some(c)
}

fn rect(doc: &Document, key: NodeKey) -> [f32; 4] {
    doc.node(key).map_or([0.0; 4], |n| [n.props.x as f32, n.props.y as f32, n.props.width as f32, n.props.height as f32])
}

impl TaffyLayout {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn mirror_count(&self) -> usize {
        self.mirrors.len()
    }

    /// Number of nodes in the mirror of `root`, if it is built.
    pub fn mirror_len(&self, root: NodeId) -> Option<usize> {
        self.mirrors.get(&root).map(|m| m.nodes.len())
    }

    fn mark_root(&mut self, doc: &Document, key: NodeKey) {
        if let Some(r) = root_of(doc, key).and_then(|r| doc.node(r)) {
            self.rebuild.insert(r.id);
        }
    }

    /// Drops a mirror; returns the auto-layout containers it held besides the root.
    fn remove_mirror(&mut self, root: NodeId) -> Vec<NodeId> {
        let mut containers = Vec::new();
        if let Some(m) = self.mirrors.remove(&root) {
            for e in &m.nodes {
                if self.owner.get(&e.id) == Some(&root) {
                    self.owner.remove(&e.id);
                }
                if e.container && e.id != root {
                    containers.push(e.id);
                }
            }
        }
        self.dirty.remove(&root);
        containers
    }

    fn build(&mut self, doc: &Document, root: NodeId) {
        let nested = self.remove_mirror(root);
        self.build_mirror(doc, root);
        // Containers that were nested in the old mirror may be roots now (e.g. the root lost
        // its auto layout).
        for id in nested {
            if let Some(k) = doc.key(id)
                && !self.mirrors.contains_key(&id)
                && is_al(doc, k)
                && parent_al(doc, k).is_none()
            {
                self.build(doc, id);
            }
        }
    }

    fn build_mirror(&mut self, doc: &Document, root: NodeId) {
        let Some(rkey) = doc.key(root) else { return };
        if !is_al(doc, rkey) || parent_al(doc, rkey).is_some() {
            return;
        }
        let mut m = Mirror { tree: TaffyTree::new(), nodes: Vec::new(), index: FxHashMap::default() };
        m.tree.disable_rounding();
        self.add(doc, &mut m, root, rkey, None, u32::MAX);
        self.mirrors.insert(root, m);
        self.dirty.insert(root);
    }

    /// Adds `key` and its participating descendants to `m`; returns its Taffy id.
    fn add(&mut self, doc: &Document, m: &mut Mirror, root: NodeId, key: NodeKey, parent: Option<&AutoLayout>, pidx: u32) -> Option<taffy::NodeId> {
        let node = doc.node(key)?;
        if node.id != root && self.mirrors.contains_key(&node.id) {
            // A former root, now nested in this one.
            let _ = self.remove_mirror(node.id);
        }
        let style = semantics::style(node, parent);
        let tid = match &node.kind {
            NodeKind::Text { .. } => m.tree.new_leaf_with_context(style, TextLeaf { id: node.id, wrap: semantics::text_wraps(node), memo: Vec::new() }).ok()?,
            _ => m.tree.new_leaf(style).ok()?,
        };
        let idx = m.nodes.len() as u32;
        let al = auto_layout(&node.kind);
        let absolute = parent.is_some() && node.props.layout_child.absolute;
        m.nodes.push(Entry { id: node.id, tid, parent: pidx, absolute, container: al.is_some(), hidden: !node.props.visible, end: idx + 1, last: rect(doc, key) });
        m.index.insert(node.id, idx);
        self.owner.insert(node.id, root);
        if let Some(al) = al {
            let kids: Vec<taffy::NodeId> = node.children.iter().filter_map(|c| self.add(doc, m, root, *c, Some(al), idx)).collect();
            m.tree.set_children(tid, &kids).ok()?;
        }
        m.nodes[idx as usize].end = m.nodes.len() as u32;
        Some(tid)
    }

    /// Restyles one participating node; returns false if its role changed and the mirror must
    /// be rebuilt.
    fn restyle(&mut self, doc: &Document, root: NodeId, id: NodeId, key: PropKey) -> bool {
        let Some(m) = self.mirrors.get_mut(&root) else { return false };
        let (Some(&idx), Some(nkey)) = (m.index.get(&id), doc.key(id)) else { return false };
        let Some(node) = doc.node(nkey) else { return false };
        let parent = parent_al(doc, nkey).and_then(|p| doc.node(p)).and_then(|p| auto_layout(&p.kind));
        let e = &m.nodes[idx as usize];
        if e.container != auto_layout(&node.kind).is_some() || (parent.is_some() && node.props.layout_child.absolute) != e.absolute {
            return false;
        }
        let tid = e.tid;
        if e.hidden && node.props.visible {
            // Shown again: the subtree may have been edited while hidden, so compare everything
            // in it with the document at the next compute.
            for i in idx..e.end {
                m.nodes[i as usize].last = [f32::NAN; 4];
            }
        }
        m.nodes[idx as usize].hidden = !node.props.visible;
        let style = semantics::style(node, parent);
        let changed = m.tree.style(tid).map_or(true, |s| *s != style);
        if changed {
            let _ = m.tree.set_style(tid, style);
        }
        // Sizing decides whether text wraps.
        if let NodeKind::Text { .. } = node.kind
            && let Some(c) = m.tree.get_node_context_mut(tid)
        {
            let wrap = semantics::text_wraps(node);
            if c.wrap != wrap || key == PropKey::Kind {
                c.wrap = wrap;
                c.memo.clear();
                let _ = m.tree.mark_dirty(tid);
            }
        }
        if key == PropKey::Kind {
            match &node.kind {
                NodeKind::Text { .. } => {}
                // Direction changes how children read fill and alignment.
                _ if auto_layout(&node.kind).is_some() => {
                    for c in &node.children {
                        if let Some(ch) = doc.node(*c)
                            && let Some(&ci) = m.index.get(&ch.id)
                        {
                            let st = semantics::style(ch, auto_layout(&node.kind));
                            let _ = m.tree.set_style(m.nodes[ci as usize].tid, st);
                        }
                    }
                }
                _ => {}
            }
        } else if !changed && matches!(key, PropKey::X | PropKey::Y | PropKey::Width | PropKey::Height) {
            self.checks.insert(id);
        }
        self.dirty.insert(root);
        true
    }

    fn structural(&mut self, doc: &Document, id: NodeId) {
        if let Some(r) = self.owner.get(&id).copied() {
            self.rebuild.insert(r);
        }
        if let Some(k) = doc.key(id) {
            self.mark_root(doc, k);
        }
    }

    /// Size and first baseline of a text leaf under the given constraints.
    fn measure(doc: &Document, text: &mut TextEngine, leaf: &mut TextLeaf, known: Size<Option<f32>>, avail: Size<AvailableSpace>) -> (Size<f32>, f32) {
        let wrap_at = match known.width {
            Some(w) => Some(w),
            None => match avail.width {
                AvailableSpace::Definite(w) => Some(w),
                AvailableSpace::MinContent => Some(0.0),
                AvailableSpace::MaxContent => None,
            },
        };
        let width = if leaf.wrap { wrap_at } else { None };
        let key = width.map_or(u32::MAX, f32::to_bits);
        let [w, h, b] = match leaf.memo.iter().find(|(k, _)| *k == key) {
            Some((_, v)) => *v,
            None => {
                let Some(ti) = doc.get(leaf.id).and_then(|n| TextInput::from_kind(&n.kind)) else { return (Size::ZERO, 0.0) };
                let t = text.measure(ti, width);
                let v = [t.width, t.height, t.first_baseline];
                if leaf.memo.len() >= 8 {
                    leaf.memo.clear();
                }
                leaf.memo.push((key, v));
                v
            }
        };
        (Size { width: known.width.unwrap_or(w), height: known.height.unwrap_or(h) }, b)
    }

    fn leaf(doc: &Document, text: &mut TextEngine, input: LayoutInput, ctx: Option<&mut TextLeaf>, style: &taffy::Style) -> LayoutOutput {
        let Some(leaf) = ctx else { return compute_leaf_layout(input, style, |_, _| 0.0, |_, _| Size::ZERO) };
        let mut baseline = None;
        let mut out = compute_leaf_layout(input, style, |_, _| 0.0, |known, avail| {
            let (size, b) = Self::measure(doc, text, leaf, known, avail);
            baseline = Some(b);
            size
        });
        if out.baselines.first.is_none() {
            out.baselines.first = baseline;
        }
        out
    }
}

impl LayoutEngine for TaffyLayout {
    fn reset(&mut self) {
        *self = Self::default();
    }

    fn invalidate(&mut self, doc: &Document, changes: &ChangeSet, old_parents: &[NodeId]) {
        for id in changes.inserted.iter().chain(&changes.moved) {
            self.structural(doc, *id);
            // Auto-layout containers anywhere in an inserted or moved subtree may be new roots.
            if let Some(k) = doc.key(*id) {
                for d in doc.descendants(k) {
                    if is_al(doc, d) && parent_al(doc, d).is_none() {
                        self.mark_root(doc, d);
                    }
                }
            }
        }
        for id in &changes.deleted {
            self.structural(doc, *id);
        }
        for p in old_parents {
            if let Some(k) = doc.key(*p) {
                self.mark_root(doc, k);
            }
        }
        if !changes.deleted.is_empty() {
            let gone: Vec<NodeId> = self.mirrors.keys().filter(|r| doc.get(**r).is_none()).copied().collect();
            for r in gone {
                let _ = self.remove_mirror(r);
            }
        }
        for (id, key) in &changes.updated {
            let relevant = matches!(
                key,
                PropKey::Kind
                    | PropKey::X
                    | PropKey::Y
                    | PropKey::Width
                    | PropKey::Height
                    | PropKey::Visible
                    | PropKey::SizingH
                    | PropKey::SizingV
                    | PropKey::MinWidth
                    | PropKey::MaxWidth
                    | PropKey::MinHeight
                    | PropKey::MaxHeight
                    | PropKey::LayoutChild
            );
            if !relevant {
                continue;
            }
            match self.owner.get(id).copied() {
                Some(r) if self.mirrors.contains_key(&r) => {
                    if !self.rebuild.contains(&r) && !self.restyle(doc, r, *id, *key) {
                        self.rebuild.insert(r);
                    }
                }
                // Not in a built mirror: build the one it belongs to, if any.
                _ => {
                    if let Some(k) = doc.key(*id) {
                        self.mark_root(doc, k);
                    }
                }
            }
        }
        let roots: Vec<NodeId> = self.rebuild.drain().collect();
        for r in roots {
            self.build(doc, r);
        }
    }

    fn invalidate_all(&mut self, doc: &Document) {
        self.reset();
        let roots: Vec<NodeId> = doc.iter().filter(|(k, _)| is_al(doc, *k) && parent_al(doc, *k).is_none()).map(|(_, n)| n.id).collect();
        for r in roots {
            self.build(doc, r);
        }
    }

    fn compute(&mut self, doc: &Document, text: &mut TextEngine, out: &mut Vec<Write>) {
        let dirty: Vec<NodeId> = self.dirty.drain().collect();
        for root in dirty {
            let Some(m) = self.mirrors.get_mut(&root) else { continue };
            let Some(rnode) = doc.get(root) else { continue };
            if !rnode.props.visible {
                continue;
            }
            let (h, v) = semantics::sizing(rnode);
            let space = |s: Sizing, value: f64| if s == Sizing::Hug { AvailableSpace::MaxContent } else { AvailableSpace::Definite(value as f32) };
            let avail = Size { width: space(h, rnode.props.width), height: space(v, rnode.props.height) };
            let rtid = m.nodes[0].tid;
            let res = m.tree.compute_layout_with_measure(rtid, avail, |input, _tid, ctx, style| Self::leaf(doc, text, input, ctx, style));
            if res.is_err() {
                continue;
            }
            let mut hidden = vec![false; m.nodes.len()];
            for i in 0..m.nodes.len() {
                let e = &m.nodes[i];
                if e.hidden || (e.parent != u32::MAX && hidden[e.parent as usize]) {
                    hidden[i] = true;
                    continue;
                }
                let l = m.tree.unrounded_layout(e.tid);
                let r = [l.location.x, l.location.y, l.size.width, l.size.height];
                let changed = r.iter().zip(&e.last).any(|(a, b)| !same(*a as f64, *b as f64));
                if !changed && !self.checks.contains(&e.id) {
                    continue;
                }
                let Some(node) = doc.get(e.id) else { continue };
                let p = &node.props;
                let positioned = i != 0 && !e.absolute;
                let props = [(PropKey::X, p.x, positioned), (PropKey::Y, p.y, positioned), (PropKey::Width, p.width, true), (PropKey::Height, p.height, true)];
                for ((key, cur, write), new) in props.into_iter().zip(r) {
                    if write && !same(cur, new as f64) {
                        out.push(Write::new(e.id, key, new as f64));
                    }
                }
                m.nodes[i].last = r;
            }
        }
        self.checks.clear();
    }
}
