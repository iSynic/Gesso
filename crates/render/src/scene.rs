//! A page's retained render state: fragments with cached display lists per zoom band, the
//! spatial index, and the text and image caches. Kept current from each transaction's
//! [`ChangeSet`]; produces a [`Frame`] for a camera.
//!
//! A fragment is a top-level node of the page with its subtree. A subtree larger than
//! [`Scene::split_threshold`] nodes is split, when nothing about the node composites its
//! children as a group: the node's own fills (`Pre`), each child as its own fragment, then its
//! strokes (`Post`). Each fragment's commands are in its root node's local space, so moving a
//! fragment root only changes where the cached list is drawn.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use doc::model::AssetId;
use doc::{ChangeSet, Document, Node, NodeId, NodeKey, NodeKind, PropKey};
use index::{SpatialIndex, local_transform};
use kurbo::{Affine, BezPath, Rect};
use peniko::Color;
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use text::{FontDb, TextEngine, TextInput};

use crate::build::{Band, Builder, Caches, Part, splittable};
use crate::images::{AssetSource, ImageCache};
use crate::list::DisplayList;
use crate::{Frame, FrameItem, geometry, paint};

/// Default subtree size above which a node is split into several fragments.
pub const SPLIT_THRESHOLD: usize = 2000;

/// Fragments with an older list prefetched together under an encode budget: enough for the
/// parallel shaper to have work, few enough to stop near the budget.
const PREFETCH_CHUNK: usize = 4;
/// Fragments that must be built in one frame from which they are built in parallel (e.g. every
/// instance of an edited component).
const PARALLEL_BUILD: usize = 32;

/// Fragment ids are unique across scenes, so a renderer shared by several scenes (documents,
/// exports) can key its caches by them.
static NEXT_FRAGMENT: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct FragKey {
    root: NodeId,
    part: Part,
}

struct Frag {
    id: u64,
    /// Lists per band, their versions (bumped on each rebuild), and whether each is current.
    lists: [Option<Arc<DisplayList>>; Band::COUNT],
    versions: [u64; Band::COUNT],
    fresh: [bool; Band::COUNT],
    /// Estimated bounds (root-local) while no current list exists.
    estimate: Option<Rect>,
    /// Ancestors (split, clipping) whose clip applies, outermost first.
    clips: Vec<NodeId>,
}

impl Frag {
    fn dirty(&mut self) {
        self.fresh = [false; Band::COUNT];
        self.estimate = None;
    }
}

/// What one frame did.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct FrameStats {
    pub fragments: usize,
    pub visible: usize,
    pub built: usize,
    /// Stale lists drawn because the encode budget ran out.
    pub stale: usize,
    pub build_ms: f64,
    /// Of `build_ms`, time spent shaping text and decoding images ahead of building.
    pub prefetch_ms: f64,
}

pub struct Scene {
    page: NodeId,
    pub index: SpatialIndex,
    pub text: TextEngine,
    pub images: ImageCache,
    assets: Arc<dyn AssetSource>,
    frags: FxHashMap<FragKey, Frag>,
    /// Fragments in drawing order.
    order: Vec<FragKey>,
    /// Which fragment draws each node.
    member: FxHashMap<NodeId, FragKey>,
    /// Nodes that are split.
    split: FxHashSet<NodeId>,
    replan: bool,
    /// Fragments drawn with a placeholder for an image still decoding in the background.
    waiting: FxHashMap<AssetId, Vec<FragKey>>,
    pub split_threshold: usize,
    pub stats: FrameStats,
}

fn subtree_sizes(doc: &Document, key: NodeKey, out: &mut FxHashMap<NodeKey, usize>) -> usize {
    let Some(n) = doc.node(key) else { return 0 };
    let mut s = 1;
    for &c in &n.children {
        s += subtree_sizes(doc, c, out);
    }
    out.insert(key, s);
    s
}

