//! Scripted, frame-count-driven scenario so every renderer does identical work.

use crate::scene::{Kind, Rect, Scene};
use crate::Rng;

#[derive(Clone, Copy, Debug)]
pub struct Camera {
    /// World point at the top-left of the viewport.
    pub x: f64,
    pub y: f64,
    pub zoom: f64,
}

impl Camera {
    pub fn view(&self, w: u32, h: u32) -> Rect {
        Rect::new(self.x, self.y, w as f64 / self.zoom, h as f64 / self.zoom)
    }
    /// world -> screen: s = (p - origin) * zoom
    pub fn affine(&self) -> [f64; 6] {
        [self.zoom, 0.0, 0.0, self.zoom, -self.x * self.zoom, -self.y * self.zoom]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum Phase {
    PanZoom1,
    Zoom,
    PanOverview,
    Edit,
}

pub struct Step {
    pub phase: Phase,
    pub camera: Camera,
    /// Item to edit this frame (fill + nudge), if any.
    pub edit: Option<u32>,
}

pub fn centered(scene: &Scene, w: u32, h: u32, zoom: f64, fx: f64, fy: f64) -> Camera {
    let cx = scene.bounds.x + scene.bounds.w * fx;
    let cy = scene.bounds.y + scene.bounds.h * fy;
    Camera { x: cx - w as f64 / zoom / 2.0, y: cy - h as f64 / zoom / 2.0, zoom }
}

/// Phases: 300 frames pan at 100%, 300 frames zoom 5%->800%, 300 frames pan at 15%
/// (thousands of visible nodes), 60 frames of edits at 100%.
pub fn build(scene: &Scene, w: u32, h: u32, frames_per_phase: usize) -> Vec<Step> {
    let n = frames_per_phase;
    let mut steps = Vec::new();
    for i in 0..n {
        let t = i as f64 / n as f64;
        let c = centered(scene, w, h, 1.0, 0.3 + 0.2 * t, 0.4 + 0.05 * t);
        steps.push(Step { phase: Phase::PanZoom1, camera: c, edit: None });
    }
    for i in 0..n {
        let t = i as f64 / (n - 1) as f64;
        let zoom = 0.05 * (8.0f64 / 0.05).powf(t);
        steps.push(Step { phase: Phase::Zoom, camera: centered(scene, w, h, zoom, 0.5, 0.5), edit: None });
    }
    for i in 0..n {
        let t = i as f64 / n as f64;
        steps.push(Step { phase: Phase::PanOverview, camera: centered(scene, w, h, 0.15, 0.2 + 0.6 * t, 0.5), edit: None });
    }
    // Edits target visible cards' rects/buttons around the 100% view.
    let cam = centered(scene, w, h, 1.0, 0.5, 0.5);
    let view = cam.view(w, h);
    let candidates: Vec<u32> = scene
        .items
        .iter()
        .enumerate()
        .filter(|(_, it)| it.kind == Kind::Rect && it.rect.w < 500.0 && it.rect.intersects(&view))
        .map(|(i, _)| i as u32)
        .collect();
    let mut rng = Rng::new(7);
    for _ in 0..60 {
        let e = (!candidates.is_empty()).then(|| candidates[rng.below(candidates.len())]);
        steps.push(Step { phase: Phase::Edit, camera: cam, edit: e });
    }
    steps
}

/// Apply a deterministic edit: recolor and nudge the item, then re-index it.
pub fn apply_edit(scene: &mut Scene, idx: u32, frame: usize) {
    let old = Scene::visual_bounds(&scene.items[idx as usize]);
    let it = &mut scene.items[idx as usize];
    it.fill = [(frame * 37 % 255) as u8, (frame * 91 % 255) as u8, 200, 255];
    it.rect.x += if frame % 2 == 0 { 2.0 } else { -2.0 };
    scene.update_index(idx, old);
}
