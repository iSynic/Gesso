//! Snapping for move, resize and drawing: edges and centres of neighbours and the parent (smart
//! guides), equal spacing between neighbours, the pixel grid, and the distance labels shown
//! while dragging. Everything is in world space on axis-aligned bounds.

use doc::{Document, NodeId, NodeKind};
use index::SpatialIndex;
use kurbo::{Point, Rect, Vec2};
use rustc_hash::FxHashSet;

use crate::xform::geometric_bounds;

/// Snap distance in screen pixels.
pub const SNAP_PX: f64 = 6.0;

/// Above this many siblings, candidates come from an index query over the view instead of a
/// scan of the parent's children.
const SCAN_LIMIT: usize = 2_000;

const EPS: f64 = 1e-6;

/// A smart-guide line (world).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Guide {
    pub a: Point,
    pub b: Point,
}

/// A measured distance (world segment and its length).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Measure {
    pub a: Point,
    pub b: Point,
    pub value: f64,
}

/// What to draw while a gesture runs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Feedback {
    pub guides: Vec<Guide>,
    /// Equal gaps (each one marked).
    pub gaps: Vec<Measure>,
    /// Distances to the nearest neighbour on each side.
    pub measures: Vec<Measure>,
    /// Insertion line for a reorder inside auto layout.
    pub reorder: Option<(Point, Point)>,
    /// Frame the dragged nodes would move into on drop.
    pub target: Option<NodeId>,
}

/// Candidates to snap against.
#[derive(Debug, Clone, Default)]
pub struct Targets {
    pub rects: Vec<Rect>,
    /// The parent frame's box, when it is one.
    pub parent: Option<Rect>,
}

impl Targets {
    /// Siblings under `parent` (other than `exclude`) whose boxes are near `view`, and the parent.
    pub fn collect(doc: &Document, index: &SpatialIndex, page: NodeId, parent: NodeId, exclude: &FxHashSet<NodeId>, view: Rect) -> Targets {
        let Some(pk) = doc.key(parent) else { return Targets::default() };
        let Some(p) = doc.node(pk) else { return Targets::default() };
        let near = view.inflate(view.width() * 0.5, view.height() * 0.5);
        let ok = |n: &doc::Node| !exclude.contains(&n.id) && n.props.visible;
        let mut rects: Vec<Rect> = if p.children.len() <= SCAN_LIMIT {
            p.children.iter().filter_map(|&k| doc.node(k)).filter(|n| ok(n)).filter_map(|n| geometric_bounds(index, n)).filter(|r| r.overlaps(near)).collect()
        } else {
            index
                .query(page, near)
                .filter_map(|id| doc.get(id))
                .filter(|n| n.parent == Some(pk) && ok(n))
                .filter_map(|n| geometric_bounds(index, n))
                .collect()
        };
        let parent = (!matches!(p.kind, NodeKind::Page { .. })).then(|| geometric_bounds(index, p)).flatten();
        rects.extend(parent);
        Targets { rects, parent }
    }

    pub fn neighbours(&self) -> impl Iterator<Item = &Rect> {
        self.rects.iter().filter(move |r| Some(**r) != self.parent)
    }
}

/// Swaps x and y, so one implementation serves both axes.
fn t(r: Rect) -> Rect {
    Rect::new(r.y0, r.x0, r.y1, r.x1)
}

fn tp(p: Point) -> Point {
    Point::new(p.y, p.x)
}

fn stops(r: Rect) -> [f64; 3] {
    [r.x0, (r.x0 + r.x1) / 2.0, r.x1]
}

/// Best edge/centre alignment along x: (adjustment, distance).
fn align_x(m: Rect, targets: &[Rect], only_edges: Option<&[f64]>, thr: f64) -> Option<(f64, f64)> {
    let mine: Vec<f64> = only_edges.map_or_else(|| stops(m).to_vec(), <[f64]>::to_vec);
    let mut best: Option<(f64, f64)> = None;
    for r in targets {
        for tv in stops(*r) {
            for &mv in &mine {
                let d = (tv - mv).abs();
                if d <= thr && best.is_none_or(|b| d < b.1) {
                    best = Some((tv - mv, d));
                }
            }
        }
    }
    best
}

/// Rects overlapping `m` across the axis (same row for x).
fn in_row<'a>(m: Rect, targets: impl Iterator<Item = &'a Rect>) -> Vec<Rect> {
    let mut v: Vec<Rect> = targets.filter(|r| r.y0 < m.y1 && r.y1 > m.y0).copied().collect();
    v.sort_by(|a, b| a.x0.total_cmp(&b.x0));
    v
}