impl Scene {
    pub fn new(doc: &Document, page: NodeId, fonts: FontDb, assets: Arc<dyn AssetSource>) -> Self {
        let mut s = Scene {
            page,
            index: SpatialIndex::build(doc),
            text: TextEngine::new(fonts),
            images: ImageCache::default(),
            assets,
            frags: FxHashMap::default(),
            order: Vec::new(),
            member: FxHashMap::default(),
            split: FxHashSet::default(),
            replan: true,
            waiting: FxHashMap::default(),
            split_threshold: SPLIT_THRESHOLD,
            stats: FrameStats::default(),
        };
        s.plan(doc);
        s
    }

    /// Where image bytes come from.
    pub fn assets(&self) -> Arc<dyn AssetSource> {
        self.assets.clone()
    }

    pub fn page(&self) -> NodeId {
        self.page
    }

    /// Changes the split size; the fragments are re-planned on the next frame.
    pub fn set_split_threshold(&mut self, nodes: usize) {
        self.split_threshold = nodes;
        self.replan = true;
    }

    pub fn fragment_count(&self) -> usize {
        self.order.len()
    }

    /// Re-derives the fragment structure. Fragments whose (root, part) survive keep their lists.
    fn plan(&mut self, doc: &Document) {
        self.replan = false;
        let Some(pk) = doc.key(self.page) else {
            self.order.clear();
            self.frags.clear();
            self.member.clear();
            return;
        };
        let mut sizes = FxHashMap::default();
        subtree_sizes(doc, pk, &mut sizes);
        let mut order = Vec::new();
        let mut member = FxHashMap::default();
        let mut split = FxHashSet::default();
        let mut clips = FxHashMap::default();
        let page_kids = doc.node(pk).map(|p| p.children.clone()).unwrap_or_default();
        for k in page_kids {
            self.plan_node(doc, k, &sizes, &mut Vec::new(), &mut order, &mut member, &mut split, &mut clips);
        }
        let mut frags = std::mem::take(&mut self.frags);
        let mut next = FxHashMap::default();
        for key in &order {
            let f = frags.remove(key).unwrap_or_else(|| {
                Frag { id: NEXT_FRAGMENT.fetch_add(1, Ordering::Relaxed), versions: [0; Band::COUNT], lists: Default::default(), fresh: [false; Band::COUNT], estimate: None, clips: Vec::new() }
            });
            next.insert(*key, f);
        }
        for (k, c) in clips {
            if let Some(f) = next.get_mut(&k) {
                if f.clips != c {
                    f.dirty();
                }
                f.clips = c;
            }
        }
        self.frags = next;
        self.order = order;
        self.member = member;
        self.split = split;
    }

