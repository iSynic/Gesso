//! Vector paths as anchors and handles: the model behind the pen tool and path edit mode.
//! Path data stays SVG `d` strings in the document; this module parses them into contours of
//! anchors (each with optional in and out handles, as absolute points), edits those, and writes
//! them back. Quadratic segments become cubics when edited.
//!
//! A path node's data is in its local space with the box at the origin. After an edit the box
//! is fitted to the new outline again: the data is shifted to start at the origin and the
//! node's position moves to match, so nothing jumps on the canvas (rotation included).

use doc::model::{FillRule, PathData};
use doc::{NodeId, NodeKind, PropValue, TxMeta};
use kurbo::{Affine, BezPath, CubicBez, Line, ParamCurve, ParamCurveNearest, PathEl, PathSeg, Point, Rect, Shape, Vec2};
use serde_json::json;

use crate::session::{Error, Session};
use crate::snap::{self, Targets};
use crate::xform::Placement;

/// Screen distance (px) within which a point or handle is grabbed.
pub const GRAB_PX: f64 = 6.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Anchor {
    pub p: Point,
    pub h_in: Option<Point>,
    pub h_out: Option<Point>,
}

impl Anchor {
    pub fn corner(p: Point) -> Self {
        Anchor { p, h_in: None, h_out: None }
    }

    /// Both handles exist and point in opposite directions.
    pub fn is_smooth(&self) -> bool {
        match (self.h_in, self.h_out) {
            (Some(a), Some(b)) => {
                let (u, v) = (a - self.p, b - self.p);
                u.hypot() > 1e-9 && v.hypot() > 1e-9 && u.cross(v).abs() <= 1e-3 * u.hypot() * v.hypot() && u.dot(v) < 0.0
            }
            _ => false,
        }
    }

