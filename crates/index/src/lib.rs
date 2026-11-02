//! Spatial index: world transforms and world-space visual bounds of every visible node, one
//! R-tree per page. Bounds include stroke, shadow and blur outsets and are clipped by ancestor
//! frames that clip their content, so culling and hit-testing see what is actually drawn.
//! Kept current from each transaction's [`ChangeSet`].

use doc::model::{Effect, StrokeAlign};
use doc::{ChangeSet, Document, Node, NodeId, NodeKey, NodeKind, PropKey};
use kurbo::{Affine, Rect, Vec2};
use rayon::prelude::*;
use rstar::primitives::{GeomWithData, Rectangle};
use rstar::{AABB, RStarInsertionStrategy, RTree, RTreeParams};
use rustc_hash::{FxHashMap, FxHashSet};

/// A node's transform relative to its parent: position, then rotation and flips about the
/// centre of its box. Rotation is in degrees, clockwise on screen (y down).
pub fn local_transform(n: &Node) -> Affine {
    let p = &n.props;
    let (cx, cy) = (p.width / 2.0, p.height / 2.0);
    let mut t = Affine::translate((p.x, p.y));
    if p.rotation != 0.0 || p.flip_x || p.flip_y {
        let flip = Affine::scale_non_uniform(if p.flip_x { -1.0 } else { 1.0 }, if p.flip_y { -1.0 } else { 1.0 });
        t = t * Affine::translate((cx, cy)) * Affine::rotate(p.rotation.to_radians()) * flip * Affine::translate((-cx, -cy));
    }
    t
}

/// Gaussian standard deviation for a design-tool blur value (shadow blur, blur radius): σ = v / 2,
/// the CSS convention. Content spreads about 3σ.
pub fn sigma(blur: f64) -> f64 {
    blur.max(0.0) / 2.0
}

/// How far a node's drawing can reach outside its box, in its local units:
/// `[left, top, right, bottom]`. Conservative (used for culling).
pub fn visual_outset(n: &Node) -> [f64; 4] {
    let p = &n.props;
    let mut o = 0.0f64;
    for s in p.strokes.iter().filter(|s| s.paint.visible) {
        // Miter joins can reach past the stroke width; allow twice the outward extent.
        let reach = match (&n.kind, s.align) {
            (NodeKind::Line {} | NodeKind::Path { .. }, _) | (_, StrokeAlign::Center) => s.weight,
            (_, StrokeAlign::Outside) => s.weight * 2.0,
            (_, StrokeAlign::Inside) => 0.0,
        };
        o = o.max(reach);
    }
    let mut out = [o; 4];
    for e in &p.effects {
        match e {
            Effect::DropShadow(s) if s.visible => {
                let r = 3.0 * sigma(s.blur) + s.spread.max(0.0);
                out[0] = out[0].max(r - s.offset[0]);
                out[1] = out[1].max(r - s.offset[1]);
                out[2] = out[2].max(r + s.offset[0]);
                out[3] = out[3].max(r + s.offset[1]);
            }
            Effect::LayerBlur { radius, visible: true } => out = out.map(|v| v + 3.0 * sigma(*radius)),
            _ => {}
        }
    }
    out
}

/// Clips children to the node's box.
pub fn clips(n: &Node) -> bool {
    matches!(n.kind, NodeKind::Frame { clip: true, .. })
}

/// World-space bounds of a local rectangle under `t` (the box of the rotated rectangle).
pub fn world_bounds(t: Affine, r: Rect) -> Rect {
    t.transform_rect_bbox(r)
}

fn own_bounds(n: &Node, world: Affine) -> Rect {
    let o = visual_outset(n);
    let p = &n.props;
    world_bounds(world, Rect::new(-o[0], -o[1], p.width + o[2], p.height + o[3]))
}