    /// Adds inserted subtrees to the plan without re-planning the page, giving the same result as
    /// [`Scene::plan`]: new top-level nodes at the end of the page get their own fragments, and a
    /// node inserted inside a whole fragment joins it while that fragment stays below the split
    /// size. Returns false (leaving a partial plan that `plan` replaces) for anything else.
    fn plan_inserts(&mut self, doc: &Document, inserted: &[NodeId]) -> bool {
        let Some(pk) = doc.key(self.page) else { return false };
        let Some(kids) = doc.node(pk).map(|p| p.children.clone()) else { return false };
        let mut top = Vec::new();
        let mut inner = Vec::new();
        for id in inserted {
            let Some(k) = doc.key(*id) else { return false };
            // Pages, and nodes on other pages, aren't drawn by this scene.
            if std::iter::once(k).chain(doc.ancestors(k)).last() != Some(pk) || k == pk {
                continue;
            }
            if doc.node(k).and_then(|n| n.parent) == Some(pk) {
                top.push(k);
            } else {
                inner.push(k);
            }
        }
        // New top-level nodes must be the last children of the page, so their fragments go last.
        let tail = &kids[kids.len().saturating_sub(top.len())..];
        if !tail.iter().all(|k| top.contains(k)) {
            return false;
        }
        for &k in tail {
            let mut sizes = FxHashMap::default();
            subtree_sizes(doc, k, &mut sizes);
            let (mut order, mut member, mut split, mut clips) = (Vec::new(), FxHashMap::default(), FxHashSet::default(), FxHashMap::default());
            self.plan_node(doc, k, &sizes, &mut Vec::new(), &mut order, &mut member, &mut split, &mut clips);
            for key in &order {
                let f = Frag { id: NEXT_FRAGMENT.fetch_add(1, Ordering::Relaxed), versions: [0; Band::COUNT], lists: Default::default(), fresh: [false; Band::COUNT], estimate: None, clips: clips.remove(key).unwrap_or_default() };
                self.frags.insert(*key, f);
            }
            self.order.extend(order);
            self.member.extend(member);
            self.split.extend(split);
        }
        for k in inner {
            let Some(n) = doc.node(k) else { return false };
            if self.member.contains_key(&n.id) {
                continue;
            }
            let Some(parent) = n.parent.and_then(|p| doc.node(p)) else { return false };
            let Some(&fk) = self.member.get(&parent.id) else { return false };
            if fk.part != Part::Whole {
                return false;
            }
            // The fragment root would split now that it has grown.
            let Some(rk) = doc.key(fk.root) else { return false };
            let Some(root) = doc.node(rk) else { return false };
            if root.props.visible && splittable(root) && !root.children.is_empty() && doc.descendants(rk).len() > self.split_threshold {
                return false;
            }
            for d in doc.descendants(k) {
                if let Some(dn) = doc.node(d) {
                    self.member.insert(dn.id, fk);
                }
            }
        }
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn plan_node(
        &self,
        doc: &Document,
        key: NodeKey,
        sizes: &FxHashMap<NodeKey, usize>,
        clip_chain: &mut Vec<NodeId>,
        order: &mut Vec<FragKey>,
        member: &mut FxHashMap<NodeId, FragKey>,
        split: &mut FxHashSet<NodeId>,
        clips: &mut FxHashMap<FragKey, Vec<NodeId>>,
    ) {
        let Some(n) = doc.node(key) else { return };
        let size = sizes.get(&key).copied().unwrap_or(1);
        if size > self.split_threshold && n.props.visible && splittable(n) && !n.children.is_empty() {
            split.insert(n.id);
            let pre = FragKey { root: n.id, part: Part::Pre };
            member.insert(n.id, pre);
            order.push(pre);
            clips.insert(pre, clip_chain.clone());
            let clipping = index::clips(n);
            if clipping {
                clip_chain.push(n.id);
            }
            for &c in &n.children {
                self.plan_node(doc, c, sizes, clip_chain, order, member, split, clips);
            }
            if clipping {
                clip_chain.pop();
            }
            let post = FragKey { root: n.id, part: Part::Post };
            order.push(post);
            clips.insert(post, clip_chain.clone());
        } else {
            let whole = FragKey { root: n.id, part: Part::Whole };
            order.push(whole);
            clips.insert(whole, clip_chain.clone());
            for k in doc.descendants(key) {
                if let Some(d) = doc.node(k) {
                    member.insert(d.id, whole);
                }
            }
        }
    }

    fn dirty_key(&mut self, k: FragKey) {
        if let Some(f) = self.frags.get_mut(&k) {
            f.dirty();
        }
        if k.part == Part::Pre
            && let Some(f) = self.frags.get_mut(&FragKey { root: k.root, part: Part::Post })
        {
            f.dirty();
        }
    }

    fn dirty_node(&mut self, id: NodeId) {
        if let Some(k) = self.member.get(&id).copied() {
            self.dirty_key(k);
        }
    }

    /// Brings the scene up to date after a transaction, undo or redo.
    pub fn apply(&mut self, doc: &Document, changes: &ChangeSet) {
        self.index.apply(doc, changes);
        // Fragments that drew deleted or moved nodes, before the structure changes.
        for id in changes.deleted.iter().chain(&changes.moved) {
            self.dirty_node(*id);
        }
        let structural = !changes.inserted.is_empty() || !changes.deleted.is_empty() || !changes.moved.is_empty();
        for (id, k) in &changes.updated {
            let is_root = self.frags.contains_key(&FragKey { root: *id, part: Part::Whole });
            let is_split = self.split.contains(id);
            match k {
                PropKey::X | PropKey::Y | PropKey::Rotation | PropKey::FlipX | PropKey::FlipY if is_root || is_split => {}
                PropKey::Visible | PropKey::Opacity | PropKey::Blend | PropKey::Effects | PropKey::Kind if is_split => self.replan = true,
                // A whole fragment that can split again (its effect was removed, say).
                PropKey::Opacity | PropKey::Blend | PropKey::Effects if is_root && doc.get(*id).is_some_and(splittable) => {
                    self.replan = true;
                    self.dirty_node(*id);
                }
                // A text is a leaf whatever its content, so typing leaves the plan as it is.
                PropKey::Kind if doc.get(*id).is_some_and(|n| matches!(n.kind, NodeKind::Text { .. })) => self.dirty_node(*id),
                PropKey::Visible | PropKey::Kind => {
                    // Showing or hiding can change a subtree's size or a split decision.
                    self.replan = true;
                    self.dirty_node(*id);
                }
                _ => self.dirty_node(*id),
            }
        }
        if changes.assets.iter().len() > 0 {
            for f in self.frags.values_mut() {
                f.dirty();
            }
        }
        // Inserts alone usually extend the plan in place; anything else re-plans the page.
        if structural && (self.replan || !changes.deleted.is_empty() || !changes.moved.is_empty() || !self.plan_inserts(doc, &changes.inserted)) {
            self.replan = true;
        }
        if self.replan {
            self.plan(doc);
        }
        for id in changes.inserted.iter().chain(&changes.moved) {
            self.dirty_node(*id);
            // The new parent's fragment (an inserted child of a split node is its own fragment).
            if let Some(p) = doc.get(*id).and_then(|n| n.parent).and_then(|p| doc.node(p)) {
                let pid = p.id;
                self.dirty_node(pid);
            }
        }
    }

    /// Image assets used inside `key`'s subtree (instance content included).
    pub fn assets_in(doc: &Document, key: NodeKey, out: &mut Vec<(AssetId, doc::model::AssetMeta)>) {
        let Some(n) = doc.node(key) else { return };
        let mut add = |a: &AssetId| {
            if let Some(m) = doc.assets().get(a) {
                out.push((a.clone(), m.clone()));
            }
        };
        if let NodeKind::Image { asset, .. } = &n.kind {
            add(asset);
        }
        for p in &n.props.fills {
            if let doc::model::PaintKind::Image { asset, .. } = &p.kind {
                add(asset);
            }
        }
        for &c in &n.children {
            Self::assets_in(doc, c, out);
        }
    }

    /// Texts the band will shape inside `key`'s subtree.
    fn texts_in<'d>(doc: &'d Document, key: NodeKey, band: Band, out: &mut Vec<(TextInput<'d>, Option<f32>)>) {
        let Some(n) = doc.node(key) else { return };
        if !n.props.visible {
            return;
        }
        if let NodeKind::Text { style, spans, .. } = &n.kind {
            let size = spans.iter().filter_map(|s| s.font_size).fold(style.font_size, f64::max);
            if size * band.lod_zoom() >= crate::build::GREEK_PX
                && let Some(t) = TextInput::from_kind(&n.kind)
            {
                out.push((t, layout::semantics::text_wraps(n).then_some(n.props.width as f32)));
            }
        }
        for &c in &n.children {
            Self::texts_in(doc, c, band, out);
        }
    }

    fn world(&self, doc: &Document, id: NodeId) -> Affine {
        self.index.world(id).unwrap_or_else(|| {
            // Not indexed (shouldn't happen): compose from the document.
            let Some(k) = doc.key(id) else { return Affine::IDENTITY };
            let mut t = doc.node(k).map_or(Affine::IDENTITY, local_transform);
            for a in doc.ancestors(k) {
                if let Some(n) = doc.node(a) {
                    t = local_transform(n) * t;
                }
            }
            t
        })
    }

    /// Root-local bounds estimated from the index (used to cull fragments without a list).
    fn estimate(&self, doc: &Document, key: FragKey) -> Option<Rect> {
        let k = doc.key(key.root)?;
        let ids: Vec<NodeId> = match key.part {
            Part::Whole => doc.descendants(k).into_iter().filter_map(|d| doc.node(d)).map(|n| n.id).collect(),
            _ => vec![key.root],
        };
        let inv = self.index.world(key.root)?.inverse();
        let mut r: Option<Rect> = None;
        for id in ids {
            if let Some(b) = self.index.bounds(id) {
                let b = inv.transform_rect_bbox(b);
                r = Some(r.map_or(b, |x| x.union(b)));
            }
        }
        r
    }

    fn build(&mut self, doc: &Document, key: FragKey, band: Band) -> Arc<DisplayList> {
        let mut out = DisplayList::default();
        if let Some(k) = doc.key(key.root) {
            Builder::new(doc, Caches::Fill { text: &mut self.text, images: &mut self.images }, band).root(k, key.part, &mut out);
        }
        Arc::new(out)
    }

    /// Prepares caches for building `keys` at `band`: shapes text in parallel, and decodes
    /// images in parallel (or starts decoding them in the background, noting which fragments
    /// wait for them).
    fn prefetch(&mut self, doc: &Document, keys: &[FragKey], band: Band) {
        let mut assets = Vec::new();
        let mut texts = Vec::new();
        for key in keys {
            let Some(k) = doc.key(key.root) else { continue };
            let from = assets.len();
            match key.part {
                Part::Whole => {
                    Self::assets_in(doc, k, &mut assets);
                    Self::texts_in(doc, k, band, &mut texts);
                }
                _ => {
                    if let Some(n) = doc.node(k) {
                        for p in &n.props.fills {
                            if let doc::model::PaintKind::Image { asset, .. } = &p.kind
                                && let Some(m) = doc.assets().get(asset)
                            {
                                assets.push((asset.clone(), m.clone()));
                            }
                        }
                    }
                }
            }
            if self.images.is_async() {
                for (id, _) in &assets[from..] {
                    if !self.images.is_cached(id) {
                        let w = self.waiting.entry(id.clone()).or_default();
                        if !w.contains(key) {
                            w.push(*key);
                        }
                    }
                }
            }
        }
        if !assets.is_empty() {
            assets.sort_by(|a, b| a.0.cmp(&b.0));
            assets.dedup_by(|a, b| a.0 == b.0);
            let src = self.assets.clone();
            self.images.request(&src, &assets);
        }
        if texts.len() > 1 {
            self.text.shape_many(&texts);
        }
    }

    /// Decodes images in the background from now on, calling `wake` as each finishes (frames
    /// draw placeholders until then); `None` decodes them before drawing.
    pub fn set_image_waker(&mut self, wake: Option<crate::Waker>) {
        self.images.set_waker(wake);
        self.waiting.clear();
    }

    /// Background image decodes under way.
    pub fn images_pending(&self) -> usize {
        self.images.pending()
    }

    /// Whether finished decodes wait for the next frame (keep redrawing while so).
    pub fn images_ready(&self) -> bool {
        self.images.has_ready()
    }

    /// Takes in finished background decodes (waiting for all of them if `all`) and marks the
    /// fragments drawn without them for rebuilding.
    fn take_decoded(&mut self, all: bool) {
        let ids = if all { self.images.wait() } else { self.images.poll() };
        for id in ids {
            for k in self.waiting.remove(&id).unwrap_or_default() {
                self.dirty_key(k);
            }
        }
    }

    /// Like [`Scene::frame`] with no budget, but complete: every image decoded and drawn (for
    /// screenshots and exports of an interactive scene).
    pub fn frame_complete(&mut self, doc: &Document, camera: Affine, width: u32, height: u32) -> Frame {
        let wake = self.images.take_waker();
        self.take_decoded(true);
        let f = self.frame(doc, camera, width, height, None);
        self.images.restore_waker(wake);
        f
    }

    fn clip_paths(&self, doc: &Document, ids: &[NodeId], memo: &mut FxHashMap<NodeId, (Arc<BezPath>, Affine)>) -> Vec<(Arc<BezPath>, Affine)> {
        ids.iter()
            .filter_map(|id| {
                if let Some(c) = memo.get(id) {
                    return Some(c.clone());
                }
                let n = doc.get(*id)?;
                let c = (Arc::new(geometry::clip_path(n)), self.world(doc, *id));
                memo.insert(*id, c.clone());
                Some(c)
            })
            .collect()
    }

    /// The frame for `camera` (world → device) on a `width × height` target. Dirty visible
    /// fragments are rebuilt; once `budget` is spent, fragments that have an older list keep
    /// drawing it until a later frame.
    pub fn frame(&mut self, doc: &Document, camera: Affine, width: u32, height: u32, budget: Option<Duration>) -> Frame {
        if self.replan {
            self.plan(doc);
        }
        self.take_decoded(false);
        let t0 = Instant::now();
        let zoom = crate::exec::scale(camera);
        let band = Band::for_zoom(zoom);
        let b = band as usize;
        let view = camera.inverse().transform_rect_bbox(Rect::new(0.0, 0.0, width as f64, height as f64));
        let mut stats = FrameStats { fragments: self.order.len(), ..Default::default() };
        // Cull, using current lists where possible and index estimates otherwise.
        let mut visible: Vec<(FragKey, Affine)> = Vec::new();
        let order = self.order.clone();
        for key in order {
            let xf = self.world(doc, key.root);
            let local = match self.frags.get(&key) {
                Some(f) if f.fresh.iter().any(|x| *x) => f.lists.iter().zip(f.fresh).find(|(_, fr)| *fr).and_then(|(l, _)| l.as_ref()).and_then(|l| l.bounds),
                Some(f) if f.estimate.is_some() => f.estimate,
                _ => {
                    let e = self.estimate(doc, key);
                    if let Some(f) = self.frags.get_mut(&key) {
                        f.estimate = e;
                    }
                    e
                }
            };
            let Some(local) = local else { continue };
            // Ancestor clips, as the index applies them (a cached list's bounds don't include them).
            let Some(wb) = index::clip_to(xf.transform_rect_bbox(local), self.index.clip(key.root)) else { continue };
            if wb.x1 < view.x0 || wb.x0 > view.x1 || wb.y1 < view.y0 || wb.y0 > view.y1 {
                continue;
            }
            visible.push((key, xf));
        }
        // Build what's missing or stale, text and images first in parallel. Fragments with no
        // list at all must be built now; those with an older list are prefetched a few at a time
        // so the budget bounds shaping as well as building.
        let (mut must, mut later) = (Vec::new(), Vec::new());
        for (k, _) in &visible {
            if let Some(f) = self.frags.get(k).filter(|f| !f.fresh[b]) {
                if budget.is_some() && f.lists.iter().any(Option::is_some) { later.push(*k) } else { must.push(*k) }
            }
        }
        if !must.is_empty() {
            let tp = Instant::now();
            self.prefetch(doc, &must, band);
            stats.prefetch_ms += tp.elapsed().as_secs_f64() * 1e3;
        }
        if must.len() >= PARALLEL_BUILD {
            // Text and images are prefetched, so builders only read the caches. A fragment that
            // still misses text is left stale and built below, on this thread.
            let (text, images) = (&self.text, &self.images);
            let lists: Vec<(FragKey, Option<Arc<DisplayList>>)> = must
                .par_iter()
                .map(|key| {
                    let mut out = DisplayList::default();
                    let Some(k) = doc.key(key.root) else { return (*key, Some(Arc::new(out))) };
                    let mut b = Builder::new(doc, Caches::Read { text, images, missed: false }, band);
                    b.root(k, key.part, &mut out);
                    (*key, (!b.caches.missed()).then(|| Arc::new(out)))
                })
                .collect();
            for (key, list) in lists {
                let (Some(list), Some(f)) = (list, self.frags.get_mut(&key)) else { continue };
                f.versions[b] += 1;
                f.lists[b] = Some(list);
                f.fresh[b] = true;
                f.estimate = None;
                stats.built += 1;
            }
        }
        let mut ready: FxHashSet<FragKey> = must.into_iter().collect();
        let mut next = 0;
        let mut items = Vec::with_capacity(visible.len());
        let mut memo = FxHashMap::default();
        for (key, xf) in visible {
            // At least one build per frame, so a spent budget can't stall a band change.
            let over = stats.built > 0 && budget.is_some_and(|bd| t0.elapsed() > bd);
            let (fresh, has) = match self.frags.get(&key) {
                Some(f) => (f.fresh[b], f.lists.iter().any(Option::is_some)),
                None => continue,
            };
            if !fresh && !(over && has) {
                if !ready.contains(&key) {
                    let tp = Instant::now();
                    let chunk: Vec<FragKey> = later[next.min(later.len())..].iter().copied().take(PREFETCH_CHUNK).collect();
                    next += chunk.len();
                    self.prefetch(doc, &chunk, band);
                    ready.extend(chunk);
                    stats.prefetch_ms += tp.elapsed().as_secs_f64() * 1e3;
                }
                let list = self.build(doc, key, band);
                if let Some(f) = self.frags.get_mut(&key) {
                    f.versions[b] += 1;
                    f.lists[b] = Some(list);
                    f.fresh[b] = true;
                    f.estimate = None;
                }
                stats.built += 1;
            } else if !fresh {
                stats.stale += 1;
            }
            let Some(f) = self.frags.get(&key) else { continue };
            // Until it's rebuilt, the nearest band's list stands in.
            let near = (0..Band::COUNT).filter(|i| f.lists[*i].is_some()).min_by_key(|i| i.abs_diff(b));
            let Some((i, list)) = near.and_then(|i| Some((i, f.lists[i].clone()?))) else { continue };
            if list.is_empty() {
                continue;
            }
            let (id, version, clip_ids) = (f.id, f.versions[i], f.clips.clone());
            let clips = self.clip_paths(doc, &clip_ids, &mut memo);
            items.push(FrameItem { fragment: id << 3 | i as u64, version, list, xf, clips });
        }
        stats.visible = items.len();
        stats.build_ms = t0.elapsed().as_secs_f64() * 1e3;
        self.stats = stats;
        self.text.next_generation(120);
        self.images.next_generation();
        Frame { width, height, camera, background: self.background(doc), items }
    }

    fn background(&self, doc: &Document) -> Color {
        match doc.get(self.page).map(|n| &n.kind) {
            Some(NodeKind::Page { background: Some(c) }) => paint::color(*c, 1.0),
            _ => Color::from_rgba8(0xf5, 0xf5, 0xf5, 0xff),
        }
    }

    /// World bounds of everything on the page (for "zoom to fit" and exports).
    pub fn page_bounds(&self, doc: &Document) -> Option<Rect> {
        let pk = doc.key(self.page)?;
        let mut r: Option<Rect> = None;
        for k in doc.descendants(pk) {
            if let Some(b) = doc.node(k).and_then(|n| self.index.bounds(n.id)) {
                r = Some(r.map_or(b, |x| x.union(b)));
            }
        }
        r
    }

    /// Bounds of one node (and its subtree) in world space.
    pub fn node_bounds(&self, doc: &Document, id: NodeId) -> Option<Rect> {
        let k = doc.key(id)?;
        let mut r: Option<Rect> = None;
        for d in doc.descendants(k) {
            if let Some(b) = doc.node(d).and_then(|n: &Node| self.index.bounds(n.id)) {
                r = Some(r.map_or(b, |x| x.union(b)));
            }
        }
        r
    }
}

#[cfg(test)]
#[allow(clippy::panic, clippy::type_complexity)]
mod tests {
    use super::*;
    use crate::images::NoAssets;
    use doc::testing::{GenConfig, generate};
    use doc::{Store, TxMeta};
    use serde_json::json;