    fn map(&self, a: Affine) -> Self {
        Anchor { p: a * self.p, h_in: self.h_in.map(|h| a * h), h_out: self.h_out.map(|h| a * h) }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Contour {
    pub anchors: Vec<Anchor>,
    pub closed: bool,
}

impl Contour {
    /// Segment `i`, from anchor `i` to the next (wrapping when closed).
    pub fn segment(&self, i: usize) -> Option<PathSeg> {
        let n = self.anchors.len();
        let j = if i + 1 < n { i + 1 } else if self.closed && n > 1 { 0 } else { return None };
        let (a, b) = (&self.anchors[i], &self.anchors[j]);
        Some(match (a.h_out, b.h_in) {
            (None, None) => PathSeg::Line(Line::new(a.p, b.p)),
            (o, i) => PathSeg::Cubic(CubicBez::new(a.p, o.unwrap_or(a.p), i.unwrap_or(b.p), b.p)),
        })
    }

    pub fn segment_count(&self) -> usize {
        let n = self.anchors.len();
        if self.closed && n > 1 { n } else { n.saturating_sub(1) }
    }

    fn map(&self, a: Affine) -> Self {
        Contour { anchors: self.anchors.iter().map(|x| x.map(a)).collect(), closed: self.closed }
    }
}

/// One `PathData` of a path node as contours.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SubPath {
    pub rule: FillRule,
    pub contours: Vec<Contour>,
}

/// Parses SVG path data into contours. Invalid data gives no contours.
pub fn parse(d: &str) -> Vec<Contour> {
    let Ok(b) = BezPath::from_svg(d) else { return Vec::new() };
    let mut out: Vec<Contour> = Vec::new();
    let mut cur: Option<Contour> = None;
    let mut last = Point::ZERO;
    let finish = |cur: &mut Option<Contour>, out: &mut Vec<Contour>| {
        if let Some(c) = cur.take()
            && !c.anchors.is_empty()
        {
            out.push(c);
        }
    };
    for el in b.elements() {
        match *el {
            PathEl::MoveTo(p) => {
                finish(&mut cur, &mut out);
                cur = Some(Contour { anchors: vec![Anchor::corner(p)], closed: false });
                last = p;
            }
            PathEl::LineTo(p) => {
                cur.get_or_insert_with(|| Contour { anchors: vec![Anchor::corner(last)], closed: false }).anchors.push(Anchor::corner(p));
                last = p;
            }
            PathEl::QuadTo(c, p) => {
                let c1 = last + (c - last) * (2.0 / 3.0);
                let c2 = p + (c - p) * (2.0 / 3.0);
                let k = cur.get_or_insert_with(|| Contour { anchors: vec![Anchor::corner(last)], closed: false });
                if let Some(a) = k.anchors.last_mut() {
                    a.h_out = Some(c1);
                }
                k.anchors.push(Anchor { p, h_in: Some(c2), h_out: None });
                last = p;
            }
            PathEl::CurveTo(c1, c2, p) => {
                let k = cur.get_or_insert_with(|| Contour { anchors: vec![Anchor::corner(last)], closed: false });
                // A control point on its anchor is no handle (how `to_d` writes a missing one).
                if let Some(a) = k.anchors.last_mut() {
                    a.h_out = (c1 != a.p).then_some(c1);
                }
                k.anchors.push(Anchor { p, h_in: (c2 != p).then_some(c2), h_out: None });
                last = p;
            }
            PathEl::ClosePath => {
                if let Some(mut c) = cur.take() {
                    // A last anchor on the first one is the closing segment's end: merge them.
                    if c.anchors.len() > 1
                        && let (Some(first), Some(end)) = (c.anchors.first().copied(), c.anchors.last().copied())
                        && first.p.distance(end.p) < 1e-9
                    {
                        c.anchors.pop();
                        c.anchors[0].h_in = end.h_in;
                    }
                    last = c.anchors.first().map_or(last, |a| a.p);
                    c.closed = true;
                    out.push(c);
                }
            }
        }
    }
    finish(&mut cur, &mut out);
    out
}

fn num(v: f64, out: &mut String) {
    let r = (v * 1000.0).round() / 1000.0;
    let r = if r == 0.0 { 0.0 } else { r };
    let s = format!("{r:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    out.push_str(s);
}

fn pt(p: Point, out: &mut String) {
    num(p.x, out);
    out.push(' ');
    num(p.y, out);
}

/// Writes contours as SVG path data (absolute commands, at most three decimals).
pub fn to_d(contours: &[Contour]) -> String {
    let mut s = String::new();
    for c in contours {
        let Some(first) = c.anchors.first() else { continue };
        if !s.is_empty() {
            s.push(' ');
        }
        s.push('M');
        pt(first.p, &mut s);
        for i in 0..c.segment_count() {
            match c.segment(i) {
                Some(PathSeg::Cubic(b)) => {
                    s.push_str(" C");
                    pt(b.p1, &mut s);
                    s.push(' ');
                    pt(b.p2, &mut s);
                    s.push(' ');
                    pt(b.p3, &mut s);
                }
                Some(PathSeg::Line(l)) => {
                    // The closing line is implied by Z.
                    if c.closed && i + 1 == c.segment_count() {
                        continue;
                    }
                    s.push_str(" L");
                    pt(l.p1, &mut s);
                }
                _ => {}
            }
        }
        if c.closed {
            s.push_str(" Z");
        }
    }
    s
}

pub fn to_bez(contours: &[Contour]) -> BezPath {
    BezPath::from_svg(&to_d(contours)).unwrap_or_default()
}

/// The path node's data as sub-paths.
pub fn read(s: &Session, id: NodeId) -> Option<Vec<SubPath>> {
    match &s.doc().get(id)?.kind {
        NodeKind::Path { paths } => Some(paths.iter().map(|p| SubPath { rule: p.fill_rule, contours: parse(&p.d) }).collect()),
        _ => None,
    }
}

/// Fits a box to `subs` (in the space `local` maps to the parent from) and returns the new
/// placement, keeping rotation and flips, plus the data shifted into it.
fn fit(subs: &[SubPath], old: &Placement) -> (Placement, Vec<SubPath>) {
    let mut bb: Option<Rect> = None;
    for sp in subs {
        for c in &sp.contours {
            let b = if c.anchors.len() == 1 { Rect::from_points(c.anchors[0].p, c.anchors[0].p) } else { to_bez(std::slice::from_ref(c)).bounding_box() };
            bb = Some(bb.map_or(b, |r| r.union(b)));
        }
    }
    // The origin is kept on the data's 1/1000 grid, so shifting loses nothing when written.
    let b = bb.unwrap_or_default();
    let b = Rect::from_origin_size(Point::new((b.x0 * 1000.0).floor() / 1000.0, (b.y0 * 1000.0).floor() / 1000.0), b.size());
    let shift = Affine::translate(-b.origin().to_vec2());
    let moved: Vec<SubPath> = subs.iter().map(|sp| SubPath { rule: sp.rule, contours: sp.contours.iter().map(|c| c.map(shift)).collect() }).collect();
    let at = old.local() * b.origin();
    let shape = Placement { x: 0.0, y: 0.0, w: b.width(), h: b.height(), ..*old };
    let o = shape.local() * Point::ZERO;
    (Placement { x: at.x - o.x, y: at.y - o.y, ..shape }, moved)
}

fn path_kind(subs: &[SubPath]) -> NodeKind {
    NodeKind::Path { paths: subs.iter().map(|sp| PathData { d: to_d(&sp.contours), fill_rule: sp.rule }).collect() }
}

/// Writes `subs` (in the node's local space as placed at `old`) to node `id`, refitting its box.
fn write(tx: &mut doc::Tx<'_>, id: NodeId, subs: &[SubPath], old: &Placement) -> Result<(), doc::Error> {
    let (p, moved) = fit(subs, old);
    tx.update(id, PropValue::Kind(path_kind(&moved)))?;
    let now = tx.doc().get(id).map(Placement::of).ok_or(doc::Error::Input("path went away".into()))?;
    for u in p.diff(&now) {
        tx.update(id, u)?;
    }
    Ok(())
}

// ---- path edit mode -------------------------------------------------------------------------

/// A point of a path: sub-path, contour and anchor index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PointRef {
    pub sub: usize,
    pub contour: usize,
    pub anchor: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    In,
    Out,
}

/// What a press in path edit mode grabbed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Grab {
    Anchor(PointRef),
    Handle(PointRef, Side),
    /// A point on segment `anchor` (to the next anchor) at curve parameter `t`.
    Segment(PointRef, f64),
}

