//! Move, resize and rotate gestures, nudging, and reorder/reparent on drop.
//!
//! A gesture records each node's placement when it starts and rewrites positions from the total
//! pointer delta on every update, as one coalesced transaction per frame, so the whole gesture
//! is one undo step and rounding never accumulates.

use std::sync::atomic::{AtomicU64, Ordering};

use doc::model::{LayoutDirection, Sizing};
use doc::{Document, NodeId, NodeKind, PropValue, TxMeta};
use index::SpatialIndex;
use kurbo::{Affine, Point, Rect, Vec2};
use rustc_hash::FxHashSet;

use crate::camera::Camera;
use crate::select;
use crate::session::{Error, Session};
use crate::snap::{self, Feedback, Moving, Targets};
use crate::xform::{Placement, angle_of, geometric_bounds, is_mirrored, linear, normalize_deg, parent_world};

static GESTURE: AtomicU64 = AtomicU64::new(0);

fn coalesce_key(what: &str) -> String {
    format!("{what}#{}", GESTURE.fetch_add(1, Ordering::Relaxed))
}

/// Modifier keys during a gesture.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    /// Keep proportions / snap angles.
    pub shift: bool,
    /// From the centre (resize) / duplicate (move) / measure to hovered.
    pub alt: bool,
    /// Suppress reparenting.
    pub ctrl: bool,
}

/// Resize handles, named by compass direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Handle {
    N,
    NE,
    E,
    SE,
    S,
    SW,
    W,
    NW,
}

impl Handle {
    pub const ALL: [Handle; 8] = [Handle::N, Handle::NE, Handle::E, Handle::SE, Handle::S, Handle::SW, Handle::W, Handle::NW];
    pub const CORNERS: [Handle; 4] = [Handle::NE, Handle::SE, Handle::SW, Handle::NW];

    /// Which edges move: -1 left/top, 1 right/bottom, 0 none.
    pub fn sides(self) -> (i8, i8) {
        match self {
            Handle::N => (0, -1),
            Handle::NE => (1, -1),
            Handle::E => (1, 0),
            Handle::SE => (1, 1),
            Handle::S => (0, 1),
            Handle::SW => (-1, 1),
            Handle::W => (-1, 0),
            Handle::NW => (-1, -1),
        }
    }

    pub fn is_corner(self) -> bool {
        let (x, y) = self.sides();
        x != 0 && y != 0
    }

    /// The handle's point on a `w`×`h` local box.
    pub fn local_point(self, w: f64, h: f64) -> Point {
        let (x, y) = self.sides();
        Point::new((f64::from(x) + 1.0) / 2.0 * w, (f64::from(y) + 1.0) / 2.0 * h)
    }
}

/// The box the handles sit on: one node's rotated box, or a multi-selection's world bounds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SelBox {
    /// Local box → world.
    pub world: Affine,
    pub w: f64,
    pub h: f64,
}

impl SelBox {
    pub fn handle(&self, h: Handle) -> Point {
        self.world * h.local_point(self.w, self.h)
    }

    pub fn centre(&self) -> Point {
        self.world * Point::new(self.w / 2.0, self.h / 2.0)
    }

    /// Corners in order NW, NE, SE, SW (world).
    pub fn corners(&self) -> [Point; 4] {
        [Handle::NW, Handle::NE, Handle::SE, Handle::SW].map(|h| self.handle(h))
    }

    /// The box is axis-aligned in world space (snapping applies).
    pub fn axis_aligned(&self) -> bool {
        let c = self.world.as_coeffs();
        c[1].abs() < 1e-9 && c[2].abs() < 1e-9 && c[0] > 0.0 && c[3] > 0.0
    }
}