/// Equal spacing along x: centring between the nearest neighbours, or repeating a gap that
/// already exists between two neighbours in the row. (adjustment, distance).
fn equal_x(m: Rect, row: &[Rect], thr: f64) -> Option<(f64, f64)> {
    let left = row.iter().filter(|r| r.x1 <= m.x0 + thr).max_by(|a, b| a.x1.total_cmp(&b.x1));
    let right = row.iter().filter(|r| r.x0 >= m.x1 - thr).min_by(|a, b| a.x0.total_cmp(&b.x0));
    let mut cands: Vec<f64> = Vec::new();
    if let (Some(l), Some(r)) = (left, right) {
        cands.push((l.x1 + r.x0 - m.width()) / 2.0);
    }
    for g in existing_gaps(row) {
        if let Some(l) = left {
            cands.push(l.x1 + g);
        }
        if let Some(r) = right {
            cands.push(r.x0 - g - m.width());
        }
    }
    cands.into_iter().map(|x0| (x0 - m.x0, (x0 - m.x0).abs())).filter(|c| c.1 <= thr).min_by(|a, b| a.1.total_cmp(&b.1))
}

/// Positive gaps between consecutive non-overlapping rects of a row.
fn existing_gaps(row: &[Rect]) -> Vec<f64> {
    row.windows(2).map(|w| w[1].x0 - w[0].x1).filter(|g| *g > EPS).collect()
}

/// Gaps equal (within EPS) to the moved rect's gaps, to mark along x.
fn equal_marks_x(m: Rect, row: &[Rect]) -> Vec<Measure> {
    let left = row.iter().filter(|r| r.x1 <= m.x0 + EPS).max_by(|a, b| a.x1.total_cmp(&b.x1));
    let right = row.iter().filter(|r| r.x0 >= m.x1 - EPS).min_by(|a, b| a.x0.total_cmp(&b.x0));
    let seg = |a: &Rect, b: &Rect| {
        let y = (a.y0.max(b.y0) + a.y1.min(b.y1)) / 2.0;
        Measure { a: Point::new(a.x1, y), b: Point::new(b.x0, y), value: b.x0 - a.x1 }
    };
    let mut mine = Vec::new();
    if let Some(l) = left {
        mine.push(seg(l, &m));
    }
    if let Some(r) = right {
        mine.push(seg(&m, r));
    }
    let mut all = mine.clone();
    for w in row.windows(2) {
        if w[1].x0 - w[0].x1 > EPS {
            all.push(seg(&w[0], &w[1]));
        }
    }
    let mut out = Vec::new();
    for g in &mine {
        let same: Vec<Measure> = all.iter().filter(|o| (o.value - g.value).abs() < 0.01).copied().collect();
        if same.len() >= 2 {
            for s in same {
                if !out.contains(&s) {
                    out.push(s);
                }
            }
        }
    }
    out
}

/// Vertical guide lines (x = const) where `m` aligns with a target.
fn guides_x(m: Rect, targets: &[Rect], only: Option<&[f64]>) -> Vec<Guide> {
    let mine: Vec<f64> = only.map_or_else(|| stops(m).to_vec(), <[f64]>::to_vec);
    let mut lines: Vec<(f64, f64, f64)> = Vec::new();
    for r in targets {
        for tv in stops(*r) {
            if mine.iter().any(|mv| (mv - tv).abs() < 1e-6) {
                let (y0, y1) = (m.y0.min(r.y0), m.y1.max(r.y1));
                match lines.iter_mut().find(|l| (l.0 - tv).abs() < 1e-6) {
                    Some(l) => {
                        l.1 = l.1.min(y0);
                        l.2 = l.2.max(y1);
                    }
                    None => lines.push((tv, y0, y1)),
                }
            }
        }
    }
    lines.into_iter().map(|(x, y0, y1)| Guide { a: Point::new(x, y0), b: Point::new(x, y1) }).collect()
}