struct Drag {
    grab: Grab,
    /// Pointer at the press, in the node's local space.
    start: Point,
    orig: Vec<SubPath>,
    placement: Placement,
    key: String,
    moved: bool,
    /// The grabbed anchor was smooth: dragging one handle turns the other.
    smooth: bool,
}

/// Path edit mode on one path node.
pub struct PathEdit {
    pub node: NodeId,
    pub subs: Vec<SubPath>,
    pub selected: Vec<PointRef>,
    drag: Option<Drag>,
    count: u64,
}

/// What to draw in path edit mode, in world space.
#[derive(Debug, Clone, Default)]
pub struct PathOverlay {
    pub outline: BezPath,
    /// Anchors with whether each is selected.
    pub anchors: Vec<(Point, bool)>,
    /// Handle lines (anchor, handle) of the selected anchors.
    pub handles: Vec<(Point, Point)>,
}

impl PathEdit {
    pub fn begin(s: &mut Session, id: NodeId) -> Option<PathEdit> {
        let subs = read(s, id)?;
        s.select(vec![id]);
        Some(PathEdit { node: id, subs, selected: Vec::new(), drag: None, count: 0 })
    }

    /// Re-reads the path after the document changed (undo). `false` if it is gone.
    pub fn sync(&mut self, s: &Session) -> bool {
        if self.drag.is_some() {
            return true;
        }
        let Some(subs) = read(s, self.node) else { return false };
        if subs != self.subs {
            self.subs = subs;
            self.selected.retain(|r| anchor(&self.subs, *r).is_some());
        }
        true
    }

    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    fn world(&self, s: &Session) -> Affine {
        s.scene.index.world(self.node).unwrap_or(Affine::IDENTITY)
    }

