//! The layout deriver: runs at the end of every transaction, lays out what the edits affected
//! and writes the results back as ops in the same transaction. Writes can trigger more layout
//! (a hugging frame grows, so its constrained children move), so it runs in passes until
//! nothing changes. Values are compared before writing, so a settled document produces no ops.

use doc::model::Sizing;
use doc::{ChangeSet, Deriver, Document, Error, NodeId, NodeKey, NodeKind, Op, PropKey, PropValue, Tx, VirtualSpec};
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use text::{TextEngine, TextInput};

use crate::constraints::{constrain, union};
use crate::flow::TaffyLayout;
use crate::{bindings, instances};
use crate::semantics::{self, auto_layout};
use crate::{LayoutEngine, Write, same};

/// Passes per transaction before giving up on convergence. Real documents settle in 2–4.
const MAX_PASSES: u32 = 32;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LayoutStats {
    /// Passes run by the most recent derive.
    pub last_passes: u32,
    /// Props written by the most recent derive.
    pub last_writes: usize,
    /// Derives that hit [`MAX_PASSES`].
    pub unconverged: u64,
    /// Instances whose content the most recent derive rebuilt.
    pub last_expanded: usize,
}

/// Where layout results go: a transaction (real nodes get ops), or straight into the document
/// after undo and redo, when only derived state (instance content) may change.
trait Sink {
    fn doc(&self) -> &Document;
    fn expansion(&mut self, instance: NodeKey, specs: Vec<VirtualSpec>) -> ChangeSet;
    /// Applies one write; false if it was dropped.
    fn write(&mut self, w: &Write) -> Result<bool, Error>;
    /// Updates a real node (bound values); false if it was dropped.
    fn update(&mut self, id: NodeId, value: PropValue) -> Result<bool, Error>;
}

impl Sink for Tx<'_> {
    fn doc(&self) -> &Document {
        Tx::doc(self)
    }
    fn expansion(&mut self, instance: NodeKey, specs: Vec<VirtualSpec>) -> ChangeSet {
        self.set_expansion(instance, specs)
    }
    fn write(&mut self, w: &Write) -> Result<bool, Error> {
        if !self.set_virtual(w.id, w.prop()) {
            self.apply(Op::Update { id: w.id, value: w.prop() })?;
        }
        Ok(true)
    }
    fn update(&mut self, id: NodeId, value: PropValue) -> Result<bool, Error> {
        self.apply(Op::Update { id, value })?;
        Ok(true)
    }
}

/// Derived-only sink: collects what changed for the caller.
struct Raw<'d> {
    doc: &'d mut Document,
    changes: ChangeSet,
}

impl Sink for Raw<'_> {
    fn doc(&self) -> &Document {
        self.doc
    }
    fn expansion(&mut self, instance: NodeKey, specs: Vec<VirtualSpec>) -> ChangeSet {
        let mut c = ChangeSet::default();
        self.doc.set_expansion(instance, specs, &mut c);
        self.changes.extend(&c);
        c
    }
    fn write(&mut self, w: &Write) -> Result<bool, Error> {
        // Real nodes already hold their laid-out values; only instance content is derived here.
        Ok(match self.doc.set_virtual(w.id, w.prop()) {
            Some(c) => {
                self.changes.push(c);
                true
            }
            None => false,
        })
    }
    fn update(&mut self, _: NodeId, _: PropValue) -> Result<bool, Error> {
        // Undo and redo restore bound values with everything else.
        Ok(false)
    }
}

pub struct LayoutDeriver<E: LayoutEngine = TaffyLayout> {
    text: TextEngine,
    flow: E,
    /// Child → parent, to find the former parent of moved and deleted nodes.
    parent: FxHashMap<NodeId, NodeId>,
    /// Last known size of every container, for constraints.
    sizes: FxHashMap<NodeId, (f64, f64)>,
    stats: LayoutStats,
}

impl LayoutDeriver<TaffyLayout> {
    pub fn new(text: TextEngine) -> Self {
        Self::with_engine(text, TaffyLayout::new())
    }
}

fn is_text(doc: &Document, key: NodeKey) -> bool {
    doc.node(key).is_some_and(|n| matches!(n.kind, NodeKind::Text { .. }))
}

fn in_auto_layout(doc: &Document, key: NodeKey) -> bool {
    doc.node(key).and_then(|n| n.parent).and_then(|p| doc.node(p)).is_some_and(|p| auto_layout(&p.kind).is_some())
}

fn is_group(doc: &Document, id: NodeId) -> bool {
    doc.get(id).is_some_and(|n| matches!(n.kind, NodeKind::Group {}))
}