/// `b` limited to `clip`; `None` when nothing is left.
pub fn clip_to(b: Rect, clip: Option<Rect>) -> Option<Rect> {
    match clip {
        None => Some(b),
        Some(c) => (b.x0 <= c.x1 && b.x1 >= c.x0 && b.y0 <= c.y1 && b.y1 >= c.y0).then(|| b.intersect(c)),
    }
}

type Item = GeomWithData<Rectangle<[f64; 2]>, NodeId>;

/// rstar's defaults, without forced reinsertion. A bulk-loaded tree has leaves at different
/// depths, and reinsertion assumes even depth: inserting after a bulk load could panic.
struct Params;

impl RTreeParams for Params {
    const MIN_SIZE: usize = 3;
    const MAX_SIZE: usize = 6;
    const REINSERTION_COUNT: usize = 0;
    type DefaultInsertionStrategy = RStarInsertionStrategy;
}

type Tree = RTree<Item, Params>;

fn item(r: Rect, id: NodeId) -> Item {
    GeomWithData::new(Rectangle::from_corners([r.x0, r.y0], [r.x1, r.y1]), id)
}

#[derive(Debug, Clone, PartialEq)]
struct Entry {
    page: NodeId,
    parent: Option<NodeId>,
    kids: Vec<NodeId>,
    world: Affine,
    /// Ancestor clip in world space; `None` = unclipped.
    clip: Option<Rect>,
    /// The node or an ancestor is hidden.
    hidden: bool,
    /// Visual bounds before clipping; `None` when hidden or a page.
    visual: Option<Rect>,
    /// The world box this node clips its children to, if it clips.
    clip_box: Option<Rect>,
}

impl Entry {
    /// Indexed bounds (visual ∩ clip); `None` when hidden, fully clipped, or a page.
    fn bounds(&self) -> Option<Rect> {
        self.visual.and_then(|v| clip_to(v, self.clip))
    }
}

/// One R-tree per page plus a mirror of the tree, so deleted subtrees can be removed.
#[derive(Default)]
pub struct SpatialIndex {
    trees: FxHashMap<NodeId, Tree>,
    nodes: FxHashMap<NodeId, Entry>,
    /// During a large [`SpatialIndex::apply`]: pages whose R-tree is reloaded in bulk at the
    /// end instead of updated item by item.
    deferred: Option<FxHashSet<NodeId>>,
}

/// Moved subtrees bigger than this (and than an eighth of the page) reload the page's R-tree
/// in one bulk load: thousands of single removes and inserts cost several times more.
const BULK_RELOAD: usize = 1024;

/// What computing a subtree produces before it is put in the trees.
struct Built {
    id: NodeId,
    entry: Entry,
}

/// The offset of a transform that only translates.
fn translation(t: Affine) -> Option<Vec2> {
    let c = t.as_coeffs();
    ((c[0] - 1.0).abs() < 1e-12 && c[1].abs() < 1e-12 && c[2].abs() < 1e-12 && (c[3] - 1.0).abs() < 1e-12).then(|| Vec2::new(c[4], c[5]))
}

/// The clip a node's children get: its own clip narrowed by the box it clips to.
fn inner_clip(clip: Option<Rect>, clip_box: Option<Rect>) -> Option<Rect> {
    match (clip, clip_box) {
        (Some(c), Some(b)) => Some(c.intersect(b)),
        (c, b) => c.or(b),
    }
}

fn build_subtree(doc: &Document, key: NodeKey, page: NodeId, parent_world: Affine, clip: Option<Rect>, hidden: bool, out: &mut Vec<Built>) {
    let Some(n) = doc.node(key) else { return };
    let world = parent_world * local_transform(n);
    let hidden = hidden || !n.props.visible;
    let visual = (!hidden).then(|| own_bounds(n, world));
    let clip_box = clips(n).then(|| world_bounds(world, Rect::new(0.0, 0.0, n.props.width, n.props.height)));
    let child_clip = inner_clip(clip, clip_box);
    let parent = n.parent.and_then(|p| doc.node(p)).map(|p| p.id);
    let kids: Vec<NodeId> = n.children.iter().filter_map(|c| doc.node(*c).map(|c| c.id)).collect();
    out.push(Built { id: n.id, entry: Entry { page, parent, kids, world, clip, hidden, visual, clip_box } });
    for &c in &n.children {
        build_subtree(doc, c, page, world, child_clip, hidden, out);
    }
}