    /// What is under a screen point: handles of selected points first, then anchors, then
    /// segments.
    pub fn grab_at(&self, s: &Session, screen: Point) -> Option<Grab> {
        let to_screen = s.camera.affine() * self.world(s);
        let near = |p: Point| (to_screen * p).distance(screen) <= GRAB_PX;
        for &r in &self.selected {
            if let Some(a) = anchor(&self.subs, r) {
                if a.h_in.is_some_and(near) {
                    return Some(Grab::Handle(r, Side::In));
                }
                if a.h_out.is_some_and(near) {
                    return Some(Grab::Handle(r, Side::Out));
                }
            }
        }
        for (si, sp) in self.subs.iter().enumerate() {
            for (ci, c) in sp.contours.iter().enumerate() {
                for (ai, a) in c.anchors.iter().enumerate() {
                    if near(a.p) {
                        return Some(Grab::Anchor(PointRef { sub: si, contour: ci, anchor: ai }));
                    }
                }
            }
        }
        let local = (to_screen).inverse() * screen;
        let tol = GRAB_PX / (s.camera.zoom * linear_scale(self.world(s)));
        let mut best: Option<(f64, Grab)> = None;
        for (si, sp) in self.subs.iter().enumerate() {
            for (ci, c) in sp.contours.iter().enumerate() {
                for i in 0..c.segment_count() {
                    let Some(seg) = c.segment(i) else { continue };
                    let n = seg.nearest(local, 1e-6);
                    let d = n.distance_sq.sqrt();
                    if d <= tol && best.is_none_or(|(b, _)| d < b) {
                        best = Some((d, Grab::Segment(PointRef { sub: si, contour: ci, anchor: i }, n.t)));
                    }
                }
            }
        }
        best.map(|(_, g)| g)
    }

    /// A press: selects or grabs what is under the pointer and starts a drag. A press on a
    /// segment adds a point there. Returns `false` if nothing was hit.
    pub fn pointer_down(&mut self, s: &mut Session, screen: Point, shift: bool) -> Result<bool, Error> {
        let Some(mut grab) = self.grab_at(s, screen) else {
            if !shift {
                self.selected.clear();
            }
            return Ok(false);
        };
        let placement = s.doc().get(self.node).map(Placement::of).ok_or(Error::Doc(doc::Error::Input("path went away".into())))?;
        self.count += 1;
        let key = format!("path-edit#{}#{}", self.node, self.count);
        if let Grab::Segment(r, t) = grab {
            let at = self.insert_point(r, t);
            let id = self.node;
            let subs = self.subs.clone();
            s.transact(TxMeta::user("Add point").coalesce(key.clone()), |tx| write(tx, id, &subs, &placement))?;
            self.selected = vec![at];
            grab = Grab::Anchor(at);
            self.resync(s);
        }
        let placement = s.doc().get(self.node).map(Placement::of).unwrap_or(placement);
        match grab {
            Grab::Anchor(r) if shift => {
                if let Some(i) = self.selected.iter().position(|x| *x == r) {
                    self.selected.remove(i);
                } else {
                    self.selected.push(r);
                }
            }
            Grab::Anchor(r) if !self.selected.contains(&r) => self.selected = vec![r],
            _ => {}
        }
        let smooth = matches!(grab, Grab::Handle(r, _) if anchor(&self.subs, r).is_some_and(|a| a.is_smooth()));
        let start = (s.camera.affine() * self.world(s)).inverse() * screen;
        self.drag = Some(Drag { grab, start, orig: self.subs.clone(), placement, key, moved: false, smooth });
        Ok(true)
    }

    /// Drags what was grabbed. `alt` breaks a smooth point's handles apart.
    pub fn pointer_move(&mut self, s: &mut Session, screen: Point, alt: bool) -> Result<(), Error> {
        let Some(d) = &mut self.drag else { return Ok(()) };
        let to_local = (s.camera.affine() * d.placement_world(s, self.node)).inverse();
        let now = to_local * screen;
        let delta = now - d.start;
        if !d.moved && (s.camera.zoom * delta.hypot()) < crate::tools::DRAG_PX {
            return Ok(());
        }
        d.moved = true;
        let mut subs = d.orig.clone();
        match d.grab {
            Grab::Anchor(_) => {
                for &r in &self.selected {
                    if let Some(a) = anchor_mut(&mut subs, r) {
                        a.p += delta;
                        a.h_in = a.h_in.map(|h| h + delta);
                        a.h_out = a.h_out.map(|h| h + delta);
                    }
                }
            }
            Grab::Handle(r, side) => {
                let smooth = d.smooth && !alt;
                if let Some(a) = anchor_mut(&mut subs, r) {
                    let (this, other) = match side {
                        Side::In => (&mut a.h_in, &mut a.h_out),
                        Side::Out => (&mut a.h_out, &mut a.h_in),
                    };
                    let h = this.unwrap_or(a.p) + delta;
                    *this = Some(h);
                    if smooth && let Some(o) = other {
                        let len = (*o - a.p).hypot();
                        let dir = a.p - h;
                        if dir.hypot() > 1e-9 {
                            *other = Some(a.p + dir.normalize() * len);
                        }
                    }
                }
            }
            Grab::Segment(..) => {}
        }
        let (id, placement, key) = (self.node, d.placement, d.key.clone());
        s.transact(TxMeta::user("Edit path").coalesce(key), |tx| write(tx, id, &subs, &placement))?;
        self.resync(s);
        Ok(())
    }