    fn plan_of(s: &Scene) -> (Vec<FragKey>, Vec<(NodeId, FragKey)>, Vec<NodeId>, Vec<(FragKey, Vec<NodeId>)>) {
        let mut member: Vec<_> = s.member.iter().map(|(k, v)| (*k, *v)).collect();
        member.sort_by_key(|(k, _)| k.to_string());
        let mut split: Vec<_> = s.split.iter().copied().collect();
        split.sort_by_key(|k| k.to_string());
        let mut clips: Vec<_> = s.order.iter().map(|k| (*k, s.frags.get(k).map(|f| f.clips.clone()).unwrap_or_default())).collect();
        clips.sort_by_key(|(k, _)| (k.root.to_string(), k.part as u8));
        (s.order.clone(), member, split, clips)
    }

    #[test]
    fn incremental_inserts_plan_like_a_full_plan() {
        let mut store = Store::new(Document::from_data(generate(&GenConfig { target_nodes: 3000, ..Default::default() })).unwrap_or_else(|e| panic!("{e}")));
        let page = store.doc().pages().iter().filter_map(|&k| store.doc().node(k)).max_by_key(|p| p.children.len()).map(|p| p.id).unwrap_or_else(|| panic!("page"));
        let new_scene = |store: &Store| {
            let mut s = Scene::new(store.doc(), page, FontDb::bundled_only(), Arc::new(NoAssets));
            s.split_threshold = 40;
            s.plan(store.doc());
            s
        };
        let mut scene = new_scene(&store);
        let tree = |n: usize| json!({ "type": "frame", "width": 10.0, "height": 10.0, "children": (0..n).map(|_| json!({ "type": "rectangle" })).collect::<Vec<_>>() });
        // Parents: the page (tail and middle), a whole fragment's inner node, and a split node.
        let doc = store.doc();
        let pk = doc.key(page).unwrap_or_else(|| panic!("page key"));
        let first_screen = doc.node(pk).and_then(|p| p.children.first().copied()).and_then(|k| doc.node(k)).map(|n| n.id).unwrap_or_else(|| panic!("screen"));
        let whole_inner = scene
            .member
            .iter()
            .filter(|(id, k)| k.part == Part::Whole && doc.get(**id).is_some_and(|n| matches!(n.kind, NodeKind::Frame { .. }) && n.parent != Some(pk)))
            .map(|(id, _)| *id)
            .min_by_key(|id| id.to_string())
            .unwrap_or_else(|| panic!("inner"));
        let cases: Vec<(Option<NodeId>, Option<usize>, usize)> = vec![
            (Some(page), None, 3),
            (Some(page), None, 100),
            (Some(page), Some(0), 3),
            (Some(whole_inner), None, 2),
            (Some(whole_inner), None, 60),
            (Some(first_screen), None, 2),
        ];
        for (parent, index, n) in cases {
            let r = store.transact(TxMeta::user("insert"), |tx| tx.insert_json(parent, index, tree(n)).map(|_| ()));
            let (_, changes) = r.unwrap_or_else(|e| panic!("{e}"));
            scene.apply(store.doc(), &changes);
            let fresh = new_scene(&store);
            assert_eq!(plan_of(&scene), plan_of(&fresh), "after inserting {n} under {parent:?} at {index:?}");
        }
    }

