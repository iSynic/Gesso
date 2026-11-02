//! Synthetic "real-looking" design document: a grid of artboards, each holding a grid of UI
//! cards (shadowed rounded rects, avatars, thumbnails, icons, labels, buttons).

use crate::text::{Shaper, TextLayout};
use crate::Rng;
use rstar::primitives::{GeomWithData, Rectangle};
use rstar::{RTree, AABB};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }
    pub fn intersects(&self, o: &Rect) -> bool {
        self.x < o.x + o.w && o.x < self.x + self.w && self.y < o.y + o.h && o.y < self.y + self.h
    }
}

pub type Rgba = [u8; 4];

#[derive(Clone, Copy, Debug)]
pub struct Shadow {
    pub dx: f32,
    pub dy: f32,
    pub blur: f32,
    pub color: Rgba,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Rect,
    Ellipse,
    /// Index into [`Scene::icons`].
    Path(u16),
    /// Index into [`Scene::texts`].
    Text(u32),
    /// Index into [`Scene::images`].
    Image(u16),
}

#[derive(Clone, Debug)]
pub struct Item {
    pub rect: Rect,
    pub kind: Kind,
    pub fill: Rgba,
    pub stroke: Option<(Rgba, f32)>,
    pub radius: f32,
    pub shadow: Option<Shadow>,
    /// Gaussian layer blur std-dev in world units; 0 = none.
    pub blur: f32,
    pub opacity: f32,
}

#[derive(Clone, Copy, Debug)]
pub enum PathEl {
    Move(f32, f32),
    Line(f32, f32),
    Cubic(f32, f32, f32, f32, f32, f32),
    Close,
}

pub struct ImageData {
    pub width: u32,
    pub height: u32,
    /// Straight-alpha RGBA8 (all opaque).
    pub rgba: Vec<u8>,
}

pub struct Scene {
    pub items: Vec<Item>,
    /// Icon outlines in a 0..1 unit square.
    pub icons: Vec<Vec<PathEl>>,
    pub texts: Vec<TextLayout>,
    pub images: Vec<ImageData>,
    pub bounds: Rect,
    pub index: RTree<GeomWithData<Rectangle<[f64; 2]>, u32>>,
    /// Top-level artboards: bounds and contiguous item range (paint order).
    pub groups: Vec<(Rect, std::ops::Range<u32>)>,
    pub shape_time_ms: f64,
}

#[derive(Default, Debug, serde::Serialize)]
pub struct SceneStats {
    pub items: usize,
    pub rects: usize,
    pub ellipses: usize,
    pub paths: usize,
    pub texts: usize,
    pub images: usize,
    pub shadows: usize,
    pub blurs: usize,
    pub unique_text_layouts: usize,
}

pub const ARTBOARD_W: f64 = 1440.0;
pub const ARTBOARD_H: f64 = 1024.0;
pub const ARTBOARD_GAP: f64 = 100.0;

const WORDS_LATIN: &[&str] = &[
    "Dashboard", "Settings", "Revenue", "Active users", "Invite team", "Billing", "Overview",
    "Quarterly report", "Upload files", "Notifications", "Design system", "Release notes",
    "Search projects", "Weekly sync", "Analytics", "Get started", "Pricing", "Download",
];
const WORDS_CJK: &[&str] = &["設定", "ダッシュボード", "用户管理", "数据分析", "通知センター", "프로젝트"];
const WORDS_ARABIC: &[&str] = &["لوحة التحكم", "الإعدادات", "تقرير ربع سنوي", "مرحبا بالعالم"];
const SUBTITLES: &[&str] = &[
    "Updated 3 minutes ago", "12 new items since yesterday", "Shared with 4 people",
    "Last edited by the design team", "Draft · not published", "Synced with main branch",
];