    pub fn pointer_up(&mut self) {
        self.drag = None;
    }

    /// Esc during a drag: puts the path back. Returns whether a drag was running.
    pub fn cancel_drag(&mut self, s: &mut Session) -> Result<bool, Error> {
        let Some(d) = self.drag.take() else { return Ok(false) };
        if d.moved {
            let (id, orig, placement, key) = (self.node, d.orig, d.placement, d.key);
            s.transact(TxMeta::user("Edit path").coalesce(key), |tx| write(tx, id, &orig, &placement))?;
            self.resync(s);
        }
        Ok(true)
    }

    fn resync(&mut self, s: &Session) {
        if let Some(subs) = read(s, self.node) {
            self.subs = subs;
        }
    }

    /// Splits segment `r` at `t`, returning the new point.
    fn insert_point(&mut self, r: PointRef, t: f64) -> PointRef {
        let Some(c) = self.subs.get_mut(r.sub).and_then(|sp| sp.contours.get_mut(r.contour)) else { return r };
        let Some(seg) = c.segment(r.anchor) else { return r };
        let n = c.anchors.len();
        let j = (r.anchor + 1) % n;
        let at = r.anchor + 1;
        match seg {
            PathSeg::Line(l) => {
                c.anchors.insert(at, Anchor::corner(l.eval(t)));
            }
            PathSeg::Cubic(b) => {
                let (left, right) = (b.subsegment(0.0..t), b.subsegment(t..1.0));
                c.anchors[r.anchor].h_out = Some(left.p1);
                c.anchors[j].h_in = Some(right.p2);
                c.anchors.insert(at, Anchor { p: left.p3, h_in: Some(left.p2), h_out: Some(right.p1) });
            }
            PathSeg::Quad(_) => {}
        }
        PointRef { anchor: at, ..r }
    }

    /// Deletes the selected points. Contours left with one point go; a path left with nothing
    /// is deleted. Returns `false` when the node was deleted (the edit is over).
    pub fn delete_selected(&mut self, s: &mut Session) -> Result<bool, Error> {
        if self.selected.is_empty() {
            return Ok(true);
        }
        let mut sel = std::mem::take(&mut self.selected);
        sel.sort();
        for r in sel.iter().rev() {
            if let Some(c) = self.subs.get_mut(r.sub).and_then(|sp| sp.contours.get_mut(r.contour))
                && r.anchor < c.anchors.len()
            {
                c.anchors.remove(r.anchor);
            }
        }
        for sp in &mut self.subs {
            sp.contours.retain(|c| c.anchors.len() > 1);
            for c in &mut sp.contours {
                if c.anchors.len() < 3 {
                    c.closed = false;
                }
            }
        }
        self.subs.retain(|sp| !sp.contours.is_empty());
        let id = self.node;
        if self.subs.is_empty() {
            s.transact(TxMeta::user("Delete path"), |tx| tx.delete(id))?;
            s.clear_selection();
            return Ok(false);
        }
        let placement = s.doc().get(id).map(Placement::of).ok_or(Error::Doc(doc::Error::Input("path went away".into())))?;
        let subs = self.subs.clone();
        s.transact(TxMeta::user("Delete points"), |tx| write(tx, id, &subs, &placement))?;
        self.resync(s);
        Ok(true)
    }

