//! The view onto a page: zoom and the world point at the viewport's top-left corner.

use kurbo::{Affine, Point, Rect, Vec2};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    pub zoom: f64,
    /// World point shown at screen (0, 0).
    pub origin: Point,
}

impl Default for Camera {
    fn default() -> Self {
        Camera { zoom: 1.0, origin: Point::ZERO }
    }
}

impl Camera {
    pub const MIN_ZOOM: f64 = 0.01;
    pub const MAX_ZOOM: f64 = 256.0;

    /// World → screen.
    pub fn affine(&self) -> Affine {
        Affine::scale(self.zoom) * Affine::translate(-self.origin.to_vec2())
    }

    pub fn to_world(&self, screen: Point) -> Point {
        self.origin + screen.to_vec2() / self.zoom
    }

    pub fn to_screen(&self, world: Point) -> Point {
        ((world - self.origin) * self.zoom).to_point()
    }

    /// The world rectangle a `size` (screen px) viewport shows.
    pub fn view(&self, size: Vec2) -> Rect {
        Rect::from_origin_size(self.origin, (size / self.zoom).to_size())
    }

    /// Multiplies the zoom by `factor`, keeping the world point under `screen` in place.
    pub fn zoom_about(&mut self, screen: Point, factor: f64) {
        let at = self.to_world(screen);
        self.zoom = (self.zoom * factor).clamp(Self::MIN_ZOOM, Self::MAX_ZOOM);
        self.origin = at - screen.to_vec2() / self.zoom;
    }

    /// Moves the view by a screen-space delta (content follows the pointer).
    pub fn pan(&mut self, screen_delta: Vec2) {
        self.origin -= screen_delta / self.zoom;
    }

    /// A camera showing `rect` centred in a `size` viewport with `margin` px around it, at no
    /// more than `max_zoom`.
    pub fn fit(rect: Rect, size: Vec2, margin: f64, max_zoom: f64) -> Camera {
        let avail = Vec2::new((size.x - 2.0 * margin).max(1.0), (size.y - 2.0 * margin).max(1.0));
        let zoom = if rect.width() > 0.0 && rect.height() > 0.0 {
            (avail.x / rect.width()).min(avail.y / rect.height())
        } else {
            1.0
        }
        .clamp(Self::MIN_ZOOM, max_zoom.min(Self::MAX_ZOOM));
        Camera { zoom, origin: rect.center() - size / (2.0 * zoom) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_zoom_about() {
        let mut c = Camera { zoom: 2.0, origin: Point::new(100.0, 50.0) };
        let p = Point::new(300.0, 200.0);
        let w = c.to_world(p);
        assert!((c.to_screen(w) - p).hypot() < 1e-9);
        assert!((c.affine() * w - p).hypot() < 1e-9);
        c.zoom_about(p, 1.5);
        assert!((c.to_world(p) - w).hypot() < 1e-9);
        assert_eq!(c.zoom, 3.0);
    }

    #[test]
    fn fit_centres() {
        let c = Camera::fit(Rect::new(0.0, 0.0, 1000.0, 500.0), Vec2::new(1100.0, 700.0), 50.0, 4.0);
        assert_eq!(c.zoom, 1.0);
        assert!((c.to_screen(Point::new(500.0, 250.0)) - Point::new(550.0, 350.0)).hypot() < 1e-9);
    }
}