impl SpatialIndex {
    /// Indexes every page of `doc`, top-level subtrees in parallel.
    pub fn build(doc: &Document) -> Self {
        let mut idx = SpatialIndex::default();
        for &pk in doc.pages() {
            let Some(page) = doc.node(pk) else { continue };
            let parts: Vec<Vec<Built>> = page
                .children
                .par_iter()
                .map(|&c| {
                    let mut v = Vec::new();
                    build_subtree(doc, c, page.id, Affine::IDENTITY, None, false, &mut v);
                    v
                })
                .collect();
            let mut items = Vec::new();
            idx.nodes.reserve(parts.iter().map(Vec::len).sum::<usize>() + 1);
            for b in parts.into_iter().flatten() {
                if let Some(r) = b.entry.bounds() {
                    items.push(item(r, b.id));
                }
                idx.nodes.insert(b.id, b.entry);
            }
            idx.trees.insert(page.id, RTree::bulk_load_with_params(items));
            idx.nodes.insert(page.id, Entry {
                page: page.id,
                parent: None,
                kids: page.children.iter().filter_map(|c| doc.node(*c).map(|c| c.id)).collect(),
                world: Affine::IDENTITY,
                clip: None,
                hidden: false,
                visual: None,
                clip_box: None,
            });
        }
        idx
    }