    #[test]
    fn text_edits_keep_the_plan() {
        let mut store = Store::new(Document::from_data(generate(&GenConfig { target_nodes: 3000, ..Default::default() })).unwrap_or_else(|e| panic!("{e}")));
        let page = store.doc().pages().iter().filter_map(|&k| store.doc().node(k)).max_by_key(|p| p.children.len()).map(|p| p.id).unwrap_or_else(|| panic!("page"));
        let mut scene = Scene::new(store.doc(), page, FontDb::bundled_only(), Arc::new(NoAssets));
        scene.split_threshold = 40;
        scene.plan(store.doc());
        let doc = store.doc();
        let text = scene.member.keys().copied().filter(|id| doc.get(*id).is_some_and(|n| matches!(n.kind, NodeKind::Text { .. }))).min_by_key(|id| id.to_string()).unwrap_or_else(|| panic!("text"));
        // A full plan would put this entry back.
        let marker = scene.member.keys().copied().find(|id| *id != text).unwrap_or_else(|| panic!("marker"));
        let entry = scene.member.remove(&marker);
        let r = store.transact(TxMeta::user("type"), |tx| tx.edit_spans(text, typed));
        let (_, changes) = r.unwrap_or_else(|e| panic!("{e}"));
        scene.apply(store.doc(), &changes);
        assert!(!scene.member.contains_key(&marker), "typing planned the whole page");
        scene.member.extend(entry.map(|e| (marker, e)));
        let mut fresh = Scene::new(store.doc(), page, FontDb::bundled_only(), Arc::new(NoAssets));
        fresh.split_threshold = 40;
        fresh.plan(store.doc());
        assert_eq!(plan_of(&scene), plan_of(&fresh));
    }

    fn typed(spans: &[doc::model::TextSpan]) -> Vec<doc::model::TextSpan> {
        doc::richtext::replace(spans, 0..0, "x", None)
    }
}