/// Selected nodes that move as units: no ancestor also selected, not locked.
pub fn top_items(doc: &Document, ids: &[NodeId]) -> Vec<NodeId> {
    let set: FxHashSet<NodeId> = ids.iter().copied().collect();
    ids.iter()
        .copied()
        .filter(|&id| {
            let Some(k) = doc.key(id) else { return false };
            !select::is_locked(doc, k) && !doc.ancestors(k).filter_map(|a| doc.node(a)).any(|a| set.contains(&a.id))
        })
        .collect()
}

pub fn selection_box(s: &Session) -> Option<SelBox> {
    selection_box_of(s, s.selection())
}

/// The handle box for `ids`.
pub fn selection_box_of(s: &Session, ids: &[NodeId]) -> Option<SelBox> {
    let doc = s.doc();
    let ids = top_items(doc, ids);
    match ids.as_slice() {
        [] => None,
        [one] => {
            let n = doc.get(*one)?;
            Some(SelBox { world: s.scene.index.world(*one)?, w: n.props.width, h: n.props.height })
        }
        many => {
            let r = many.iter().filter_map(|&id| doc.get(id).and_then(|n| geometric_bounds(&s.scene.index, n))).reduce(|a, b| a.union(b))?;
            Some(SelBox { world: Affine::translate(r.origin().to_vec2()), w: r.width(), h: r.height() })
        }
    }
}

/// What the pointer is over on the selection box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grip {
    Resize(Handle),
    /// Just outside a corner.
    Rotate(Handle),
}

/// Handle radius and the rotation zone's outer radius, in screen pixels.
pub const HANDLE_PX: f64 = 5.0;
pub const ROTATE_PX: f64 = 20.0;

/// The grip under a screen point, if any. Handles first, then the edges, then the rotation zones
/// outside the corners.
pub fn grip_at(b: &SelBox, cam: &Camera, screen: Point) -> Option<Grip> {
    let sp = |h: Handle| cam.to_screen(b.handle(h));
    if let Some(h) = Handle::CORNERS.iter().copied().find(|&h| sp(h).distance(screen) <= HANDLE_PX + 1.0) {
        return Some(Grip::Resize(h));
    }
    // Edge handles are only shown, and grabbed, when the box is big enough on screen.
    let big = |a: Handle, c: Handle| sp(a).distance(sp(c)) > 4.0 * HANDLE_PX;
    for (edge, a, c) in [(Handle::N, Handle::NW, Handle::NE), (Handle::E, Handle::NE, Handle::SE), (Handle::S, Handle::SE, Handle::SW), (Handle::W, Handle::SW, Handle::NW)] {
        if big(a, c) && dist_to_segment(screen, sp(a), sp(c)) <= HANDLE_PX - 1.0 {
            return Some(Grip::Resize(edge));
        }
    }
    let local = (cam.affine() * b.world).inverse() * screen;
    let inside = local.x >= 0.0 && local.y >= 0.0 && local.x <= b.w && local.y <= b.h;
    if !inside && let Some(h) = Handle::CORNERS.iter().copied().find(|&h| sp(h).distance(screen) <= ROTATE_PX) {
        return Some(Grip::Rotate(h));
    }
    None
}

fn dist_to_segment(p: Point, a: Point, b: Point) -> f64 {
    let ab = b - a;
    let t = if ab.hypot2() > 0.0 { ((p - a).dot(ab) / ab.hypot2()).clamp(0.0, 1.0) } else { 0.0 };
    p.distance(a + ab * t)
}

/// One moving node, as it was when the gesture started.
#[derive(Debug, Clone)]
struct Item {
    id: NodeId,
    parent: NodeId,
    parent_world: Affine,
    p0: Placement,
    world0: Affine,
    /// Placed by its parent's auto layout.
    flow: bool,
    /// A path's shape at the start: resizing scales from this, so a drag through zero loses
    /// nothing.
    path0: Option<NodeKind>,
}