fn palette(rng: &mut Rng) -> Rgba {
    const P: &[Rgba] = &[
        [37, 99, 235, 255], [16, 185, 129, 255], [245, 158, 11, 255], [239, 68, 68, 255],
        [139, 92, 246, 255], [14, 165, 233, 255], [236, 72, 153, 255], [100, 116, 139, 255],
    ];
    P[rng.below(P.len())]
}

fn make_icons() -> Vec<Vec<PathEl>> {
    use PathEl::*;
    let mut icons = Vec::new();
    // Star.
    let mut star = Vec::new();
    for i in 0..10 {
        let a = std::f32::consts::PI * 2.0 * i as f32 / 10.0 - std::f32::consts::FRAC_PI_2;
        let r = if i % 2 == 0 { 0.5 } else { 0.22 };
        let (x, y) = (0.5 + r * a.cos(), 0.5 + r * a.sin());
        star.push(if i == 0 { Move(x, y) } else { Line(x, y) });
    }
    star.push(Close);
    icons.push(star);
    // Heart (cubics).
    icons.push(vec![
        Move(0.5, 0.9),
        Cubic(0.1, 0.6, 0.0, 0.3, 0.25, 0.15),
        Cubic(0.4, 0.05, 0.5, 0.25, 0.5, 0.25),
        Cubic(0.5, 0.25, 0.6, 0.05, 0.75, 0.15),
        Cubic(1.0, 0.3, 0.9, 0.6, 0.5, 0.9),
        Close,
    ]);
    // Chevron.
    icons.push(vec![Move(0.3, 0.1), Line(0.75, 0.5), Line(0.3, 0.9), Line(0.2, 0.8), Line(0.55, 0.5), Line(0.2, 0.2), Close]);
    // Blob with many cubics (stress for flattening).
    let mut blob = Vec::new();
    let n = 12;
    for i in 0..=n {
        let a = std::f32::consts::TAU * i as f32 / n as f32;
        let r = 0.35 + 0.12 * ((i * 7) % 5) as f32 / 5.0;
        let (x, y) = (0.5 + r * a.cos(), 0.5 + r * a.sin());
        if i == 0 {
            blob.push(Move(x, y));
        } else {
            let a0 = std::f32::consts::TAU * (i as f32 - 0.66) / n as f32;
            let a1 = std::f32::consts::TAU * (i as f32 - 0.33) / n as f32;
            blob.push(Cubic(0.5 + 0.48 * a0.cos(), 0.5 + 0.48 * a0.sin(), 0.5 + 0.3 * a1.cos(), 0.5 + 0.3 * a1.sin(), x, y));
        }
    }
    blob.push(Close);
    icons.push(blob);
    icons
}

fn make_images(n: usize) -> Vec<ImageData> {
    (0..n)
        .map(|k| {
            let (w, h) = (256u32, 256u32);
            let mut rgba = Vec::with_capacity((w * h * 4) as usize);
            for y in 0..h {
                for x in 0..w {
                    let fx = x as f32 / w as f32;
                    let fy = y as f32 / h as f32;
                    let ring = (((fx - 0.5).powi(2) + (fy - 0.5).powi(2)).sqrt() * (8.0 + k as f32)).sin();
                    rgba.extend_from_slice(&[
                        (fx * 255.0) as u8,
                        ((ring * 0.5 + 0.5) * 200.0) as u8,
                        ((1.0 - fy) * 255.0) as u8 ^ (k as u8 * 20),
                        255,
                    ]);
                }
            }
            ImageData { width: w, height: h, rgba }
        })
        .collect()
}

pub struct SceneConfig {
    pub target_items: usize,
    pub seed: u64,
    pub shadow_ratio: f64,
    pub blur_ratio: f64,
}

impl Default for SceneConfig {
    fn default() -> Self {
        Self { target_items: 100_000, seed: 42, shadow_ratio: 0.5, blur_ratio: 0.02 }
    }
}

impl Scene {
    pub fn generate(cfg: &SceneConfig) -> Scene {
        Self::generate_with(cfg, &mut Shaper::default())
    }