    /// Turns a smooth point into a corner, or a corner into a smooth point with handles along
    /// its neighbours' direction.
    pub fn toggle_smooth(&mut self, s: &mut Session, r: PointRef) -> Result<(), Error> {
        let Some(c) = self.subs.get_mut(r.sub).and_then(|sp| sp.contours.get_mut(r.contour)) else { return Ok(()) };
        let n = c.anchors.len();
        if r.anchor >= n {
            return Ok(());
        }
        let a = c.anchors[r.anchor];
        if a.h_in.is_some() || a.h_out.is_some() {
            c.anchors[r.anchor].h_in = None;
            c.anchors[r.anchor].h_out = None;
        } else {
            let prev = if r.anchor > 0 { Some(c.anchors[r.anchor - 1].p) } else if c.closed { Some(c.anchors[n - 1].p) } else { None };
            let next = if r.anchor + 1 < n { Some(c.anchors[r.anchor + 1].p) } else if c.closed { Some(c.anchors[0].p) } else { None };
            let dir = match (prev, next) {
                (Some(p), Some(q)) => q - p,
                (Some(p), None) => a.p - p,
                (None, Some(q)) => q - a.p,
                (None, None) => Vec2::new(1.0, 0.0),
            };
            if dir.hypot() < 1e-9 {
                return Ok(());
            }
            let u = dir.normalize();
            let li = prev.map_or(dir.hypot() / 3.0, |p| (a.p - p).hypot() / 3.0);
            let lo = next.map_or(dir.hypot() / 3.0, |q| (q - a.p).hypot() / 3.0);
            c.anchors[r.anchor].h_in = Some(a.p - u * li);
            c.anchors[r.anchor].h_out = Some(a.p + u * lo);
        }
        let id = self.node;
        let placement = s.doc().get(id).map(Placement::of).ok_or(Error::Doc(doc::Error::Input("path went away".into())))?;
        let subs = self.subs.clone();
        s.transact(TxMeta::user("Toggle smooth point"), |tx| write(tx, id, &subs, &placement))?;
        self.resync(s);
        Ok(())
    }

    /// Moves the selected points by `d` (world units), as one step per key press.
    pub fn nudge(&mut self, s: &mut Session, d: Vec2) -> Result<(), Error> {
        if self.selected.is_empty() {
            return Ok(());
        }
        let w = self.world(s);
        let dl = w.inverse() * (w * Point::ZERO + d) - Point::ZERO;
        for &r in &self.selected {
            if let Some(a) = anchor_mut(&mut self.subs, r) {
                a.p += dl;
                a.h_in = a.h_in.map(|h| h + dl);
                a.h_out = a.h_out.map(|h| h + dl);
            }
        }
        let id = self.node;
        let placement = s.doc().get(id).map(Placement::of).ok_or(Error::Doc(doc::Error::Input("path went away".into())))?;
        let subs = self.subs.clone();
        s.transact(TxMeta::user("Nudge points"), |tx| write(tx, id, &subs, &placement))?;
        self.resync(s);
        Ok(())
    }

    pub fn overlay(&self, s: &Session) -> PathOverlay {
        let w = self.world(s);
        let mut out = PathOverlay::default();
        for (si, sp) in self.subs.iter().enumerate() {
            out.outline.extend(to_bez(&sp.contours).iter());
            for (ci, c) in sp.contours.iter().enumerate() {
                for (ai, a) in c.anchors.iter().enumerate() {
                    let r = PointRef { sub: si, contour: ci, anchor: ai };
                    let sel = self.selected.contains(&r);
                    out.anchors.push((w * a.p, sel));
                    if sel {
                        for h in [a.h_in, a.h_out].into_iter().flatten() {
                            out.handles.push((w * a.p, w * h));
                        }
                    }
                }
            }
        }
        out.outline.apply_affine(w);
        out
    }
}

impl Drag {
    /// The node's world transform as placed when the drag began.
    fn placement_world(&self, s: &Session, id: NodeId) -> Affine {
        let parent = crate::select::parent_of(s.doc(), id).and_then(|p| s.scene.index.world(p)).unwrap_or(Affine::IDENTITY);
        parent * self.placement.local()
    }
}