fn items(s: &Session, ids: &[NodeId]) -> Vec<Item> {
    let doc = s.doc();
    let index = &s.scene.index;
    top_items(doc, ids)
        .into_iter()
        .filter_map(|id| {
            let n = doc.get(id)?;
            let parent = select::parent_of(doc, id)?;
            let flow = is_flow_child(doc, id);
            Some(Item { id, parent, parent_world: parent_world(index, doc, id), p0: Placement::of(n), path0: matches!(n.kind, NodeKind::Path { .. }).then(|| n.kind.clone()), world0: index.world(id)?, flow })
        })
        .collect()
}

/// The node is laid out by its parent's auto layout.
pub fn is_flow_child(doc: &Document, id: NodeId) -> bool {
    let Some(n) = doc.get(id) else { return false };
    !n.props.layout_child.absolute && n.parent.and_then(|p| doc.node(p)).is_some_and(|p| matches!(p.kind, NodeKind::Frame { auto_layout: Some(_), .. } | NodeKind::Component { auto_layout: Some(_), .. }))
}

fn is_frame_like(kind: &NodeKind) -> bool {
    matches!(kind, NodeKind::Frame { .. } | NodeKind::Component { .. })
}

/// Nearest frame or page at or above `id`: what "the container" means for reparenting (groups
/// are not drop targets).
fn container_of(doc: &Document, id: NodeId) -> Option<NodeId> {
    let k = doc.key(id)?;
    std::iter::once(k).chain(doc.ancestors(k)).filter_map(|k| doc.node(k)).find(|n| is_frame_like(&n.kind) || matches!(n.kind, NodeKind::Page { .. })).map(|n| n.id)
}

/// The topmost frame under `pt` that isn't one of `moving` or inside one, or the page.
pub fn drop_target(doc: &Document, index: &SpatialIndex, page: NodeId, pt: Point, moving: &[NodeId]) -> NodeId {
    let set: FxHashSet<NodeId> = moving.iter().copied().collect();
    select::hit_all(doc, index, page, pt, 0.0)
        .into_iter()
        .find(|&id| {
            let Some(k) = doc.key(id) else { return false };
            let inside_moving = set.contains(&id) || doc.ancestors(k).filter_map(|a| doc.node(a)).any(|a| set.contains(&a.id));
            !inside_moving && doc.node(k).is_some_and(|n| is_frame_like(&n.kind))
        })
        .unwrap_or(page)
}

/// Where a drop at `pt` would insert into the auto-layout `parent`: an index into the parent's
/// children with `dragged` taken out, and the insertion line (world).
pub fn reorder_slot(doc: &Document, index: &SpatialIndex, parent: NodeId, dragged: &[NodeId], pt: Point) -> Option<(usize, (Point, Point))> {
    let p = doc.get(parent)?;
    let al = match &p.kind {
        NodeKind::Frame { auto_layout: Some(al), .. } | NodeKind::Component { auto_layout: Some(al), .. } => al,
        _ => return None,
    };
    let pw = index.world(parent)?;
    let lp = pw.inverse() * pt;
    let rest: Vec<&doc::Node> = p.children.iter().filter_map(|&k| doc.node(k)).filter(|n| !dragged.contains(&n.id)).collect();
    let horizontal = al.direction != LayoutDirection::Vertical;
    let flow = rest.iter().enumerate().filter(|(_, n)| !n.props.layout_child.absolute);
    let nearest = flow
        .map(|(i, n)| {
            let r = Rect::new(n.props.x, n.props.y, n.props.x + n.props.width, n.props.y + n.props.height);
            let dx = (r.x0 - lp.x).max(lp.x - r.x1).max(0.0);
            let dy = (r.y0 - lp.y).max(lp.y - r.y1).max(0.0);
            (i, r, dx.hypot(dy))
        })
        .min_by(|a, b| a.2.total_cmp(&b.2));
    let [top, right, bottom, left] = al.padding;
    let (i, a, b) = match nearest {
        Some((i, r, _)) => {
            let before = if horizontal { lp.x < r.center().x } else { lp.y < r.center().y };
            let g = al.gap / 2.0;
            let at = if horizontal { if before { r.x0 - g } else { r.x1 + g } } else if before { r.y0 - g } else { r.y1 + g };
            let (a, b) = if horizontal { (Point::new(at, r.y0), Point::new(at, r.y1)) } else { (Point::new(r.x0, at), Point::new(r.x1, at)) };
            (i + usize::from(!before), a, b)
        }
        None => {
            let (w, h) = (p.props.width, p.props.height);
            let (a, b) = if horizontal { (Point::new(left, top), Point::new(left, h - bottom)) } else { (Point::new(left, top), Point::new(w - right, top)) };
            (rest.len(), a, b)
        }
    };
    Some((i, (pw * a, pw * b)))
}