    pub fn generate_with(cfg: &SceneConfig, shaper: &mut Shaper) -> Scene {
        let mut rng = Rng::new(cfg.seed);
        let t0 = web_time::Instant::now();
        // Unique text layouts: (string, size, weight).
        let mut texts = Vec::new();
        let mut title_ids = Vec::new();
        let mut sub_ids = Vec::new();
        let mut button_ids = Vec::new();
        let all_titles: Vec<&str> = WORDS_LATIN.iter().chain(WORDS_CJK).chain(WORDS_ARABIC).copied().collect();
        for t in &all_titles {
            title_ids.push(texts.len() as u32);
            texts.push(shaper.shape(t, 18.0, 600.0, None));
        }
        for t in SUBTITLES {
            sub_ids.push(texts.len() as u32);
            texts.push(shaper.shape(t, 13.0, 400.0, Some(190.0)));
        }
        for t in ["Open", "Share", "Approve", "إرسال", "開く"] {
            button_ids.push(texts.len() as u32);
            texts.push(shaper.shape(t, 13.0, 500.0, None));
        }
        let shape_time_ms = t0.elapsed().as_secs_f64() * 1e3;

        let icons = make_icons();
        let images = make_images(12);

        const COLS: usize = 6;
        const ROWS: usize = 5;
        let per_card = 8.0;
        let per_board = 1.0 + (COLS * ROWS) as f64 * per_card;
        let boards = (cfg.target_items as f64 / per_board).ceil() as usize;
        let grid = (boards as f64).sqrt().ceil() as usize;

        let mut items = Vec::with_capacity(cfg.target_items + 512);
        let card_w = 200.0;
        let card_h = 170.0;
        let gx = (ARTBOARD_W - COLS as f64 * card_w) / (COLS as f64 + 1.0);
        let gy = (ARTBOARD_H - ROWS as f64 * card_h) / (ROWS as f64 + 1.0);

        'outer: for b in 0..boards {
            let bx = (b % grid) as f64 * (ARTBOARD_W + ARTBOARD_GAP);
            let by = (b / grid) as f64 * (ARTBOARD_H + ARTBOARD_GAP);
            items.push(Item {
                rect: Rect::new(bx, by, ARTBOARD_W, ARTBOARD_H),
                kind: Kind::Rect,
                fill: [248, 250, 252, 255],
                stroke: Some(([226, 232, 240, 255], 1.0)),
                radius: 0.0,
                shadow: None,
                blur: 0.0,
                opacity: 1.0,
            });
            for c in 0..COLS * ROWS {
                if items.len() >= cfg.target_items {
                    break 'outer;
                }
                let cx = bx + gx + (c % COLS) as f64 * (card_w + gx);
                let cy = by + gy + (c / COLS) as f64 * (card_h + gy);
                let accent = palette(&mut rng);
                let base = |rect, kind, fill| Item { rect, kind, fill, stroke: None, radius: 0.0, shadow: None, blur: 0.0, opacity: 1.0 };
                // Card.
                let mut card = base(Rect::new(cx, cy, card_w, card_h), Kind::Rect, [255, 255, 255, 255]);
                card.radius = 12.0;
                card.stroke = Some(([226, 232, 240, 255], 1.0));
                if rng.chance(cfg.shadow_ratio) {
                    card.shadow = Some(Shadow { dx: 0.0, dy: 4.0, blur: 8.0, color: [15, 23, 42, 40] });
                }
                items.push(card);
                // Thumbnail image or avatar ellipse.
                if rng.chance(0.5) {
                    let mut img = base(Rect::new(cx + 12.0, cy + 12.0, 64.0, 64.0), Kind::Image(rng.below(images.len()) as u16), [255; 4]);
                    img.radius = 8.0;
                    if rng.chance(cfg.blur_ratio * 2.0) {
                        img.blur = 4.0;
                    }
                    items.push(img);
                } else {
                    let mut av = base(Rect::new(cx + 12.0, cy + 12.0, 64.0, 64.0), Kind::Ellipse, accent);
                    av.stroke = Some(([255, 255, 255, 255], 3.0));
                    items.push(av);
                }
                // Icon path.
                items.push(base(Rect::new(cx + card_w - 36.0, cy + 14.0, 22.0, 22.0), Kind::Path(rng.below(icons.len()) as u16), accent));
                // Title + subtitle.
                let t = title_ids[rng.below(title_ids.len())];
                let tl = &texts[t as usize];
                items.push(base(Rect::new(cx + 12.0, cy + 84.0, tl.width as f64, tl.height as f64), Kind::Text(t), [15, 23, 42, 255]));
                let s = sub_ids[rng.below(sub_ids.len())];
                let sl = &texts[s as usize];
                items.push(base(Rect::new(cx + 12.0, cy + 108.0, sl.width as f64, sl.height as f64), Kind::Text(s), [100, 116, 139, 255]));
                // Progress bar.
                let mut bar = base(Rect::new(cx + 100.0, cy + 30.0, 60.0 * rng.range(0.2, 1.0), 6.0), Kind::Rect, accent);
                bar.radius = 3.0;
                items.push(bar);
                // Button + label.
                let mut btn = base(Rect::new(cx + card_w - 92.0, cy + card_h - 40.0, 80.0, 28.0), Kind::Rect, accent);
                btn.radius = 6.0;
                btn.opacity = if rng.chance(0.1) { 0.6 } else { 1.0 };
                items.push(btn);
                let l = button_ids[rng.below(button_ids.len())];
                let ll = &texts[l as usize];
                items.push(base(
                    Rect::new(cx + card_w - 52.0 - ll.width as f64 / 2.0, cy + card_h - 26.0 - ll.height as f64 / 2.0, ll.width as f64, ll.height as f64),
                    Kind::Text(l),
                    [255, 255, 255, 255],
                ));
            }
        }