impl<E: LayoutEngine> LayoutDeriver<E> {
    pub fn with_engine(text: TextEngine, flow: E) -> Self {
        LayoutDeriver { text, flow, parent: FxHashMap::default(), sizes: FxHashMap::default(), stats: LayoutStats::default() }
    }

    pub fn text(&mut self) -> &mut TextEngine {
        &mut self.text
    }

    pub fn engine(&mut self) -> &mut E {
        &mut self.flow
    }

    pub fn stats(&self) -> LayoutStats {
        self.stats
    }

    fn index_node(&mut self, doc: &Document, key: NodeKey) {
        let Some(n) = doc.node(key) else { return };
        if let Some(p) = n.parent.and_then(|p| doc.node(p)) {
            self.parent.insert(n.id, p.id);
        }
        if n.kind.is_container() {
            self.sizes.insert(n.id, (n.props.width, n.props.height));
        }
    }

    fn index(&mut self, doc: &Document) {
        self.parent.clear();
        self.sizes.clear();
        let keys: Vec<NodeKey> = doc.iter().map(|(k, _)| k).collect();
        for k in keys {
            self.index_node(doc, k);
        }
    }

    /// Warms everything up after opening a document: shapes all text in parallel and lays out
    /// every auto-layout root, without writing. Returns how many stored values disagree with
    /// the computed layout (0 for a document saved by this engine).
    pub fn prepare(&mut self, doc: &Document) -> usize {
        self.index(doc);
        let nodes: Vec<&doc::Node> = doc.iter().map(|(_, n)| n).filter(|n| matches!(n.kind, NodeKind::Text { .. })).collect();
        // Wrapping text at its stored width (what the final layout asks for) and at min-content
        // (what intrinsic sizing asks for); hugging text unwrapped. Anything else Taffy asks for
        // is shaped on demand.
        let mut inputs: Vec<(TextInput<'_>, Option<f32>)> = Vec::with_capacity(nodes.len() * 2);
        for n in &nodes {
            if let Some(t) = TextInput::from_kind(&n.kind) {
                if semantics::text_wraps(n) {
                    inputs.push((t, Some(n.props.width as f32)));
                    inputs.push((t, Some(0.0)));
                } else {
                    inputs.push((t, None));
                }
            }
        }
        self.text.measure_many(&inputs);
        self.flow.invalidate_all(doc);
        let mut out = Vec::new();
        self.flow.compute(doc, &mut self.text, &mut out);
        out.len()
    }

    /// Sizes text that is not under auto layout, per its auto-resize mode.
    fn fit_text(&mut self, doc: &Document, key: NodeKey, out: &mut Vec<Write>) {
        let Some(n) = doc.node(key) else { return };
        let Some(input) = TextInput::from_kind(&n.kind) else { return };
        if in_auto_layout(doc, key) {
            return;
        }
        let (h, v) = semantics::sizing(n);
        if h != Sizing::Hug && v != Sizing::Hug {
            return;
        }
        let t = self.text.measure(input, (h != Sizing::Hug).then_some(n.props.width as f32));
        if h == Sizing::Hug && !same(n.props.width, t.width as f64) {
            out.push(Write::new(n.id, PropKey::Width, t.width as f64));
        }
        if v == Sizing::Hug && !same(n.props.height, t.height as f64) {
            out.push(Write::new(n.id, PropKey::Height, t.height as f64));
        }
    }

    /// Fits a group to the union of its children, keeping the children where they are on the
    /// canvas.
    fn fit_group(doc: &Document, id: NodeId, out: &mut Vec<Write>) {
        let Some(g) = doc.get(id) else { return };
        let kids: Vec<&doc::Node> = g.children.iter().filter_map(|k| doc.node(*k)).collect();
        let Some([x0, y0, x1, y1]) = union(kids.iter().map(|c| [c.props.x, c.props.y, c.props.width, c.props.height])) else { return };
        let p = &g.props;
        if !same(x0, 0.0) || !same(y0, 0.0) {
            out.push(Write::new(id, PropKey::X, p.x + x0));
            out.push(Write::new(id, PropKey::Y, p.y + y0));
            for c in &kids {
                out.push(Write::new(c.id, PropKey::X, c.props.x - x0));
                out.push(Write::new(c.id, PropKey::Y, c.props.y - y0));
            }
        }
        if !same(p.width, x1 - x0) {
            out.push(Write::new(id, PropKey::Width, x1 - x0));
        }
        if !same(p.height, y1 - y0) {
            out.push(Write::new(id, PropKey::Height, y1 - y0));
        }
    }

    /// Repositions the children of a resized frame that doesn't use auto layout.
    fn reflow_constraints(&mut self, doc: &Document, id: NodeId, out: &mut Vec<Write>) {
        let Some(n) = doc.get(id) else { return };
        if !n.kind.is_container() {
            return;
        }
        let new = (n.props.width, n.props.height);
        let old = self.sizes.insert(id, new);
        let constrained = matches!(n.kind, NodeKind::Frame { .. } | NodeKind::Component { .. }) && auto_layout(&n.kind).is_none();
        let Some(old) = old else { return };
        if !constrained || (same(old.0, new.0) && same(old.1, new.1)) {
            return;
        }
        for c in n.children.iter().filter_map(|k| doc.node(*k)) {
            let p = &c.props;
            let cur = [p.x, p.y, p.width, p.height];
            let next = constrain(p.constraints, cur, old, new);
            for (key, (a, b)) in [PropKey::X, PropKey::Y, PropKey::Width, PropKey::Height].into_iter().zip(cur.into_iter().zip(next)) {
                if !same(a, b) {
                    out.push(Write::new(c.id, key, b));
                }
            }
        }
    }

    /// One layout pass over `changes` (`None` = everything). Returns the props to write.
    fn pass(&mut self, doc: &Document, changes: Option<&ChangeSet>) -> Vec<Write> {
        let mut out = Vec::new();
        let empty = ChangeSet::default();
        let ch = changes.unwrap_or(&empty);
        let old_parents: Vec<NodeId> = ch.deleted.iter().chain(&ch.moved).filter_map(|id| self.parent.get(id).copied()).collect();
        for id in &ch.deleted {
            self.parent.remove(id);
            self.sizes.remove(id);
        }
        let mut texts: FxHashSet<NodeKey> = FxHashSet::default();
        let mut groups: FxHashSet<NodeId> = old_parents.iter().copied().filter(|p| is_group(doc, *p)).collect();
        for id in ch.inserted.iter().chain(&ch.moved) {
            let Some(k) = doc.key(*id) else { continue };
            for d in std::iter::once(k).chain(doc.descendants(k)) {
                self.index_node(doc, d);
                if is_text(doc, d) {
                    texts.insert(d);
                }
                if doc.node(d).is_some_and(|n| matches!(n.kind, NodeKind::Group {})) {
                    groups.extend(doc.node(d).map(|n| n.id));
                }
            }
            if let Some(p) = self.parent.get(id).copied().filter(|p| is_group(doc, *p)) {
                groups.insert(p);
            }
        }

        match changes {
            Some(ch) => self.flow.invalidate(doc, ch, &old_parents),
            None => self.flow.invalidate_all(doc),
        }
        self.flow.compute(doc, &mut self.text, &mut out);

        let mut resized: Vec<NodeId> = Vec::new();
        for (id, key) in &ch.updated {
            let Some(k) = doc.key(*id) else { continue };
            match key {
                PropKey::Kind | PropKey::SizingH | PropKey::SizingV if is_text(doc, k) => {
                    texts.insert(k);
                }
                // Auto layout added or removed: text children change who sizes them.
                PropKey::Kind => {
                    if let Some(n) = doc.node(k) {
                        texts.extend(n.children.iter().copied().filter(|c| is_text(doc, *c)));
                    }
                }
                PropKey::X | PropKey::Y | PropKey::Width | PropKey::Height => {
                    if matches!(key, PropKey::Width | PropKey::Height) {
                        if is_text(doc, k) {
                            texts.insert(k);
                        }
                        // A group's size is always derived from its children.
                        if is_group(doc, *id) {
                            groups.insert(*id);
                        }
                        resized.push(*id);
                    }
                    if let Some(p) = self.parent.get(id).copied().filter(|p| is_group(doc, *p)) {
                        groups.insert(p);
                    }
                }
                _ => {}
            }
        }
        if changes.is_none() {
            for (k, n) in doc.iter() {
                if matches!(n.kind, NodeKind::Text { .. }) {
                    texts.insert(k);
                }
                if matches!(n.kind, NodeKind::Group {}) {
                    groups.insert(n.id);
                }
            }
        }
        for k in texts {
            self.fit_text(doc, k, &mut out);
        }
        for g in groups {
            Self::fit_group(doc, g, &mut out);
        }
        resized.sort_unstable();
        resized.dedup();
        for id in resized {
            self.reflow_constraints(doc, id, &mut out);
        }
        out
    }

    /// Rebuilds the content of the instances `changes` affect (`None`: all). Returns what that
    /// changed.
    fn expand(&mut self, s: &mut impl Sink, changes: Option<&ChangeSet>) -> ChangeSet {
        let dirty = instances::dirty(s.doc(), changes, &self.parent);
        self.stats.last_expanded += dirty.len();
        let mut out = ChangeSet::default();
        // Building content only reads the document, so many instances build in parallel; the
        // results are then diffed into the document in order.
        let built: Vec<(NodeKey, Vec<VirtualSpec>)> = if dirty.len() >= 16 {
            let doc = s.doc();
            dirty.par_iter().map(|&k| (k, instances::specs(doc, k))).collect()
        } else {
            dirty.iter().map(|&k| (k, instances::specs(s.doc(), k))).collect()
        };
        for (k, specs) in built {
            // Constraints inside the content are applied by the expansion, so the sizes it
            // sets are the baseline for later resizes.
            for sp in &specs {
                if sp.kind.is_container() {
                    self.sizes.insert(sp.id, (sp.props.width, sp.props.height));
                }
            }
            out.extend(&s.expansion(k, specs));
        }
        out
    }

    /// Writes bound values into real nodes whose variables, bindings or modes `changes` touched.
    fn bind(&mut self, s: &mut impl Sink, changes: Option<&ChangeSet>) -> Result<ChangeSet, Error> {
        let mut out = ChangeSet::default();
        for k in bindings::targets(s.doc(), changes) {
            let Some(id) = s.doc().node(k).map(|n| n.id) else { continue };
            for v in bindings::updates(s.doc(), k) {
                let key = v.key();
                if s.update(id, v)? {
                    out.updated.push((id, key));
                }
            }
        }
        Ok(out)
    }

    fn run(&mut self, s: &mut impl Sink, first: Option<&ChangeSet>) -> Result<(), Error> {
        self.stats = LayoutStats { unconverged: self.stats.unconverged, ..Default::default() };
        let bound = self.bind(s, first)?;
        let merged = match first {
            Some(c) if !bound.is_empty() => {
                let mut c = c.clone();
                c.extend(&bound);
                Some(c)
            }
            _ => None,
        };
        let first = merged.as_ref().or(first);
        let expanded = self.expand(s, first);
        match first {
            Some(c) if !expanded.is_empty() => {
                let mut c = c.clone();
                c.extend(&expanded);
                self.settle(s, Some(&c))
            }
            _ => self.settle(s, first),
        }
    }

    /// Layout passes until nothing changes, rebuilding instance content that the results
    /// affect (e.g. a component that auto layout resized).
    fn settle(&mut self, s: &mut impl Sink, first: Option<&ChangeSet>) -> Result<(), Error> {
        let mut writes = self.pass(s.doc(), first);
        self.stats.last_passes = 1;
        while !writes.is_empty() {
            if self.stats.last_passes >= MAX_PASSES {
                self.stats.unconverged += 1;
                break;
            }
            let mut pending = ChangeSet::default();
            for w in &writes {
                if s.write(w)? {
                    pending.updated.push((w.id, w.key));
                }
            }
            self.stats.last_writes += writes.len();
            let expanded = self.expand(s, Some(&pending));
            pending.extend(&expanded);
            writes = if pending.is_empty() { Vec::new() } else { self.pass(s.doc(), Some(&pending)) };
            self.stats.last_passes += 1;
        }
        Ok(())
    }
}

impl<E: LayoutEngine + 'static> Deriver for LayoutDeriver<E> {
    fn attach(&mut self, doc: &mut Document) {
        self.flow.reset();
        self.index(doc);
        let mut raw = Raw { doc, changes: ChangeSet::default() };
        let expanded = self.expand(&mut raw, None);
        let _ = self.settle(&mut raw, Some(&expanded));
    }

    fn derive(&mut self, tx: &mut Tx<'_>, changes: &ChangeSet) -> Result<(), Error> {
        self.text.next_generation(256);
        self.run(tx, Some(changes))
    }

    fn derive_all(&mut self, tx: &mut Tx<'_>) -> Result<(), Error> {
        self.index(tx.doc());
        self.run(tx, None)
    }

    fn observe(&mut self, doc: &mut Document, changes: &ChangeSet) -> ChangeSet {
        // Undo and redo restore laid-out values exactly; this resyncs the caches and rebuilds
        // the instance content the restored values affect.
        let mut raw = Raw { doc, changes: ChangeSet::default() };
        let _ = self.run(&mut raw, Some(changes));
        raw.changes
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