fn linear_scale(a: Affine) -> f64 {
    let c = a.as_coeffs();
    (c[0] * c[3] - c[1] * c[2]).abs().sqrt().max(1e-9)
}

fn anchor(subs: &[SubPath], r: PointRef) -> Option<&Anchor> {
    subs.get(r.sub)?.contours.get(r.contour)?.anchors.get(r.anchor)
}

fn anchor_mut(subs: &mut [SubPath], r: PointRef) -> Option<&mut Anchor> {
    subs.get_mut(r.sub)?.contours.get_mut(r.contour)?.anchors.get_mut(r.anchor)
}

// ---- pen tool -------------------------------------------------------------------------------

/// Screen distance (px) from the first point that closes the path.
pub const CLOSE_PX: f64 = 8.0;

/// The pen tool drawing one open contour in `parent`'s space.
pub struct Pen {
    pub node: Option<NodeId>,
    pub parent: NodeId,
    /// Points in the parent's space.
    pub contour: Contour,
    key: String,
    /// The pointer is down on the last point (dragging sets its handles).
    pressed: Option<Point>,
    /// The pointer position (world), for the preview segment.
    pub hover: Option<Point>,
    targets: Targets,
}

/// What a pen press did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PenPress {
    Added,
    /// The press was on the first point: the path is closed and finished.
    Closed,
}

impl Pen {
    pub fn new(s: &Session, world: Point, key: String) -> Pen {
        let index = &s.scene.index;
        let parent = crate::transform::drop_target(s.doc(), index, s.page(), world, &[]);
        let targets = Targets::collect(s.doc(), index, s.page(), parent, &Default::default(), s.view());
        Pen { node: None, parent, contour: Contour::default(), key, pressed: None, hover: None, targets }
    }

    fn to_parent(&self, s: &Session) -> Affine {
        s.scene.index.world(self.parent).unwrap_or(Affine::IDENTITY).inverse()
    }

    /// The point a press at `world` places: snapped, or at 45° steps from the last point with
    /// Shift.
    fn place(&self, s: &Session, world: Point, shift: bool) -> Point {
        let to_parent = self.to_parent(s);
        if shift && let Some(last) = self.contour.anchors.last() {
            let lw = to_parent.inverse() * last.p;
            let d = world - lw;
            let a = (d.y.atan2(d.x) / std::f64::consts::FRAC_PI_4).round() * std::f64::consts::FRAC_PI_4;
            return to_parent * (lw + Vec2::from_angle(a) * d.hypot());
        }
        let p = if s.snap_to_objects || s.snap_to_pixels {
            let t = if s.snap_to_objects { self.targets.clone() } else { Targets::default() };
            snap::snap_point(world, &t, snap::SNAP_PX / s.camera.zoom, s.snap_to_pixels, (true, true)).0
        } else {
            world
        };
        to_parent * p
    }

    pub fn pointer_down(&mut self, s: &mut Session, world: Point, shift: bool) -> Result<PenPress, Error> {
        if self.contour.anchors.len() >= 2
            && let Some(first) = self.contour.anchors.first()
        {
            let fw = self.to_parent(s).inverse() * first.p;
            if s.camera.to_screen(fw).distance(s.camera.to_screen(world)) <= CLOSE_PX {
                self.contour.closed = true;
                self.write(s)?;
                return Ok(PenPress::Closed);
            }
        }
        let p = self.place(s, world, shift);
        self.contour.anchors.push(Anchor::corner(p));
        self.pressed = Some(p);
        self.write(s)?;
        Ok(PenPress::Added)
    }

    /// Moving with the button down drags out the last point's handles (mirrored, or only the
    /// outgoing one with Alt); moving with it up updates the preview.
    pub fn pointer_move(&mut self, s: &mut Session, world: Point, alt: bool) -> Result<(), Error> {
        self.hover = Some(world);
        let Some(p) = self.pressed else { return Ok(()) };
        let q = self.to_parent(s) * world;
        if s.camera.zoom * (q - p).hypot() < crate::tools::DRAG_PX {
            return Ok(());
        }
        if let Some(a) = self.contour.anchors.last_mut() {
            a.h_out = Some(q);
            if !alt || a.h_in.is_none() {
                a.h_in = Some(p - (q - p));
            }
        }
        self.write(s)
    }