        let bounds = Rect::new(0.0, 0.0, grid as f64 * (ARTBOARD_W + ARTBOARD_GAP), (boards.div_ceil(grid)) as f64 * (ARTBOARD_H + ARTBOARD_GAP));
        let index = build_index(&items);
        let mut groups: Vec<(Rect, std::ops::Range<u32>)> = Vec::new();
        for (i, it) in items.iter().enumerate() {
            if it.rect.w == ARTBOARD_W && it.rect.h == ARTBOARD_H {
                let pad = 40.0;
                groups.push((Rect::new(it.rect.x - pad, it.rect.y - pad, it.rect.w + 2.0 * pad, it.rect.h + 2.0 * pad), i as u32..i as u32));
            }
            groups.last_mut().unwrap().1.end = i as u32 + 1;
        }
        Scene { items, icons, texts, images, bounds, index, groups, shape_time_ms }
    }

    pub fn stats(&self) -> SceneStats {
        let mut s = SceneStats { items: self.items.len(), unique_text_layouts: self.texts.len(), ..Default::default() };
        for it in &self.items {
            match it.kind {
                Kind::Rect => s.rects += 1,
                Kind::Ellipse => s.ellipses += 1,
                Kind::Path(_) => s.paths += 1,
                Kind::Text(_) => s.texts += 1,
                Kind::Image(_) => s.images += 1,
            }
            s.shadows += it.shadow.is_some() as usize;
            s.blurs += (it.blur > 0.0) as usize;
        }
        s
    }

    /// Visual bounds including shadow and blur spread.
    pub fn visual_bounds(it: &Item) -> Rect {
        let mut pad = it.blur as f64 * 3.0 + it.stroke.map_or(0.0, |s| s.1 as f64);
        if let Some(sh) = it.shadow {
            pad = pad.max(sh.blur as f64 * 3.0 + sh.dx.abs().max(sh.dy.abs()) as f64);
        }
        Rect::new(it.rect.x - pad, it.rect.y - pad, it.rect.w + 2.0 * pad, it.rect.h + 2.0 * pad)
    }

    pub fn group_of(&self, item: u32) -> usize {
        self.groups.partition_point(|g| g.1.end <= item)
    }

    /// Re-index one item after an edit (remove old envelope, insert new).
    pub fn update_index(&mut self, idx: u32, old: Rect) {
        let geom = |r: Rect| Rectangle::from_corners([r.x, r.y], [r.x + r.w, r.y + r.h]);
        self.index.remove(&GeomWithData::new(geom(old), idx));
        let nb = Self::visual_bounds(&self.items[idx as usize]);
        self.index.insert(GeomWithData::new(geom(nb), idx));
    }
}