/// Distances from `m` to the nearest neighbour on each side (or to the parent's edge when
/// there is none and `m` is inside the parent).
pub fn measures(m: Rect, targets: &Targets) -> Vec<Measure> {
    let mut out = Vec::new();
    for transposed in [false, true] {
        let tr = |r: Rect| if transposed { t(r) } else { r };
        let mm = tr(m);
        let row = in_row(mm, targets.neighbours().map(|r| tr(*r)).collect::<Vec<_>>().iter());
        let y = (mm.y0 + mm.y1) / 2.0;
        let left = row.iter().filter(|r| r.x1 <= mm.x0).max_by(|a, b| a.x1.total_cmp(&b.x1)).map(|r| r.x1);
        let right = row.iter().filter(|r| r.x0 >= mm.x1).min_by(|a, b| a.x0.total_cmp(&b.x0)).map(|r| r.x0);
        let parent = targets.parent.map(tr).filter(|p| p.contains(Point::new(mm.x0, mm.y0)) && p.contains(Point::new(mm.x1, mm.y1)));
        let left = left.or(parent.map(|p| p.x0));
        let right = right.or(parent.map(|p| p.x1));
        let pt = |x: f64| if transposed { tp(Point::new(x, y)) } else { Point::new(x, y) };
        if let Some(l) = left.filter(|l| mm.x0 - l > EPS) {
            out.push(Measure { a: pt(l), b: pt(mm.x0), value: mm.x0 - l });
        }
        if let Some(r) = right.filter(|r| r - mm.x1 > EPS) {
            out.push(Measure { a: pt(mm.x1), b: pt(r), value: r - mm.x1 });
        }
    }
    out
}

/// Which parts of a box may snap on each axis: `None` = edges and centre (a move), or the
/// listed coordinates (the edges a resize handle moves).
#[derive(Debug, Clone, Copy, Default)]
pub struct Moving<'a> {
    pub x: Option<&'a [f64]>,
    pub y: Option<&'a [f64]>,
    /// Disable an axis entirely.
    pub skip_x: bool,
    pub skip_y: bool,
}

/// Snaps a box: returns the adjustment and what to draw. `grid` rounds to whole pixels on an
/// axis that found nothing to snap to.
pub fn snap_rect(m: Rect, targets: &Targets, thr: f64, grid: bool, moving: Moving<'_>) -> (Vec2, Feedback) {
    let mut adj = Vec2::ZERO;
    let mut fb = Feedback::default();
    let neighbours: Vec<Rect> = targets.neighbours().copied().collect();
    for transposed in [false, true] {
        let skip = if transposed { moving.skip_y } else { moving.skip_x };
        if skip {
            continue;
        }
        let tr = |r: Rect| if transposed { t(r) } else { r };
        let only = if transposed { moving.y } else { moving.x };
        let mm = tr(m);
        let all: Vec<Rect> = targets.rects.iter().map(|r| tr(*r)).collect();
        let row = in_row(mm, neighbours.iter().map(|r| tr(*r)).collect::<Vec<_>>().iter());
        let a = align_x(mm, &all, only, thr);
        // Equal spacing only for whole-box moves.
        let e = if only.is_none() { equal_x(mm, &row, thr) } else { None };
        let best = match (a, e) {
            (Some(a), Some(e)) => Some(if e.1 < a.1 { e } else { a }),
            (a, e) => a.or(e),
        };
        let d = match best {
            Some((d, _)) => d,
            None if grid => {
                let base = only.and_then(|o| o.first().copied()).unwrap_or(mm.x0);
                base.round() - base
            }
            None => 0.0,
        };
        if transposed {
            adj.y = d;
        } else {
            adj.x = d;
        }
    }
    // Feedback for the snapped box.
    let m2 = m + adj;
    for transposed in [false, true] {
        if (transposed && moving.skip_y) || (!transposed && moving.skip_x) {
            continue;
        }
        let tr = |r: Rect| if transposed { t(r) } else { r };
        let only: Option<Vec<f64>> = (if transposed { moving.y } else { moving.x }).map(|o| o.iter().map(|v| v + if transposed { adj.y } else { adj.x }).collect());
        let mm = tr(m2);
        let all: Vec<Rect> = targets.rects.iter().map(|r| tr(*r)).collect();
        let back = |g: Guide| if transposed { Guide { a: tp(g.a), b: tp(g.b) } } else { g };
        fb.guides.extend(guides_x(mm, &all, only.as_deref()).into_iter().map(back));
        if only.is_none() {
            let row = in_row(mm, neighbours.iter().map(|r| tr(*r)).collect::<Vec<_>>().iter());
            let backm = |g: Measure| if transposed { Measure { a: tp(g.a), b: tp(g.b), value: g.value } } else { g };
            fb.gaps.extend(equal_marks_x(mm, &row).into_iter().map(backm));
        }
    }
    fb.measures = measures(m2, targets);
    (adj, fb)
}

/// Snaps a point (a resize handle or a drawing corner) on the given axes.
pub fn snap_point(p: Point, targets: &Targets, thr: f64, grid: bool, axes: (bool, bool)) -> (Point, Feedback) {
    let xs = [p.x];
    let ys = [p.y];
    let moving = Moving { x: Some(&xs), y: Some(&ys), skip_x: !axes.0, skip_y: !axes.1 };
    let (adj, mut fb) = snap_rect(Rect::from_points(p, p), targets, thr, grid, moving);
    fb.measures.clear();
    (p + adj, fb)
}
