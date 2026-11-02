//! A node's placement in its parent (position, size, rotation, flips) and conversions between
//! placements and affine transforms.

use doc::{Node, NodeId, PropValue};
use index::SpatialIndex;
use kurbo::{Affine, Point, Rect, Vec2};

/// Rotation, position and size as stored on a node. Rotation and flips act about the centre.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// Degrees, clockwise.
    pub rotation: f64,
    pub flip_x: bool,
    pub flip_y: bool,
}

/// Wraps degrees into [-180, 180).
pub fn normalize_deg(r: f64) -> f64 {
    let r = (r + 180.0).rem_euclid(360.0) - 180.0;
    if r.abs() < 1e-9 { 0.0 } else { r }
}

/// Angle of a world-space linear map's x axis, in degrees clockwise (y down).
pub fn angle_of(a: Affine) -> f64 {
    let c = a.as_coeffs();
    c[1].atan2(c[0]).to_degrees()
}

/// The linear part's determinant is negative (an odd number of flips).
pub fn is_mirrored(a: Affine) -> bool {
    let c = a.as_coeffs();
    c[0] * c[3] - c[1] * c[2] < 0.0
}

/// Applies only the linear part of `a` to a vector.
pub fn linear(a: Affine, v: Vec2) -> Vec2 {
    let c = a.as_coeffs();
    Vec2::new(c[0] * v.x + c[2] * v.y, c[1] * v.x + c[3] * v.y)
}

impl Placement {
    pub fn of(n: &Node) -> Self {
        let p = &n.props;
        Placement { x: p.x, y: p.y, w: p.width, h: p.height, rotation: p.rotation, flip_x: p.flip_x, flip_y: p.flip_y }
    }

    /// Local box → parent space (the same as `index::local_transform`).
    pub fn local(&self) -> Affine {
        let (cx, cy) = (self.w / 2.0, self.h / 2.0);
        let mut t = Affine::translate((self.x, self.y));
        if self.rotation != 0.0 || self.flip_x || self.flip_y {
            let flip = Affine::scale_non_uniform(if self.flip_x { -1.0 } else { 1.0 }, if self.flip_y { -1.0 } else { 1.0 });
            t = t * Affine::translate((cx, cy)) * Affine::rotate(self.rotation.to_radians()) * flip * Affine::translate((-cx, -cy));
        }
        t
    }

    /// Centre in parent space.
    pub fn centre(&self) -> Point {
        Point::new(self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    /// The same placement moved so its centre is at `c` (parent space).
    pub fn with_centre(mut self, c: Point) -> Self {
        self.x = c.x - self.w / 2.0;
        self.y = c.y - self.h / 2.0;
        self
    }

    /// Placement of a `w`×`h` box whose transform to parent space is `a` (rotation and flips
    /// only, no scale). Keeps `prefer`'s flip on the y axis when a flip is needed.
    pub fn from_affine(a: Affine, w: f64, h: f64, prefer: &Placement) -> Self {
        let c = a.as_coeffs();
        let (rotation, flip_x, flip_y) = if !is_mirrored(a) {
            (c[1].atan2(c[0]).to_degrees(), false, false)
        } else if prefer.flip_y && !prefer.flip_x {
            // L = R·diag(1, -1): R's first column is L's first column.
            (c[1].atan2(c[0]).to_degrees(), false, true)
        } else {
            // L = R·diag(-1, 1): R's first column is minus L's first column.
            ((-c[1]).atan2(-c[0]).to_degrees(), true, false)
        };
        let centre = a * Point::new(w / 2.0, h / 2.0);
        Placement { x: 0.0, y: 0.0, w, h, rotation: normalize_deg(rotation), flip_x, flip_y }.with_centre(centre)
    }

    /// Property updates turning `old` into `self` (only what changed).
    pub fn diff(&self, old: &Placement) -> Vec<PropValue> {
        let mut v = Vec::new();
        let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
        if !close(self.x, old.x) {
            v.push(PropValue::X(self.x));
        }
        if !close(self.y, old.y) {
            v.push(PropValue::Y(self.y));
        }
        if !close(self.w, old.w) {
            v.push(PropValue::Width(self.w));
        }
        if !close(self.h, old.h) {
            v.push(PropValue::Height(self.h));
        }
        if !close(self.rotation, old.rotation) {
            v.push(PropValue::Rotation(self.rotation));
        }
        if self.flip_x != old.flip_x {
            v.push(PropValue::FlipX(self.flip_x));
        }
        if self.flip_y != old.flip_y {
            v.push(PropValue::FlipY(self.flip_y));
        }
        v
    }
}

/// World transform of a node's parent (identity for the page or a top-level node).
pub fn parent_world(index: &SpatialIndex, doc: &doc::Document, id: NodeId) -> Affine {
    crate::select::parent_of(doc, id).and_then(|p| index.world(p)).unwrap_or(Affine::IDENTITY)
}

/// World AABB of a node's own box (no stroke or effect outsets): what snapping aligns.
pub fn geometric_bounds(index: &SpatialIndex, n: &Node) -> Option<Rect> {
    let w = index.world(n.id)?;
    Some(index::world_bounds(w, index::local_box(n)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Affine, b: Affine) -> bool {
        a.as_coeffs().iter().zip(b.as_coeffs()).all(|(x, y)| (x - y).abs() < 1e-9)
    }

    #[test]
    fn from_affine_round_trips() {
        for (rot, fx, fy) in [(0.0, false, false), (30.0, false, false), (-120.0, true, false), (75.0, false, true), (10.0, true, true)] {
            let p = Placement { x: 12.0, y: -7.0, w: 40.0, h: 25.0, rotation: rot, flip_x: fx, flip_y: fy };
            let q = Placement::from_affine(p.local(), p.w, p.h, &p);
            assert!(close(p.local(), q.local()), "{p:?} -> {q:?}");
        }
    }

    #[test]
    fn normalize() {
        assert_eq!(normalize_deg(190.0), -170.0);
        assert_eq!(normalize_deg(-180.0), -180.0);
        assert_eq!(normalize_deg(360.0), 0.0);
    }
}