/// Moves `dragged` (in order) into `parent` just before what is at `slot` among the children
/// without them.
pub(crate) fn apply_reorder(tx: &mut doc::Tx<'_>, parent: NodeId, dragged: &[NodeId], slot: usize) -> Result<(), doc::Error> {
    let rest: Vec<NodeId> = {
        let d = tx.doc();
        d.get(parent).map(|p| p.children.iter().filter_map(|&k| d.node(k)).map(|n| n.id).filter(|id| !dragged.contains(id)).collect()).unwrap_or_default()
    };
    for &id in dragged {
        let index = match rest.get(slot) {
            Some(anchor) => {
                let d = tx.doc();
                let kids: Vec<NodeId> = d.get(parent).map(|p| p.children.iter().filter_map(|&k| d.node(k)).map(|n| n.id).filter(|&c| c != id).collect()).unwrap_or_default();
                kids.iter().position(|c| c == anchor)
            }
            None => None,
        };
        tx.move_node(id, Some(parent), index)?;
    }
    Ok(())
}

fn write(tx: &mut doc::Tx<'_>, id: NodeId, updates: Vec<PropValue>) -> Result<(), doc::Error> {
    for u in updates {
        tx.update(id, u)?;
    }
    Ok(())
}

fn threshold(s: &Session) -> f64 {
    snap::SNAP_PX / s.camera.zoom.max(1e-6)
}

fn targets_for(s: &Session, items: &[Item]) -> Targets {
    let Some(first) = items.first() else { return Targets::default() };
    let parent = if items.iter().all(|i| i.parent == first.parent) { first.parent } else { s.page() };
    let exclude: FxHashSet<NodeId> = items.iter().map(|i| i.id).collect();
    Targets::collect(s.doc(), &s.scene.index, s.page(), parent, &exclude, s.view())
}

// ---- move -------------------------------------------------------------------------------------

pub struct MoveGesture {
    key: String,
    items: Vec<Item>,
    start: Point,
    bounds0: Rect,
    targets: Targets,
    /// All items are flow children of this auto-layout frame.
    flow_parent: Option<NodeId>,
    delta: Vec2,
    pointer: Point,
    reorder: Option<(NodeId, usize)>,
    target: Option<NodeId>,
    committed: bool,
    pub feedback: Feedback,
}

impl MoveGesture {
    /// Starts moving the selection from `start` (world). With `duplicate` (Alt), the selection
    /// is copied first and the copies move. `None` if nothing movable is selected.
    pub fn begin(s: &mut Session, start: Point, duplicate: bool) -> Result<Option<Self>, Error> {
        let key = coalesce_key("move");
        let mut ids = top_items(s.doc(), s.selection());
        if ids.is_empty() {
            return Ok(None);
        }
        let mut committed = false;
        if duplicate {
            let copies = s.transact(TxMeta::user("Duplicate").coalesce(key.clone()), |tx| {
                let mut out = Vec::new();
                for &id in &ids {
                    let d = tx.doc();
                    let Some(k) = d.key(id) else { continue };
                    let parent = select::parent_of(d, id);
                    let at = d.index_of(k).map(|i| i + 1);
                    out.push(tx.copy(id, parent, at, None)?);
                }
                Ok(out)
            })?;
            s.select(copies.clone());
            ids = copies;
            committed = true;
        }
        let items = items(s, &ids);
        let doc = s.doc();
        let bounds0 = items.iter().filter_map(|i| doc.get(i.id).and_then(|n| geometric_bounds(&s.scene.index, n))).reduce(|a, b| a.union(b)).unwrap_or(Rect::from_points(start, start));
        let flow_parent = items.first().filter(|f| items.iter().all(|i| i.flow && i.parent == f.parent)).map(|f| f.parent);
        let targets = targets_for(s, &items);
        Ok(Some(MoveGesture { key, items, start, bounds0, targets, flow_parent, delta: Vec2::ZERO, pointer: start, reorder: None, target: None, committed, feedback: Feedback::default() }))
    }