    pub fn pointer_up(&mut self) {
        self.pressed = None;
    }

    pub fn is_pressed(&self) -> bool {
        self.pressed.is_some()
    }

    fn write(&mut self, s: &mut Session) -> Result<(), Error> {
        let subs = vec![SubPath { rule: FillRule::NonZero, contours: vec![self.contour.clone()] }];
        let (parent, existing) = (self.parent, self.node);
        let base = Placement { x: 0.0, y: 0.0, w: 0.0, h: 0.0, rotation: 0.0, flip_x: false, flip_y: false };
        let id = s.transact(TxMeta::user("Draw path").coalesce(self.key.clone()), |tx| {
            let id = match existing {
                Some(id) => id,
                None => tx.insert_json(
                    Some(parent),
                    None,
                    json!({"type": "path", "name": "Path", "paths": [], "strokes": [{"paint": {"type": "solid", "color": "#000000"}, "weight": 1}]}),
                )?,
            };
            write(tx, id, &subs, &base)?;
            Ok(id)
        })?;
        self.node = Some(id);
        Ok(())
    }

    /// Ends the drawing. A path with fewer than two points is removed. Returns the path drawn.
    pub fn finish(self, s: &mut Session) -> Result<Option<NodeId>, Error> {
        let Some(id) = self.node else { return Ok(None) };
        if self.contour.anchors.len() < 2 {
            s.transact(TxMeta::user("Draw path").coalesce(self.key), |tx| tx.delete(id))?;
            return Ok(None);
        }
        s.select(vec![id]);
        Ok(Some(id))
    }

    /// The last point and the preview segment's end, in world space.
    pub fn preview(&self, s: &Session) -> Option<(Point, Point)> {
        let last = self.contour.anchors.last()?;
        let to_world = self.to_parent(s).inverse();
        Some((to_world * last.p, self.hover?))
    }

    /// The contour drawn so far, in world space.
    pub fn outline(&self, s: &Session) -> BezPath {
        let mut b = to_bez(std::slice::from_ref(&self.contour));
        b.apply_affine(self.to_parent(s).inverse());
        b
    }

    /// Points in world space; the first is highlighted when a click would close the path.
    pub fn anchors(&self, s: &Session) -> Vec<Point> {
        let to_world = self.to_parent(s).inverse();
        self.contour.anchors.iter().map(|a| to_world * a.p).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for d in ["M0 0 L10 0 L10 10 Z", "M0 0 C5 0 10 5 10 10 L0 10", "M0 0 C0 0 5 5 10 10 C15 15 20 20 20 20", "M0 0 C1 2 3 4 5 6 C7 8 9 10 0 0 Z", "M1 1 L2 2 M5 5 L6 6"] {
            let c = parse(d);
            let again = to_d(&c);
            assert_eq!(parse(&again), c, "{d} -> {again}");
        }
        assert_eq!(to_d(&parse("M0 0 L10 0 L10 10 Z")), "M0 0 L10 0 L10 10 Z");
        let c = parse("M0 0 C1 2 3 4 5 6 C7 8 9 10 0 0 Z");
        assert_eq!(c[0].anchors.len(), 2);
        assert!(c[0].closed);
    }

    #[test]
    fn quads_become_cubics() {
        let c = parse("M0 0 Q 3 3 6 0");
        assert_eq!(c[0].anchors[0].h_out, Some(Point::new(2.0, 2.0)));
        assert_eq!(c[0].anchors[1].h_in, Some(Point::new(4.0, 2.0)));
    }

    #[test]
    fn fit_keeps_world_position() {
        let subs = vec![SubPath { rule: FillRule::NonZero, contours: parse("M10 20 L50 20 L50 80 Z") }];
        let old = Placement { x: 100.0, y: 100.0, w: 60.0, h: 90.0, rotation: 30.0, flip_x: false, flip_y: true };
        let (p, moved) = fit(&subs, &old);
        assert_eq!((p.w, p.h), (40.0, 60.0));
        let a0 = old.local() * Point::new(10.0, 20.0);
        let a1 = p.local() * moved[0].contours[0].anchors[0].p;
        assert!(a0.distance(a1) < 1e-9, "{a0:?} {a1:?}");
    }
}
