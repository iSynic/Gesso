//! Hit-testing and the selection rules: what a click, a double-click, Esc and a marquee select.
//!
//! - A click picks the topmost node under the pointer, then climbs to the child of the current
//!   container (the scope, or the page). Top-level frames are see-through: at page level their
//!   children are picked directly. A top-level frame with content is picked by its name label;
//!   its empty area selects nothing (a drag there is a marquee), as in web design tools. An
//!   empty top-level frame is picked anywhere.
//! - Ctrl-click (`deep`) picks the topmost node itself.
//! - Double-click drills one level into a selected container. Instances can be entered: their
//!   content (virtual nodes) is selectable, though edits to it become overrides. The content's
//!   root stands for the instance itself, so it is never selected; the instance is.
//! - Locked nodes and nodes under a locked ancestor are skipped; hidden and clipped-out ones are
//!   not in the index, so they never hit.

use doc::model::StrokeAlign;
use doc::{Document, Node, NodeId, NodeKey, NodeKind};
use index::SpatialIndex;
use kurbo::{ParamCurveNearest, Point, Rect, Shape};
use peniko::Fill;

/// Tolerance for hitting edges, in screen pixels.
pub const HIT_TOLERANCE_PX: f64 = 4.0;

/// Accuracy for nearest-point queries on curves, in local units.
const NEAREST_ACCURACY: f64 = 0.01;

/// The node's position in paint order: child indices from the page down. Later paths paint on
/// top; a descendant's path extends its ancestor's, so children paint over parents.
pub fn paint_path(doc: &Document, key: NodeKey) -> Vec<usize> {
    let mut v: Vec<usize> = doc.ancestors(key).filter_map(|k| doc.index_of(k)).collect();
    v.reverse();
    v.extend(doc.index_of(key));
    v
}

/// The page a node is on (the node itself for a page).
pub fn page_of(doc: &Document, id: NodeId) -> Option<NodeId> {
    let key = doc.key(id)?;
    let top = doc.ancestors(key).last().unwrap_or(key);
    doc.node(top).filter(|n| matches!(n.kind, NodeKind::Page { .. })).map(|n| n.id)
}

/// The id of a node's parent.
pub fn parent_of(doc: &Document, id: NodeId) -> Option<NodeId> {
    let n = doc.get(id)?;
    doc.node(n.parent?).map(|p| p.id)
}

/// The node or one of its ancestors is locked.
pub fn is_locked(doc: &Document, key: NodeKey) -> bool {
    doc.node(key).is_some_and(|n| n.props.locked) || doc.ancestors(key).any(|k| doc.node(k).is_some_and(|n| n.props.locked))
}

/// A frame directly on the page (an artboard).
pub fn is_top_level_frame(doc: &Document, id: NodeId) -> bool {
    let Some(n) = doc.get(id) else { return false };
    matches!(n.kind, NodeKind::Frame { .. } | NodeKind::Component { .. })
        && n.parent.and_then(|p| doc.node(p)).is_some_and(|p| matches!(p.kind, NodeKind::Page { .. }))
}

/// Containers a double-click can enter.
pub fn is_container(n: &Node) -> bool {
    !n.children.is_empty() && matches!(n.kind, NodeKind::Frame { .. } | NodeKind::Group {} | NodeKind::Component { .. } | NodeKind::Instance { .. })
}

/// The root of an instance's content, which stands in for the instance.
fn content_root(doc: &Document, id: NodeId) -> Option<NodeId> {
    let n = doc.get(id)?;
    let parent = doc.node(n.parent?)?;
    (n.virt.is_some() && matches!(parent.kind, NodeKind::Instance { .. })).then_some(parent.id)
}

/// `id`, or the instance when `id` is the root of its content.
pub fn selectable(doc: &Document, id: NodeId) -> NodeId {
    content_root(doc, id).unwrap_or(id)
}

/// The container whose children a drill into `id` picks from: an instance's content root.
pub fn inner(doc: &Document, id: NodeId) -> NodeId {
    let Some(n) = doc.get(id) else { return id };
    match (&n.kind, n.children.first()) {
        (NodeKind::Instance { .. }, Some(&k)) => doc.node(k).map_or(id, |r| r.id),
        _ => id,
    }
}