    pub fn ids(&self) -> Vec<NodeId> {
        self.items.iter().map(|i| i.id).collect()
    }

    pub fn update(&mut self, s: &mut Session, pointer: Point, mods: Mods) -> Result<&Feedback, Error> {
        self.pointer = pointer;
        let raw = pointer - self.start;
        let ids = self.ids();
        self.reorder = None;
        self.target = None;
        if let Some(parent) = self.flow_parent {
            // Flow children stay in their slots; show where they would go.
            self.delta = raw;
            let inside = s.doc().get(parent).and_then(|p| geometric_bounds(&s.scene.index, p)).is_some_and(|r| r.contains(pointer));
            let mut fb = Feedback::default();
            if inside || mods.ctrl {
                if let Some((slot, line)) = reorder_slot(s.doc(), &s.scene.index, parent, &ids, pointer) {
                    self.reorder = Some((parent, slot));
                    fb.reorder = Some(line);
                }
            } else {
                let t = drop_target(s.doc(), &s.scene.index, s.page(), pointer, &ids);
                self.target = Some(t);
                fb.target = (t != s.page()).then_some(t);
            }
            self.feedback = fb;
            return Ok(&self.feedback);
        }
        let thr = threshold(s);
        let (adj, mut fb) = if s.snap_to_objects || s.snap_to_pixels {
            let t = if s.snap_to_objects { self.targets.clone() } else { Targets::default() };
            snap::snap_rect(self.bounds0 + raw, &t, thr, s.snap_to_pixels, Moving::default())
        } else {
            (Vec2::ZERO, Feedback::default())
        };
        self.delta = raw + adj;
        let delta = self.delta;
        let items = self.items.clone();
        s.transact(TxMeta::user("Move").coalesce(self.key.clone()), |tx| {
            for it in &items {
                let dp = linear(it.parent_world.inverse(), delta);
                let p = Placement { x: it.p0.x + dp.x, y: it.p0.y + dp.y, ..it.p0 };
                write(tx, it.id, p.diff(&Placement::of(tx.doc().get(it.id).ok_or(doc::Error::Input("node went away".into()))?)))?;
            }
            Ok(())
        })?;
        self.committed = true;
        if !mods.ctrl {
            let doc = s.doc();
            let t = drop_target(doc, &s.scene.index, s.page(), pointer, &ids);
            let changes = items.iter().any(|it| container_of(doc, it.parent) != Some(t));
            if changes {
                self.target = Some(t);
                fb.target = (t != s.page()).then_some(t);
                if let Some((_, line)) = reorder_slot(doc, &s.scene.index, t, &ids, pointer) {
                    fb.reorder = Some(line);
                }
            }
        }
        self.feedback = fb;
        Ok(&self.feedback)
    }