fn build_index(items: &[Item]) -> RTree<GeomWithData<Rectangle<[f64; 2]>, u32>> {
    let elems = items
        .iter()
        .enumerate()
        .map(|(i, it)| {
            let r = Scene::visual_bounds(it);
            GeomWithData::new(Rectangle::from_corners([r.x, r.y], [r.x + r.w, r.y + r.h]), i as u32)
        })
        .collect();
    RTree::bulk_load(elems)
}

/// What a renderer should draw for one visible item after level-of-detail.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Lod {
    Full,
    /// Draw without shadow/blur/stroke (small on screen).
    Simple,
    /// Text too small to read: draw a translucent bar instead.
    Greek,
}

/// Per-item level of detail at a given zoom; `None` = too small to draw.
pub fn lod_at(it: &Item, zoom: f64) -> Option<Lod> {
    let px = it.rect.w.max(it.rect.h) * zoom;
    if px < 0.5 {
        return None;
    }
    Some(match it.kind {
        Kind::Text(_) if it.rect.h * zoom < 5.0 => Lod::Greek,
        _ if px < 6.0 => Lod::Simple,
        _ => Lod::Full,
    })
}

/// Zoom bands for cached artboard fragments: (band index, representative zoom used for LOD).
/// The representative zoom is the band's lower bound, so a fragment never shows less detail
/// than per-item LOD would at any zoom inside the band.
pub fn zoom_band(zoom: f64) -> (usize, f64) {
    if zoom >= 0.35 {
        (0, f64::INFINITY)
    } else if zoom >= 0.12 {
        (1, 0.12)
    } else {
        (2, 0.04)
    }
}

/// Culling + LOD with reusable output buffers (no per-frame allocation once warmed up).
#[derive(Default)]
pub struct Culler {
    pub visible: Vec<(u32, Lod)>,
    pub groups: Vec<u32>,
    scratch: Vec<u32>,
    pub use_rtree: bool,
}

impl Culler {
    pub fn new(use_rtree: bool) -> Self {
        Self { use_rtree, ..Default::default() }
    }

    /// Visible artboards (for retained per-artboard fragments).
    pub fn cull_groups(&mut self, scene: &Scene, view: Rect) {
        self.groups.clear();
        self.groups.extend(scene.groups.iter().enumerate().filter(|(_, g)| g.0.intersects(&view)).map(|(i, _)| i as u32));
    }

    pub fn cull(&mut self, scene: &Scene, view: Rect, zoom: f64) {
        self.scratch.clear();
        if self.use_rtree {
            let env = AABB::from_corners([view.x, view.y], [view.x + view.w, view.y + view.h]);
            self.scratch.extend(scene.index.locate_in_envelope_intersecting(env).map(|g| g.data));
            // Paint order = document order.
            self.scratch.sort_unstable();
        } else {
            self.scratch.extend(0..scene.items.len() as u32);
        }
        self.visible.clear();
        for &i in &self.scratch {
            if !self.use_rtree {
                self.visible.push((i, Lod::Full));
            } else if let Some(lod) = lod_at(&scene.items[i as usize], zoom) {
                self.visible.push((i, lod));
            }
        }
    }
}