fn box_contains(n: &Node, p: Point, tol: f64) -> bool {
    p.x >= -tol && p.y >= -tol && p.x <= n.props.width + tol && p.y <= n.props.height + tol
}

fn distance_to(path: &kurbo::BezPath, p: Point) -> f64 {
    path.segments().map(|s| s.nearest(p, NEAREST_ACCURACY).distance_sq).fold(f64::INFINITY, f64::min).sqrt()
}

/// How far a node's visible strokes reach outside its outline (inside: 0).
fn stroke_reach(n: &Node, closed: bool) -> f64 {
    n.props
        .strokes
        .iter()
        .filter(|s| s.paint.visible)
        .map(|s| match (closed, s.align) {
            (false, _) | (true, StrokeAlign::Center) => s.weight / 2.0,
            (true, StrokeAlign::Outside) => s.weight,
            (true, StrokeAlign::Inside) => 0.0,
        })
        .fold(0.0, f64::max)
}

/// Exact test of a world point against one node's own geometry (not its children). `tol` is
/// the edge tolerance in world units.
pub fn hits_node(index: &SpatialIndex, n: &Node, world_pt: Point, tol: f64) -> bool {
    let Some(world) = index.world(n.id) else { return false };
    let p = world.inverse() * world_pt;
    match &n.kind {
        NodeKind::Page { .. } | NodeKind::Group {} => false,
        NodeKind::Text { .. } => box_contains(n, p, tol),
        NodeKind::Frame { .. } | NodeKind::Component { .. } | NodeKind::Instance { .. } | NodeKind::Image { .. } | NodeKind::Note { .. } => {
            box_contains(n, p, tol + stroke_reach(n, true))
        }
        _ => {
            let Some(o) = render::geometry::outline(n) else { return false };
            let has_fill = n.props.fills.iter().any(|f| f.visible);
            let has_stroke = n.props.strokes.iter().any(|s| s.paint.visible);
            if o.closed && (has_fill || !has_stroke) {
                let w = o.path.winding(p);
                let inside = match o.rule {
                    Fill::NonZero => w != 0,
                    Fill::EvenOdd => w % 2 != 0,
                };
                if inside {
                    return true;
                }
            }
            distance_to(&o.path, p) <= tol.max(stroke_reach(n, o.closed))
        }
    }
}

/// The point is inside every clipping ancestor (exactly, so rotated clips work).
fn inside_clips(doc: &Document, index: &SpatialIndex, key: NodeKey, world_pt: Point) -> bool {
    doc.ancestors(key).filter_map(|k| doc.node(k)).filter(|a| index::clips(a)).all(|a| {
        index.world(a.id).is_some_and(|w| box_contains(a, w.inverse() * world_pt, 0.0))
    })
}

/// Every node on `page` whose own geometry is under `pt` (world), topmost first. Locked nodes
/// and nodes outside their clips are left out.
pub fn hit_all(doc: &Document, index: &SpatialIndex, page: NodeId, pt: Point, tol: f64) -> Vec<NodeId> {
    let probe = Rect::from_center_size(pt, (2.0 * tol, 2.0 * tol));
    let mut hits: Vec<(Vec<usize>, NodeId)> = index
        .query(page, probe)
        .filter_map(|id| {
            let key = doc.key(id)?;
            let n = doc.node(key)?;
            (hits_node(index, n, pt, tol) && !is_locked(doc, key) && inside_clips(doc, index, key, pt)).then(|| (paint_path(doc, key), id))
        })
        .collect();
    hits.sort_by(|a, b| b.0.cmp(&a.0));
    hits.into_iter().map(|h| h.1).collect()
}

/// The ancestor chain from `id` up to (not including) the page, nearest first, `id` included.
fn chain(doc: &Document, id: NodeId) -> Vec<NodeId> {
    let Some(key) = doc.key(id) else { return Vec::new() };
    std::iter::once(key)
        .chain(doc.ancestors(key))
        .filter_map(|k| doc.node(k))
        .take_while(|n| !matches!(n.kind, NodeKind::Page { .. }))
        .map(|n| n.id)
        .collect()
}