    /// Ends the gesture: commits a reorder or a reparent if one is pending.
    pub fn finish(self, s: &mut Session) -> Result<(), Error> {
        let ids = self.ids();
        if let Some((parent, slot)) = self.reorder {
            s.transact(TxMeta::user("Reorder").coalesce(self.key.clone()), |tx| apply_reorder(tx, parent, &ids, slot))?;
        } else if let Some(target) = self.target {
            let index = &s.scene.index;
            let target_world = index.world(target).unwrap_or(Affine::IDENTITY);
            let slot = reorder_slot(s.doc(), index, target, &ids, self.pointer).map(|r| r.0);
            let delta = self.delta;
            let placed: Vec<(NodeId, Placement)> = self
                .items
                .iter()
                .map(|it| {
                    let world = Affine::translate(delta) * it.world0;
                    (it.id, Placement::from_affine(target_world.inverse() * world, it.p0.w, it.p0.h, &it.p0))
                })
                .collect();
            s.transact(TxMeta::user("Move into frame").coalesce(self.key.clone()), |tx| {
                match slot {
                    Some(slot) => apply_reorder(tx, target, &ids, slot)?,
                    None => {
                        for &id in &ids {
                            tx.move_node(id, Some(target), None)?;
                        }
                    }
                }
                for (id, p) in &placed {
                    let now = Placement::of(tx.doc().get(*id).ok_or(doc::Error::Input("node went away".into()))?);
                    write(tx, *id, p.diff(&now))?;
                }
                Ok(())
            })?;
            s.select(ids);
        }
        Ok(())
    }

    /// Abandons the gesture (Esc), undoing what it changed.
    pub fn cancel(self, s: &mut Session) -> Result<(), Error> {
        if self.committed {
            s.undo()?;
        }
        Ok(())
    }
}

// ---- resize -----------------------------------------------------------------------------------

pub struct ResizeGesture {
    /// Scale every item's box relative to the handle box (multi-selection or groups), rather
    /// than resizing one node in its own frame.
    scale: bool,
    key: String,
    handle: Handle,
    items: Vec<Item>,
    box0: SelBox,
    targets: Targets,
    committed: bool,
    pub feedback: Feedback,
}

/// New extent along one axis of a box `0..len` for a handle on side `side`.
fn extent(side: i8, p: f64, len: f64, centred: bool) -> (f64, f64) {
    match (side, centred) {
        (0, _) => (0.0, len),
        (1, false) => (0.0, p),
        (-1, false) => (p, len),
        (1, true) => (len - p, p),
        (_, _) => (p, len - p),
    }
}

impl ResizeGesture {
    pub fn begin(s: &Session, handle: Handle) -> Option<Self> {
        let box0 = selection_box(s)?;
        let sel = items(s, s.selection());
        if sel.is_empty() {
            return None;
        }
        let targets = targets_for(s, &sel);
        // A group's box is derived from its children, so resizing a group scales its children.
        let doc = s.doc();
        let mut ids = Vec::new();
        let mut stack: Vec<NodeId> = sel.iter().rev().map(|i| i.id).collect();
        let mut grouped = false;
        while let Some(id) = stack.pop() {
            match doc.get(id) {
                Some(n) if matches!(n.kind, NodeKind::Group {}) && !n.children.is_empty() => {
                    grouped = true;
                    stack.extend(n.children.iter().rev().filter_map(|&k| doc.node(k)).map(|c| c.id));
                }
                Some(_) => ids.push(id),
                None => {}
            }
        }
        let items = if grouped { items(s, &ids) } else { sel };
        let scale = grouped || items.len() > 1;
        Some(ResizeGesture {
            scale, key: coalesce_key("resize"), handle, items, box0, targets, committed: false, feedback: Feedback::default() })
    }

