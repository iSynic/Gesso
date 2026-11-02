//! Outlines of nodes in their local coordinates (box from (0, 0) to (width, height)).

use std::f64::consts::{PI, TAU};

use doc::model::{Arc as ArcSpec, FillRule, Radius};
use doc::{Node, NodeKind};
use kurbo::{Arc, BezPath, Ellipse, Point, Rect, RoundedRect, RoundedRectRadii, Shape, Vec2};
use peniko::Fill;

/// Curve tolerance for converting shapes to paths, in local units.
pub const TOLERANCE: f64 = 0.05;

/// A filled outline and its fill rule.
#[derive(Debug, Clone)]
pub struct Outline {
    pub path: BezPath,
    pub rule: Fill,
    /// Closed shapes can take inside/outside strokes; open ones are always centred.
    pub closed: bool,
}

/// Corner radii clamped so opposite corners never overlap.
pub fn radii(r: Radius, w: f64, h: f64) -> RoundedRectRadii {
    let [tl, tr, br, bl] = match r {
        Radius::Uniform(v) => [v; 4],
        Radius::Each(v) => v,
    };
    let m = (w.min(h) / 2.0).max(0.0);
    let c = |v: f64| v.clamp(0.0, m);
    RoundedRectRadii::new(c(tl), c(tr), c(br), c(bl))
}

/// The radius if all four corners are equal (the blurred-rect fast path needs that).
pub fn uniform_radius(r: Radius, w: f64, h: f64) -> Option<f64> {
    let rr = radii(r, w, h);
    (rr.top_left == rr.top_right && rr.top_left == rr.bottom_right && rr.top_left == rr.bottom_left).then_some(rr.top_left)
}

fn rounded_rect(n: &Node) -> BezPath {
    let (w, h) = (n.props.width, n.props.height);
    RoundedRect::from_rect(Rect::new(0.0, 0.0, w, h), radii(n.props.radius, w, h)).to_path(TOLERANCE)
}

fn ellipse(w: f64, h: f64, arc: Option<ArcSpec>) -> BezPath {
    let (c, r) = (Point::new(w / 2.0, h / 2.0), Vec2::new(w / 2.0, h / 2.0));
    let a = arc.unwrap_or(ArcSpec { start: 0.0, sweep: 360.0, inner_ratio: 0.0 });
    let inner = a.inner_ratio.clamp(0.0, 1.0);
    let sweep = a.sweep.clamp(-360.0, 360.0).to_radians();
    let start = a.start.to_radians();
    let full = sweep.abs() >= TAU - 1e-9;
    if full {
        let mut p = Ellipse::new(c, r, 0.0).to_path(TOLERANCE);
        if inner > 0.0 {
            // A hole: the inner ellipse, filled even-odd by the caller's rule choice.
            p.extend(Ellipse::new(c, r * inner, 0.0).to_path(TOLERANCE));
        }
        return p;
    }
    let mut p = BezPath::new();
    let outer = Arc::new(c, r, start, sweep, 0.0);
    p.move_to(c + Vec2::new(r.x * start.cos(), r.y * start.sin()));
    outer.to_cubic_beziers(TOLERANCE, |a, b, e| p.curve_to(a, b, e));
    if inner > 0.0 {
        let ri = r * inner;
        let end = start + sweep;
        p.line_to(c + Vec2::new(ri.x * end.cos(), ri.y * end.sin()));
        Arc::new(c, ri, end, -sweep, 0.0).to_cubic_beziers(TOLERANCE, |a, b, e| p.curve_to(a, b, e));
    } else {
        p.line_to(c);
    }
    p.close_path();
    p
}

fn polygon(w: f64, h: f64, sides: u32, star: Option<f64>) -> BezPath {
    let n = sides.max(3) as usize;
    let (cx, cy, rx, ry) = (w / 2.0, h / 2.0, w / 2.0, h / 2.0);
    let points = if star.is_some() { n * 2 } else { n };
    let mut p = BezPath::new();
    for i in 0..points {
        let a = -PI / 2.0 + i as f64 * TAU / points as f64;
        let k = match star {
            Some(r) if i % 2 == 1 => r.clamp(0.0, 1.0),
            _ => 1.0,
        };
        let pt = Point::new(cx + rx * k * a.cos(), cy + ry * k * a.sin());
        if i == 0 { p.move_to(pt) } else { p.line_to(pt) }
    }
    p.close_path();
    p
}

/// The node's own outline, if it has one. Pages, groups, text and instances' content are drawn
/// another way; text has none here.
pub fn outline(n: &Node) -> Option<Outline> {
    let (w, h) = (n.props.width, n.props.height);
    let closed = |path| Some(Outline { path, rule: Fill::NonZero, closed: true });
    match &n.kind {
        NodeKind::Frame { .. } | NodeKind::Component { .. } | NodeKind::Rectangle {} | NodeKind::Image { .. } | NodeKind::Instance { .. } | NodeKind::Note { .. } => {
            closed(rounded_rect(n))
        }
        NodeKind::Ellipse { arc } => {
            let donut = arc.is_some_and(|a| a.inner_ratio > 0.0 && a.sweep.abs() >= 360.0);
            Some(Outline { path: ellipse(w, h, *arc), rule: if donut { Fill::EvenOdd } else { Fill::NonZero }, closed: true })
        }
        NodeKind::Polygon { sides, star_inner_ratio } => closed(polygon(w, h, *sides, *star_inner_ratio)),
        NodeKind::Line {} => {
            let mut p = BezPath::new();
            p.move_to((0.0, h / 2.0));
            p.line_to((w, h / 2.0));
            Some(Outline { path: p, rule: Fill::NonZero, closed: false })
        }
        NodeKind::Path { paths } => {
            let mut p = BezPath::new();
            let mut rule = Fill::NonZero;
            let mut closed = true;
            for d in paths {
                if let Ok(b) = BezPath::from_svg(&d.d) {
                    closed &= b.elements().last().is_some_and(|e| matches!(e, kurbo::PathEl::ClosePath));
                    p.extend(b);
                }
                if d.fill_rule == FillRule::EvenOdd {
                    rule = Fill::EvenOdd;
                }
            }
            Some(Outline { path: p, rule, closed })
        }
        NodeKind::Page { .. } | NodeKind::Group {} | NodeKind::Text { .. } => None,
    }
}

/// The rectangle-like outline used for clipping children: frames, components and instances.
pub fn clip_path(n: &Node) -> BezPath {
    rounded_rect(n)
}

/// A ring outside `path`: `outer` minus the shape, filled even-odd.
pub fn outside_ring(path: &BezPath, outer: Rect) -> BezPath {
    let mut p = outer.to_path(TOLERANCE);
    p.extend(path.iter());
    p
}
