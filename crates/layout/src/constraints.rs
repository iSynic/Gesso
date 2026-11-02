//! Layout outside auto layout: constraints when a frame is resized, and group bounds.

use doc::model::{ConstraintH, ConstraintV, Constraints};

/// A constraint along one axis.
#[derive(Clone, Copy)]
enum Pin {
    Start,
    End,
    Both,
    Center,
    Scale,
}

/// How a child at `pos` with `size` follows its parent going from `old` to `new` along one
/// axis. Returns the new `(pos, size)`.
fn axis(pin: Pin, pos: f64, size: f64, old: f64, new: f64) -> (f64, f64) {
    match pin {
        Pin::Start => (pos, size),
        Pin::End => (pos + new - old, size),
        Pin::Both => (pos, (size + new - old).max(0.0)),
        Pin::Center => (pos + (new - old) / 2.0, size),
        Pin::Scale => {
            let k = if old == 0.0 { 1.0 } else { new / old };
            (pos * k, size * k)
        }
    }
}

/// The new rect `[x, y, w, h]` of a child with `constraints` when its parent goes from `old` to
/// `new` size.
pub fn constrain(c: Constraints, r: [f64; 4], old: (f64, f64), new: (f64, f64)) -> [f64; 4] {
    let h = match c.h {
        ConstraintH::Left => Pin::Start,
        ConstraintH::Right => Pin::End,
        ConstraintH::LeftRight => Pin::Both,
        ConstraintH::Center => Pin::Center,
        ConstraintH::Scale => Pin::Scale,
    };
    let v = match c.v {
        ConstraintV::Top => Pin::Start,
        ConstraintV::Bottom => Pin::End,
        ConstraintV::TopBottom => Pin::Both,
        ConstraintV::Center => Pin::Center,
        ConstraintV::Scale => Pin::Scale,
    };
    let (x, w) = axis(h, r[0], r[2], old.0, new.0);
    let (y, hh) = axis(v, r[1], r[3], old.1, new.1);
    [x, y, w, hh]
}

/// Union `[x0, y0, x1, y1]` of child rects given in the group's coordinates; `None` if empty.
/// Rotation is ignored (children's unrotated boxes are used).
pub fn union(rects: impl IntoIterator<Item = [f64; 4]>) -> Option<[f64; 4]> {
    rects.into_iter().fold(None, |acc, [x, y, w, h]| {
        let r = [x, y, x + w, y + h];
        Some(match acc {
            None => r,
            Some([a, b, c, d]) => [a.min(r[0]), b.min(r[1]), c.max(r[2]), d.max(r[3])],
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(h: ConstraintH, v: ConstraintV) -> Constraints {
        Constraints { h, v }
    }

    #[test]
    fn each_mode() {
        let r = [10.0, 20.0, 30.0, 40.0];
        let (old, new) = ((100.0, 100.0), (200.0, 150.0));
        assert_eq!(constrain(c(ConstraintH::Left, ConstraintV::Top), r, old, new), r);
        assert_eq!(constrain(c(ConstraintH::Right, ConstraintV::Bottom), r, old, new), [110.0, 70.0, 30.0, 40.0]);
        assert_eq!(constrain(c(ConstraintH::LeftRight, ConstraintV::TopBottom), r, old, new), [10.0, 20.0, 130.0, 90.0]);
        assert_eq!(constrain(c(ConstraintH::Center, ConstraintV::Center), r, old, new), [60.0, 45.0, 30.0, 40.0]);
        assert_eq!(constrain(c(ConstraintH::Scale, ConstraintV::Scale), r, old, new), [20.0, 30.0, 60.0, 60.0]);
    }

    #[test]
    fn union_of_rects() {
        assert_eq!(union([[10.0, 10.0, 5.0, 5.0], [-5.0, 12.0, 5.0, 10.0]]), Some([-5.0, 10.0, 15.0, 22.0]));
        assert_eq!(union([]), None);
    }
}