    pub fn update(&mut self, s: &mut Session, pointer: Point, mods: Mods) -> Result<&Feedback, Error> {
        let (sx, sy) = self.handle.sides();
        let (w, h) = (self.box0.w, self.box0.h);
        let mut pointer = pointer;
        self.feedback = Feedback::default();
        if self.box0.axis_aligned() && (s.snap_to_objects || s.snap_to_pixels) {
            let t = if s.snap_to_objects { self.targets.clone() } else { Targets::default() };
            let (p, fb) = snap::snap_point(pointer, &t, threshold(s), s.snap_to_pixels, (sx != 0, sy != 0));
            pointer = p;
            self.feedback = fb;
        }
        let lp = self.box0.world.inverse() * pointer;
        let (mut x0, mut x1) = extent(sx, lp.x, w, mods.alt);
        let (mut y0, mut y1) = extent(sy, lp.y, h, mods.alt);
        if mods.shift && w > 1e-9 && h > 1e-9 {
            let (fx, fy) = ((x1 - x0) / w, (y1 - y0) / h);
            let k = match (sx, sy) {
                (0, _) => fy.abs(),
                (_, 0) => fx.abs(),
                _ => fx.abs().max(fy.abs()),
            };
            let fit = |side: i8, a: f64, b: f64, len: f64, f: f64| -> (f64, f64) {
                let n = k * len * if f < 0.0 { -1.0 } else { 1.0 };
                match (side, mods.alt) {
                    (0, _) | (_, true) => {
                        let c = if side == 0 { len / 2.0 } else { (a + b) / 2.0 };
                        (c - n / 2.0, c + n / 2.0)
                    }
                    (1, false) => (a, a + n),
                    _ => (b - n, b),
                }
            };
            (x0, x1) = fit(sx, x0, x1, w, fx);
            (y0, y1) = fit(sy, y0, y1, h, fy);
        }
        let single = !self.scale;
        let items = self.items.clone();
        let box0 = self.box0;
        s.transact(TxMeta::user("Resize").coalesce(self.key.clone()), |tx| {
            for it in &items {
                let new = if single {
                    let c = it.p0.local() * Point::new((x0 + x1) / 2.0, (y0 + y1) / 2.0);
                    Placement { w: (x1 - x0).abs(), h: (y1 - y0).abs(), flip_x: it.p0.flip_x ^ (x1 < x0), flip_y: it.p0.flip_y ^ (y1 < y0), ..it.p0 }.with_centre(c)
                } else {
                    // Scale each node's box about the selection's anchor; rotations are kept.
                    let (nx0, nx1) = (x0.min(x1), x0.max(x1));
                    let (ny0, ny1) = (y0.min(y1), y0.max(y1));
                    let kx = if w > 1e-9 { (nx1 - nx0) / w } else { 1.0 };
                    let ky = if h > 1e-9 { (ny1 - ny0) / h } else { 1.0 };
                    let o = box0.world * Point::ZERO;
                    let c0 = it.world0 * Point::new(it.p0.w / 2.0, it.p0.h / 2.0);
                    let c1 = Point::new(o.x + nx0 + (c0.x - o.x) * kx, o.y + ny0 + (c0.y - o.y) * ky);
                    let th = angle_of(it.world0).to_radians();
                    let (cs, sn) = (th.cos().abs(), th.sin().abs());
                    let nw = it.p0.w * (kx * cs).hypot(ky * sn);
                    let nh = it.p0.h * (kx * sn).hypot(ky * cs);
                    Placement { w: nw, h: nh, ..it.p0 }.with_centre(it.parent_world.inverse() * c1)
                };
                let n = tx.doc().get(it.id).ok_or(doc::Error::Input("node went away".into()))?;
                let mut ups = new.diff(&Placement::of(n));
                if (new.w - it.p0.w).abs() > 1e-9 && n.props.sizing_h != Sizing::Fixed {
                    ups.push(PropValue::SizingH(Sizing::Fixed));
                }
                if (new.h - it.p0.h).abs() > 1e-9 && n.props.sizing_v != Sizing::Fixed {
                    ups.push(PropValue::SizingV(Sizing::Fixed));
                }
                let ratio = |new: f64, old: f64| if old > 1e-9 { new.max(1e-3) / old } else { 1.0 };
                if let Some(kind) = it.path0.as_ref().and_then(|k| k.scaled_path(ratio(new.w, it.p0.w), ratio(new.h, it.p0.h))) {
                    ups.push(PropValue::Kind(kind));
                }
                write(tx, it.id, ups)?;
            }
            Ok(())
        })?;
        self.committed = true;
        if let Some(b) = selection_box(s).filter(SelBox::axis_aligned) {
            let r = Rect::from_points(b.world * Point::ZERO, b.world * Point::new(b.w, b.h));
            self.feedback.measures = snap::measures(r, &self.targets);
        }
        Ok(&self.feedback)
    }