/// Resolves a hit to what a click selects, given the container (`None` = the page).
pub fn climb(doc: &Document, hit: NodeId, scope: Option<NodeId>) -> Option<NodeId> {
    let c = chain(doc, hit);
    if let Some(s) = scope
        && let Some(i) = c.iter().position(|&x| x == s)
    {
        // The child of the scope on the way to the hit.
        return if i == 0 { Some(s) } else { Some(c[i - 1]) };
    }
    // Page level: top-level frames are see-through. Their empty area selects nothing when they
    // have content, so a drag there draws a marquee over their children; such a frame is
    // selected by its name label (or a marquee from outside it).
    let top = *c.last()?;
    if is_top_level_frame(doc, top) {
        if c.len() >= 2 {
            return Some(c[c.len() - 2]);
        }
        if doc.get(top).is_some_and(|n| !n.children.is_empty()) {
            return None;
        }
    }
    Some(top)
}

/// What a click at `pt` selects. Falls back to page level when the hit is outside the scope.
/// `deep` (Ctrl) picks the topmost node itself.
pub fn pick(doc: &Document, index: &SpatialIndex, page: NodeId, scope: Option<NodeId>, pt: Point, tol: f64, deep: bool) -> Option<NodeId> {
    let hit = *hit_all(doc, index, page, pt, tol).first()?;
    if deep {
        return Some(selectable(doc, hit));
    }
    let in_scope = scope.filter(|&s| chain(doc, hit).contains(&s));
    climb(doc, hit, in_scope).map(|id| selectable(doc, id))
}

/// The scope that makes `id` directly selectable: its parent, or `None` at page level (the
/// page, or a top-level frame, which is see-through).
pub fn scope_for(doc: &Document, id: NodeId) -> Option<NodeId> {
    let p = parent_of(doc, id)?;
    if content_root(doc, p).is_some() {
        return Some(p);
    }
    let n = doc.get(p)?;
    if matches!(n.kind, NodeKind::Page { .. }) || is_top_level_frame(doc, p) { None } else { Some(p) }
}

/// Double-click on a selected container: the child of `container` under the pointer.
pub fn drill(doc: &Document, index: &SpatialIndex, page: NodeId, container: NodeId, pt: Point, tol: f64) -> Option<NodeId> {
    let container = inner(doc, container);
    hit_all(doc, index, page, pt, tol).into_iter().find_map(|h| {
        let c = chain(doc, h);
        let i = c.iter().position(|&x| x == container)?;
        (i > 0).then(|| c[i - 1])
    })
}

/// The marquee's container: the scope, or at page level the topmost top-level frame the drag
/// started in (then its children are selected), or the page.
pub fn marquee_container(doc: &Document, index: &SpatialIndex, page: NodeId, scope: Option<NodeId>, start: Point) -> NodeId {
    if let Some(s) = scope {
        return s;
    }
    let Some(pn) = doc.get(page) else { return page };
    pn.children
        .iter()
        .rev()
        .filter_map(|&k| doc.node(k))
        .find(|n| is_top_level_frame(doc, n.id) && !n.props.locked && hits_node(index, n, start, 0.0))
        .map_or(page, |n| n.id)
}

/// Children of `container` whose visible bounds intersect `rect` (world), in tree order.
/// Locked and hidden children are skipped. Lines (zero-height boxes) count when touched.
pub fn marquee(doc: &Document, index: &SpatialIndex, container: NodeId, rect: Rect) -> Vec<NodeId> {
    let Some(c) = doc.get(container) else { return Vec::new() };
    c.children
        .iter()
        .filter_map(|&k| doc.node(k))
        .filter(|n| !n.props.locked)
        .filter(|n| index.bounds(n.id).is_some_and(|b| b.x0 <= rect.x1 && b.x1 >= rect.x0 && b.y0 <= rect.y1 && b.y1 >= rect.y0))
        .map(|n| n.id)
        .collect()
}

/// World bounds of several nodes (their indexed visual bounds).
pub fn union_bounds(index: &SpatialIndex, ids: &[NodeId]) -> Option<Rect> {
    ids.iter().filter_map(|&id| index.bounds(id)).reduce(|a, b| a.union(b))
}

/// The rectangle a drag from `a` to `b` spans.
pub fn drag_rect(a: Point, b: Point) -> Rect {
    Rect::from_points(a, b)
}

/// Edge tolerance in world units at a zoom.
pub fn tolerance(zoom: f64) -> f64 {
    HIT_TOLERANCE_PX / zoom.max(1e-6)
}