    /// Ids on `page` whose bounds intersect `rect` (world space), in no particular order.
    pub fn query(&self, page: NodeId, rect: Rect) -> impl Iterator<Item = NodeId> + '_ {
        let env = AABB::from_corners([rect.x0, rect.y0], [rect.x1, rect.y1]);
        self.trees.get(&page).into_iter().flat_map(move |t| t.locate_in_envelope_intersecting(env).map(|i| i.data))
    }

    /// The union of a page's indexed bounds.
    pub fn page_bounds(&self, page: NodeId) -> Option<Rect> {
        let t = self.trees.get(&page).filter(|t| t.size() > 0)?;
        let e = t.root().envelope();
        let (lo, hi) = (e.lower(), e.upper());
        Some(Rect::new(lo[0], lo[1], hi[0], hi[1]))
    }

    /// Indexed (visible, clipped) world bounds of a node.
    pub fn bounds(&self, id: NodeId) -> Option<Rect> {
        self.nodes.get(&id).and_then(Entry::bounds)
    }

    /// The node's transform to world (page) space.
    pub fn world(&self, id: NodeId) -> Option<Affine> {
        self.nodes.get(&id).map(|e| e.world)
    }

    /// The world-space clip from ancestors, if any.
    pub fn clip(&self, id: NodeId) -> Option<Rect> {
        self.nodes.get(&id).and_then(|e| e.clip)
    }

    pub fn len(&self) -> usize {
        self.trees.values().map(RTree::size).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Brings the index up to date after a transaction (or undo/redo) described by `changes`.
    pub fn apply(&mut self, doc: &Document, changes: &ChangeSet) {
        let mut roots: Vec<NodeId> = Vec::new();
        let mut selves: Vec<NodeId> = Vec::new();
        roots.extend(changes.deleted.iter().copied());
        roots.extend(changes.inserted.iter().copied());
        roots.extend(changes.moved.iter().copied());
        for &(id, k) in &changes.updated {
            match k {
                PropKey::X | PropKey::Y | PropKey::Width | PropKey::Height | PropKey::Rotation | PropKey::FlipX | PropKey::FlipY | PropKey::Visible | PropKey::Kind => {
                    roots.push(id)
                }
                PropKey::Strokes | PropKey::Effects => selves.push(id),
                _ => {}
            }
        }
        // Detach moved and deleted subtrees from where the index last saw them first: a moved
        // node under a resynced ancestor is rebuilt by that ancestor, which never sees its old
        // parent's child list or its old tree items.
        for &id in changes.deleted.iter().chain(&changes.moved) {
            if let Some(parent) = self.nodes.get(&id).and_then(|e| e.parent)
                && let Some(p) = self.nodes.get_mut(&parent)
            {
                p.kids.retain(|k| *k != id);
            }
            self.remove_subtree(id);
        }
        // Bring each changed subtree up to date once: skip a root whose ancestor (in the
        // document) is also a root. Inserted and moved subtrees are built afresh; the others are
        // refreshed top-down, recomputing only the nodes on the way to changed descendants.
        let set: FxHashSet<NodeId> = roots.iter().copied().collect();
        self.deferred = self.large(&set).then(FxHashSet::default);
        let structural: FxHashSet<NodeId> = changes.inserted.iter().chain(&changes.moved).chain(&changes.deleted).copied().collect();
        let mut dirty = FxHashSet::default();
        let mut seen = FxHashSet::default();
        let mut todo = Vec::new();
        for &id in &roots {
            if !seen.insert(id) {
                continue;
            }
            dirty.insert(id);
            let mut covered = false;
            if let Some(k) = doc.key(id) {
                for a in doc.ancestors(k).filter_map(|a| doc.node(a)) {
                    covered |= set.contains(&a.id);
                    dirty.insert(a.id);
                }
            }
            if !covered {
                todo.push(id);
            }
        }
        for id in todo {
            let parent = self.nodes.get(&id).and_then(|e| e.parent).and_then(|p| self.nodes.get(&p)).map(|p| (p.world, inner_clip(p.clip, p.clip_box), p.hidden));
            match parent {
                Some((world, clip, hidden)) if !structural.contains(&id) => self.refresh(doc, id, world, clip, hidden, &dirty),
                _ => self.resync(doc, id),
            }
        }
        for id in selves {
            if !set.contains(&id) {
                self.refresh_one(doc, id);
            }
        }
        for page in self.deferred.take().into_iter().flatten() {
            if !self.trees.contains_key(&page) {
                continue;
            }
            let items = self.nodes.iter().filter(|(_, e)| e.page == page).filter_map(|(id, e)| e.bounds().map(|r| item(r, *id))).collect();
            self.trees.insert(page, RTree::bulk_load_with_params(items));
        }
    }

    /// Brings `id`'s subtree up to date under its parent's current transform, clip and
    /// visibility: recomputes `id`, then recurses into children on the way to changed nodes
    /// (`dirty`), shifts children whose placement only moved, and leaves the rest. Rebuilds
    /// from the document where the children or the visibility changed.
    fn refresh(&mut self, doc: &Document, id: NodeId, pworld: Affine, clip: Option<Rect>, phidden: bool, dirty: &FxHashSet<NodeId>) {
        let Some(n) = doc.get(id) else { return self.resync(doc, id) };
        let hidden = phidden || !n.props.visible;
        let same = self.nodes.get(&id).is_some_and(|e| {
            e.hidden == hidden && e.kids.len() == n.children.len() && e.kids.iter().zip(&n.children).all(|(k, &c)| doc.node(c).is_some_and(|c| c.id == *k))
        });
        if !same {
            return self.resync(doc, id);
        }
        let world = pworld * local_transform(n);
        let visual = (!hidden).then(|| own_bounds(n, world));
        let bounds = visual.and_then(|v| clip_to(v, clip));
        let clip_box = clips(n).then(|| world_bounds(world, local_box(n)));
        let Some(e) = self.nodes.get_mut(&id) else { return };
        let (old_world, old_inner, old_bounds) = (e.world, inner_clip(e.clip, e.clip_box), e.bounds());
        (e.world, e.visual, e.clip, e.clip_box) = (world, visual, clip, clip_box);
        let kids = e.kids.clone();
        let page = e.page;
        self.move_item(page, id, old_bounds, bounds);
        let inner = inner_clip(clip, clip_box);
        if world == old_world && inner == old_inner {
            for c in kids.into_iter().filter(|c| dirty.contains(c)) {
                self.refresh(doc, c, world, inner, hidden, dirty);
            }
            return;
        }
        let shift = translation(world * old_world.inverse());
        for c in kids {
            match shift {
                _ if dirty.contains(&c) => self.refresh(doc, c, world, inner, hidden, dirty),
                Some(v) => self.shift(c, v, inner),
                // Rotated or flipped: rebuild under the updated entry.
                None => self.resync(doc, c),
            }
        }
    }

    /// Moves an unchanged subtree by `v` (its parent moved), recomputing clips from `clip` down.
    fn shift(&mut self, id: NodeId, v: Vec2, clip: Option<Rect>) {
        let Some(page) = self.nodes.get(&id).map(|e| e.page) else { return };
        let mut stack = vec![(id, clip)];
        while let Some((k, clip)) = stack.pop() {
            let Some(e) = self.nodes.get_mut(&k) else { continue };
            let old = e.bounds();
            e.world = Affine::translate(v) * e.world;
            e.visual = e.visual.map(|r| r + v);
            e.clip_box = e.clip_box.map(|r| r + v);
            e.clip = clip;
            let inner = inner_clip(clip, e.clip_box);
            stack.extend(e.kids.iter().map(|&c| (c, inner)));
            let new = e.bounds();
            self.move_item(page, k, old, new);
        }
    }

    /// Replaces a node's item in its page's tree (unless the tree is reloaded at the end).
    fn move_item(&mut self, page: NodeId, id: NodeId, old: Option<Rect>, new: Option<Rect>) {
        if old == new {
            return;
        }
        if let Some(t) = self.tree_mut(page) {
            if let Some(o) = old {
                t.remove(&item(o, id));
            }
            if let Some(r) = new {
                t.insert(item(r, id));
            }
        }
    }

    /// Whether the indexed subtrees under `roots` hold enough nodes that reloading their pages'
    /// trees beats updating them item by item.
    fn large(&self, roots: &FxHashSet<NodeId>) -> bool {
        let mut pages: FxHashMap<NodeId, usize> = FxHashMap::default();
        for id in roots {
            let Some(e) = self.nodes.get(id) else { continue };
            let n = pages.entry(e.page).or_default();
            let size = self.trees.get(&e.page).map_or(0, RTree::size);
            let mut stack = vec![*id];
            while let Some(k) = stack.pop() {
                *n += 1;
                if *n > BULK_RELOAD && *n > size / 8 {
                    return true;
                }
                if let Some(e) = self.nodes.get(&k) {
                    stack.extend(&e.kids);
                }
            }
        }
        false
    }

    /// The page's tree, unless its updates are deferred to a bulk reload.
    fn tree_mut(&mut self, page: NodeId) -> Option<&mut Tree> {
        if let Some(d) = &mut self.deferred {
            d.insert(page);
            return None;
        }
        self.trees.get_mut(&page)
    }

    fn remove_subtree(&mut self, id: NodeId) {
        let Some(e) = self.nodes.remove(&id) else { return };
        if let Some(r) = e.bounds()
            && let Some(t) = self.tree_mut(e.page)
        {
            t.remove(&item(r, id));
        }
        for k in e.kids {
            self.remove_subtree(k);
        }
    }

    fn resync(&mut self, doc: &Document, id: NodeId) {
        if let Some(parent) = self.nodes.get(&id).and_then(|e| e.parent)
            && let Some(p) = self.nodes.get_mut(&parent)
        {
            p.kids.retain(|k| *k != id);
        }
        if self.nodes.get(&id).is_some_and(|e| e.parent.is_none()) {
            // A page: drop its tree too.
            self.trees.remove(&id);
        }
        self.remove_subtree(id);
        let Some(key) = doc.key(id) else { return };
        let Some(n) = doc.node(key) else { return };
        if n.kind.is_page() {
            let mut sub = SpatialIndex::default();
            // Rebuild just this page.
            for b in {
                let mut v = Vec::new();
                for &c in &n.children {
                    build_subtree(doc, c, n.id, Affine::IDENTITY, None, false, &mut v);
                }
                v
            } {
                sub.nodes.insert(b.id, b.entry);
            }
            let items = sub.nodes.iter().filter_map(|(id, e)| e.bounds().map(|r| item(r, *id))).collect();
            self.trees.insert(n.id, RTree::bulk_load_with_params(items));
            self.nodes.extend(sub.nodes);
            self.nodes.insert(n.id, Entry {
                page: n.id,
                parent: None,
                kids: n.children.iter().filter_map(|c| doc.node(*c).map(|c| c.id)).collect(),
                world: Affine::IDENTITY,
                clip: None,
                hidden: false,
                visual: None,
                clip_box: None,
            });
            return;
        }
        if let Some((parent, page, built)) = self.build_under_parent(doc, id) {
            self.add_subtree(id, parent, page, built);
        }
    }

    /// Builds `id`'s subtree from the document under its parent's indexed transform and clip:
    /// the parent's id, the page and the entries (`None` for a page or an unindexed parent).
    fn build_under_parent(&self, doc: &Document, id: NodeId) -> Option<(NodeId, NodeId, Vec<Built>)> {
        let key = doc.key(id)?;
        let pk = doc.node(key)?.parent?;
        let parent = doc.node(pk)?;
        let pe = self.nodes.get(&parent.id)?;
        let (page, pworld) = (pe.page, pe.world);
        let hidden = !parent.props.visible || doc.ancestors(pk).any(|a| doc.node(a).is_some_and(|a| !a.props.visible));
        let child_clip = if clips(parent) && !parent.kind.is_page() {
            let own = world_bounds(pworld, Rect::new(0.0, 0.0, parent.props.width, parent.props.height));
            Some(pe.clip.map_or(own, |c| c.intersect(own)))
        } else {
            pe.clip
        };
        let mut built = Vec::new();
        build_subtree(doc, key, page, pworld, child_clip, hidden, &mut built);
        Some((parent.id, page, built))
    }

    fn add_subtree(&mut self, id: NodeId, parent: NodeId, page: NodeId, built: Vec<Built>) {
        if let Some(p) = self.nodes.get_mut(&parent) {
            p.kids.push(id);
        }
        self.trees.entry(page).or_default();
        let live = self.tree_mut(page).is_some();
        for b in built {
            if live
                && let Some(r) = b.entry.bounds()
                && let Some(t) = self.trees.get_mut(&page)
            {
                t.insert(item(r, b.id));
            }
            self.nodes.insert(b.id, b.entry);
        }
    }

    /// Recomputes one node's own bounds (stroke or effect change), keeping its transform.
    fn refresh_one(&mut self, doc: &Document, id: NodeId) {
        let Some(n) = doc.get(id) else { return };
        let Some(e) = self.nodes.get_mut(&id) else { return };
        if e.hidden {
            return;
        }
        let old = e.bounds();
        e.visual = Some(own_bounds(n, e.world));
        let (page, new) = (e.page, e.bounds());
        self.move_item(page, id, old, new);
    }

    /// Every indexed node with its bounds, sorted by id (for tests).
    pub fn snapshot(&self) -> Vec<(NodeId, Option<Rect>, Affine)> {
        let mut v: Vec<_> = self.nodes.iter().map(|(id, e)| (*id, e.bounds(), e.world)).collect();
        v.sort_by_key(|x| x.0);
        v
    }
}

/// The bounds of a node's local box, `Rect::new(0, 0, w, h)`.
pub fn local_box(n: &Node) -> Rect {
    Rect::new(0.0, 0.0, n.props.width, n.props.height)
}