    pub fn cancel(self, s: &mut Session) -> Result<(), Error> {
        if self.committed {
            s.undo()?;
        }
        Ok(())
    }
}

// ---- rotate -----------------------------------------------------------------------------------

pub struct RotateGesture {
    key: String,
    items: Vec<Item>,
    centre: Point,
    a0: f64,
    committed: bool,
    /// The rotation applied so far, degrees.
    pub angle: f64,
}

impl RotateGesture {
    pub fn begin(s: &Session, start: Point) -> Option<Self> {
        let b = selection_box(s)?;
        let items = items(s, s.selection());
        if items.is_empty() {
            return None;
        }
        let centre = b.centre();
        let a0 = (start - centre).atan2().to_degrees();
        Some(RotateGesture { key: coalesce_key("rotate"), items, centre, a0, committed: false, angle: 0.0 })
    }

    pub fn update(&mut self, s: &mut Session, pointer: Point, mods: Mods) -> Result<(), Error> {
        let mut d = normalize_deg((pointer - self.centre).atan2().to_degrees() - self.a0);
        if mods.shift {
            d = match self.items.as_slice() {
                // Snap the node's resulting world angle.
                [one] => {
                    let world = angle_of(one.world0);
                    (((world + d) / 15.0).round() * 15.0) - world
                }
                _ => (d / 15.0).round() * 15.0,
            };
        }
        self.angle = d;
        let items = self.items.clone();
        let centre = self.centre;
        s.transact(TxMeta::user("Rotate").coalesce(self.key.clone()), |tx| {
            for it in &items {
                let sign = if is_mirrored(it.parent_world) { -1.0 } else { 1.0 };
                let c0 = it.world0 * Point::new(it.p0.w / 2.0, it.p0.h / 2.0);
                let c1 = Affine::rotate_about(d.to_radians(), centre) * c0;
                let new = Placement { rotation: normalize_deg(it.p0.rotation + sign * d), ..it.p0 }.with_centre(it.parent_world.inverse() * c1);
                let n = tx.doc().get(it.id).ok_or(doc::Error::Input("node went away".into()))?;
                write(tx, it.id, new.diff(&Placement::of(n)))?;
            }
            Ok(())
        })?;
        self.committed = true;
        Ok(())
    }

    pub fn cancel(self, s: &mut Session) -> Result<(), Error> {
        if self.committed {
            s.undo()?;
        }
        Ok(())
    }
}

// ---- nudge ------------------------------------------------------------------------------------

/// Moves the selection by a world delta (arrow keys). Flow children of auto layout move one
/// slot instead, along the layout direction.
pub fn nudge(s: &mut Session, delta: Vec2) -> Result<(), Error> {
    let items = items(s, s.selection());
    if items.is_empty() {
        return Ok(());
    }
    s.transact(TxMeta::user("Nudge"), |tx| {
        for it in &items {
            if it.flow {
                let d = tx.doc();
                let Some(k) = d.key(it.id) else { continue };
                let (Some(i), Some(len)) = (d.index_of(k), d.get(it.parent).map(|p| p.children.len())) else { continue };
                let step = if delta.x + delta.y > 0.0 { 1 } else { -1 };
                let j = (i as i64 + step).clamp(0, len as i64 - 1) as usize;
                if j != i {
                    tx.move_node(it.id, Some(it.parent), Some(j))?;
                }
                continue;
            }
            let dp = linear(it.parent_world.inverse(), delta);
            tx.update(it.id, PropValue::X(it.p0.x + dp.x))?;
            tx.update(it.id, PropValue::Y(it.p0.y + dp.y))?;
        }
        Ok(())
    })?;
    Ok(())
}
