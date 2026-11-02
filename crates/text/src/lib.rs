//! Text shaping and measurement with Parley. Text is shaped once per distinct (content, style,
//! width) and cached; layout asks for sizes, the renderer (Stage 4) replays the cached glyph
//! runs, and editing asks for caret and selection geometry.
//!
//! Fonts: the bundled Inter (SIL OFL-1.1, `fonts/OFL.txt`) is always available, so the default
//! family measures the same on every machine. System fonts are added unless the engine is built
//! with [`FontDb::bundled_only`], which tests and golden images use.

use std::borrow::Cow;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, OnceLock};

use doc::NodeKind;
use doc::model::{TextAlign, TextDecoration, TextSpan, TextStyle, VerticalAlign};
use parley::fontique::{Blob, Collection, CollectionOptions, FamilyId, Script, SourceCache};
use parley::{
    Affinity, Alignment, AlignmentOptions, BoundingBox, Cursor, FontContext, FontData, FontFamily, FontFamilyName, FontStyle, FontWeight,
    Layout, LayoutContext, LineHeight, PositionedLayoutItem, Selection, StyleProperty,
};
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHasher};

const INTER: &[u8] = include_bytes!("../fonts/InterVariable.ttf");
/// The bundled Inter, for UI toolkits that want the same face as the canvas.
pub const INTER_TTF: &[u8] = INTER;
const INTER_ITALIC: &[u8] = include_bytes!("../fonts/InterVariable-Italic.ttf");
/// Family used when a requested family is missing; always available.
pub const FALLBACK_FAMILY: &str = "Inter";
/// The name the bundled variable font registers under. Documents say "Inter"; it always means
/// the bundled face, even when a static Inter is installed, so measurement stays identical.
const BUNDLED_INTER: &str = "Inter Variable";

fn canonical(name: &str) -> &str {
    if name.eq_ignore_ascii_case(FALLBACK_FAMILY) { BUNDLED_INTER } else { name }
}

/// The font collection: bundled fonts plus, optionally, the system's.
#[derive(Clone)]
pub struct FontDb {
    collection: Collection,
    system: bool,
}

impl FontDb {
    pub fn new(system_fonts: bool) -> Self {
        let mut collection = Collection::new(CollectionOptions { shared: true, system_fonts });
        for bytes in [INTER, INTER_ITALIC] {
            collection.register_fonts(Blob::new(Arc::new(bytes)), None);
        }
        if system_fonts {
            // Han text with no locale falls back to the platform's first CJK face (Yu Gothic on
            // Windows), which lacks many simplified-Chinese characters. Append the faces for each
            // Han locale so every character finds one.
            let hani = Script::from_bytes(*b"Hani");
            let mut extra: Vec<FamilyId> = Vec::new();
            for locale in ["zh-Hans", "zh-Hant", "ja", "ko"] {
                for f in collection.fallback_families((hani, locale)) {
                    if !extra.contains(&f) {
                        extra.push(f);
                    }
                }
            }
            collection.append_fallbacks(hani, extra.into_iter());
        }
        FontDb { collection, system: system_fonts }
    }

    /// Bundled fonts only: identical results on every machine.
    pub fn bundled_only() -> Self {
        Self::new(false)
    }

    pub fn uses_system_fonts(&self) -> bool {
        self.system
    }

    /// Registers extra font bytes (e.g. fonts shipped with a document). Returns family names.
    pub fn register(&mut self, bytes: Vec<u8>) -> Vec<String> {
        let fams = self.collection.register_fonts(Blob::new(Arc::new(bytes)), None);
        fams.iter().filter_map(|(id, _)| self.collection.family_name(*id).map(str::to_string)).collect()
    }

    pub fn has_family(&mut self, name: &str) -> bool {
        self.collection.family_id(canonical(name)).is_some()
    }

    fn context(&self) -> FontContext {
        FontContext { collection: self.collection.clone(), source_cache: SourceCache::new_shared() }
    }
}

/// The text-bearing parts of a text node.
#[derive(Debug, Clone, Copy)]
pub struct TextInput<'a> {
    pub spans: &'a [TextSpan],
    pub style: &'a TextStyle,
    pub align: TextAlign,
}

impl<'a> TextInput<'a> {
    pub fn from_kind(kind: &'a NodeKind) -> Option<Self> {
        match kind {
            NodeKind::Text { spans, style, align, .. } => Some(TextInput { spans, style, align: *align }),
            _ => None,
        }
    }

    /// Cache key: content, styles, alignment and the wrap width.
    pub fn key(&self, max_width: Option<f32>) -> u64 {
        let mut h = FxHasher::default();
        hash_style(&mut h, self.style);
        (self.align as u8).hash(&mut h);
        max_width.map(f32::to_bits).hash(&mut h);
        for s in self.spans {
            s.text.hash(&mut h);
            s.font_family.hash(&mut h);
            s.font_size.map(f64::to_bits).hash(&mut h);
            s.font_weight.hash(&mut h);
            s.italic.hash(&mut h);
            s.decoration.map(|d| d as u8).hash(&mut h);
            s.color.hash(&mut h);
        }
        h.finish()
    }
}

fn hash_style(h: &mut FxHasher, s: &TextStyle) {
    s.font_family.hash(h);
    s.font_size.to_bits().hash(h);
    s.font_weight.hash(h);
    s.italic.hash(h);
    s.line_height.map(f64::to_bits).hash(h);
    s.letter_spacing.to_bits().hash(h);
    (s.decoration as u8).hash(h);
    s.color.hash(h);
}

/// A shaped, line-broken text layout. The brush of each glyph run is the index of its span.
pub struct ShapedText {
    pub layout: Layout<u32>,
    /// Width of the widest line.
    pub width: f32,
    pub height: f32,
    /// Distance from the top to the first line's baseline.
    pub first_baseline: f32,
    /// Requested families that are not installed (fallback was used).
    pub missing_families: Vec<String>,
    /// True if any run is right-to-left.
    pub has_rtl: bool,
    /// The source text, all spans joined.
    pub text: Arc<str>,
    draw: OnceLock<TextDraw>,
}

/// One run of glyphs in one font, size and span, ready to draw.
#[derive(Clone)]
pub struct GlyphRun {
    pub font: FontData,
    pub size: f32,
    /// Variation coordinates (F2Dot14 bits) for variable fonts.
    pub coords: Vec<i16>,
    /// Synthetic oblique angle in degrees, when the font has no italic.
    pub skew: Option<f32>,
    /// Synthetic bold, when the font has no such weight.
    pub embolden: bool,
    /// Index of the span the run belongs to (selects its paint).
    pub span: u32,
    /// `(glyph id, x, y)` with `y` on the baseline, in layout coordinates.
    pub glyphs: Vec<(u32, f32, f32)>,
    /// `(glyph id, text)`: the source text of each cluster, on the cluster's first glyph (a
    /// ligature's glyph carries all its letters). For selectable text in exports.
    pub chars: Vec<(u32, Box<str>)>,
}

impl std::fmt::Debug for GlyphRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlyphRun").field("size", &self.size).field("span", &self.span).field("glyphs", &self.glyphs.len()).finish()
    }
}

/// An underline or strikethrough bar `[x0, y0, x1, y1]` in layout coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DecorationRect {
    pub span: u32,
    pub rect: [f32; 4],
}

/// What the renderer draws for a text.
#[derive(Clone, Default)]
pub struct TextDraw {
    pub runs: Vec<Arc<GlyphRun>>,
    pub decorations: Vec<DecorationRect>,
}

/// Vertical offset of a text of height `text_h` inside a box of height `box_h`.
pub fn vertical_offset(align: VerticalAlign, box_h: f32, text_h: f32) -> f32 {
    match align {
        VerticalAlign::Top => 0.0,
        VerticalAlign::Middle => (box_h - text_h) / 2.0,
        VerticalAlign::Bottom => box_h - text_h,
    }
}

impl ShapedText {
    /// Byte index of the caret position nearest to a point in layout coordinates.
    pub fn hit(&self, x: f32, y: f32) -> usize {
        Cursor::from_point(&self.layout, x, y).index()
    }

    /// Caret rectangle `[x0, y0, x1, y1]` before byte `index`.
    pub fn caret(&self, index: usize) -> [f64; 4] {
        bbox(Cursor::from_byte_index(&self.layout, index, Affinity::Downstream).geometry(&self.layout, 1.0))
    }

    /// Highlight rectangles for the byte range `start..end`, one or more per line.
    pub fn selection(&self, start: usize, end: usize) -> Vec<[f64; 4]> {
        let a = Cursor::from_byte_index(&self.layout, start, Affinity::Downstream);
        let b = Cursor::from_byte_index(&self.layout, end, Affinity::Upstream);
        Selection::new(a, b).geometry(&self.layout).into_iter().map(|(r, _)| bbox(r)).collect()
    }

    pub fn line_count(&self) -> usize {
        self.layout.len()
    }

    /// Glyph runs and decoration bars, extracted once per shaped text.
    pub fn draw(&self) -> &TextDraw {
        self.draw.get_or_init(|| extract(&self.layout, &self.text))
    }
}

fn extract(layout: &Layout<u32>, text: &str) -> TextDraw {
    let mut out = TextDraw::default();
    for line in layout.lines() {
        for item in line.items() {
            let PositionedLayoutItem::GlyphRun(gr) = item else { continue };
            let run = gr.run();
            let style = gr.style();
            let span = style.brush;
            let synth = run.synthesis();
            out.runs.push(Arc::new(GlyphRun {
                font: run.font().clone(),
                size: run.font_size(),
                coords: run.normalized_coords().iter().map(|c| c.to_bits()).collect(),
                skew: synth.skew(),
                embolden: synth.embolden(),
                span,
                glyphs: gr.positioned_glyphs().map(|g| (g.id, g.x, g.y)).collect(),
                chars: cluster_chars(run, gr.style_index(), text),
            }));
            let m = run.font_metrics();
            let (x0, x1, base) = (gr.offset(), gr.offset() + gr.advance(), gr.baseline());
            // Offsets are measured upward from the baseline to the bar's top edge.
            let mut bar = |d: &parley::layout::Decoration<u32>, offset: f32, size: f32| {
                let top = base - d.offset.unwrap_or(offset);
                out.decorations.push(DecorationRect { span: d.brush, rect: [x0, top, x1, top + d.size.unwrap_or(size)] });
            };
            if let Some(d) = &style.underline {
                bar(d, m.underline_offset, m.underline_size);
            }
            if let Some(d) = &style.strikethrough {
                bar(d, m.strikethrough_offset, m.strikethrough_size);
            }
        }
    }
    out
}

/// Cluster text by first glyph, for the clusters of `run` in style `style`. Clusters with no
/// glyphs of their own (ligature continuations) add their text to the previous cluster's.
fn cluster_chars(run: &parley::Run<'_, u32>, style: u16, text: &str) -> Vec<(u32, Box<str>)> {
    let mut out: Vec<(u32, String)> = Vec::new();
    for c in run.clusters().filter(|c| c.style_index() == style) {
        let t = text.get(c.text_range()).unwrap_or_default();
        match (c.glyphs().next(), out.last_mut()) {
            (Some(g), _) => out.push((g.id, t.to_string())),
            (None, Some(last)) => last.1.push_str(t),
            (None, None) => {}
        }
    }
    out.into_iter().map(|(g, t)| (g, t.into_boxed_str())).collect()
}

fn bbox(b: BoundingBox) -> [f64; 4] {
    [b.x0, b.y0, b.x1, b.y1]
}

/// The size of shaped text: all layout needs. Kept for every text measured, unlike full
/// [`ShapedText`] layouts, which hold glyphs and are kept only for text being drawn or edited.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    pub width: f32,
    pub height: f32,
    pub first_baseline: f32,
}

impl From<&ShapedText> for Metrics {
    fn from(t: &ShapedText) -> Self {
        Metrics { width: t.width, height: t.height, first_baseline: t.first_baseline }
    }
}

struct Entry {
    text: Arc<ShapedText>,
    used: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    /// Full layouts held.
    pub entries: usize,
    /// Sizes held.
    pub metrics: usize,
}

/// Full layouts kept before old ones are dropped at the next generation.
const MAX_LAYOUTS: usize = 4096;

/// Shapes text with a cache. One engine per document session.
pub struct TextEngine {
    db: FontDb,
    fcx: FontContext,
    lcx: LayoutContext<u32>,
    cache: FxHashMap<u64, Entry>,
    metrics: FxHashMap<u64, Metrics>,
    generation: u64,
    hits: u64,
    misses: u64,
}

impl TextEngine {
    pub fn new(db: FontDb) -> Self {
        let fcx = db.context();
        TextEngine { db, fcx, lcx: LayoutContext::new(), cache: FxHashMap::default(), metrics: FxHashMap::default(), generation: 0, hits: 0, misses: 0 }
    }

    pub fn fonts(&mut self) -> &mut FontDb {
        &mut self.db
    }

    pub fn stats(&self) -> CacheStats {
        CacheStats { hits: self.hits, misses: self.misses, entries: self.cache.len(), metrics: self.metrics.len() }
    }

    /// Shapes `input`, wrapping at `max_width` (`None` = one line per paragraph).
    pub fn shape(&mut self, input: TextInput<'_>, max_width: Option<f32>) -> Arc<ShapedText> {
        let key = input.key(max_width);
        if let Some(e) = self.cache.get_mut(&key) {
            e.used = self.generation;
            self.hits += 1;
            return e.text.clone();
        }
        self.misses += 1;
        let text = Arc::new(shape_with(&mut self.fcx, &mut self.lcx, input, max_width));
        self.metrics.insert(key, Metrics::from(&*text));
        self.cache.insert(key, Entry { text: text.clone(), used: self.generation });
        text
    }

    /// `input` wrapped at `max_width` if it is already shaped (read-only; for parallel readers).
    pub fn cached(&self, input: TextInput<'_>, max_width: Option<f32>) -> Option<Arc<ShapedText>> {
        self.cache.get(&input.key(max_width)).map(|e| e.text.clone())
    }

    /// The size of `input` wrapped at `max_width`, without keeping the glyphs.
    pub fn measure(&mut self, input: TextInput<'_>, max_width: Option<f32>) -> Metrics {
        let key = input.key(max_width);
        if let Some(m) = self.metrics.get(&key) {
            self.hits += 1;
            return *m;
        }
        self.misses += 1;
        let m = Metrics::from(&shape_with(&mut self.fcx, &mut self.lcx, input, max_width));
        self.metrics.insert(key, m);
        m
    }

    /// Measures many inputs, misses in parallel, keeping only sizes. Used when a document opens.
    pub fn measure_many(&mut self, inputs: &[(TextInput<'_>, Option<f32>)]) {
        let mut seen = rustc_hash::FxHashSet::default();
        let todo: Vec<(u64, usize)> =
            inputs.iter().enumerate().map(|(i, (t, w))| (t.key(*w), i)).filter(|(k, _)| !self.metrics.contains_key(k) && seen.insert(*k)).collect();
        let db = &self.db;
        let shaped: Vec<(u64, Metrics)> = todo
            .par_iter()
            .map_init(|| (db.context(), LayoutContext::new()), |(fcx, lcx), &(k, i)| (k, Metrics::from(&shape_with(fcx, lcx, inputs[i].0, inputs[i].1))))
            .collect();
        self.misses += shaped.len() as u64;
        self.hits += (inputs.len() - shaped.len()) as u64;
        self.metrics.extend(shaped);
    }

    /// Shapes many inputs, cache misses in parallel. Results are in input order.
    pub fn shape_many(&mut self, inputs: &[(TextInput<'_>, Option<f32>)]) -> Vec<Arc<ShapedText>> {
        let keys: Vec<u64> = inputs.iter().map(|(i, w)| i.key(*w)).collect();
        let mut todo: Vec<usize> = Vec::new();
        let mut seen = rustc_hash::FxHashSet::default();
        for (i, k) in keys.iter().enumerate() {
            if !self.cache.contains_key(k) && seen.insert(*k) {
                todo.push(i);
            }
        }
        let db = &self.db;
        let shaped: Vec<(u64, ShapedText)> = todo
            .par_iter()
            .map_init(|| (db.context(), LayoutContext::new()), |(fcx, lcx), &i| (keys[i], shape_with(fcx, lcx, inputs[i].0, inputs[i].1)))
            .collect();
        self.misses += shaped.len() as u64;
        self.hits += (inputs.len() - shaped.len()) as u64;
        for (k, t) in shaped {
            self.metrics.insert(k, Metrics::from(&t));
            self.cache.insert(k, Entry { text: Arc::new(t), used: self.generation });
        }
        let generation = self.generation;
        keys.iter()
            .filter_map(|k| {
                self.cache.get_mut(k).map(|e| {
                    e.used = generation;
                    e.text.clone()
                })
            })
            .collect()
    }

    /// Starts a new cache generation; full layouts not used in the last `keep` generations are
    /// dropped, sooner if there are more than [`MAX_LAYOUTS`]. Call once per transaction or
    /// frame. Sizes are small and kept until [`TextEngine::clear_cache`].
    pub fn next_generation(&mut self, keep: u64) {
        self.generation += 1;
        let g = self.generation;
        if g.is_multiple_of(64) || self.cache.len() > MAX_LAYOUTS {
            let keep = if self.cache.len() > MAX_LAYOUTS { 0 } else { keep };
            self.cache.retain(|_, e| g - e.used <= keep);
        }
    }

    pub fn clear_cache(&mut self) {
        self.cache.clear();
        self.metrics.clear();
    }
}

fn family<'a>(name: &'a str) -> FontFamily<'a> {
    FontFamily::List(Cow::Owned(vec![FontFamilyName::Named(Cow::Borrowed(canonical(name))), FontFamilyName::Named(Cow::Borrowed(BUNDLED_INTER))]))
}

fn shape_with(fcx: &mut FontContext, lcx: &mut LayoutContext<u32>, input: TextInput<'_>, max_width: Option<f32>) -> ShapedText {
    let text: String = input.spans.iter().map(|s| s.text.as_str()).collect();
    let st = input.style;
    let mut missing: Vec<String> = Vec::new();
    let mut check = |fcx: &mut FontContext, name: &str| {
        if !missing.iter().any(|m| m == name) && fcx.collection.family_id(canonical(name)).is_none() {
            missing.push(name.to_string());
        }
    };
    check(fcx, &st.font_family);
    for s in input.spans {
        if let Some(f) = &s.font_family {
            check(fcx, f);
        }
    }
    let mut b = lcx.ranged_builder(fcx, &text, 1.0, true);
    b.push_default(StyleProperty::FontFamily(family(&st.font_family)));
    b.push_default(StyleProperty::FontSize(st.font_size as f32));
    b.push_default(StyleProperty::FontWeight(FontWeight::new(st.font_weight as f32)));
    if st.italic {
        b.push_default(StyleProperty::FontStyle(FontStyle::Italic));
    }
    b.push_default(StyleProperty::LineHeight(match st.line_height {
        Some(px) => LineHeight::Absolute(px as f32),
        None => LineHeight::MetricsRelative(1.0),
    }));
    if st.letter_spacing != 0.0 {
        b.push_default(StyleProperty::LetterSpacing(st.letter_spacing as f32));
    }
    match st.decoration {
        TextDecoration::Underline => b.push_default(StyleProperty::Underline(true)),
        TextDecoration::Strikethrough => b.push_default(StyleProperty::Strikethrough(true)),
        TextDecoration::None => {}
    }
    let mut start = 0usize;
    for (i, s) in input.spans.iter().enumerate() {
        let r = start..start + s.text.len();
        start = r.end;
        if r.is_empty() {
            continue;
        }
        b.push(StyleProperty::Brush(i as u32), r.clone());
        if let Some(f) = &s.font_family {
            b.push(StyleProperty::FontFamily(family(f)), r.clone());
        }
        if let Some(v) = s.font_size {
            b.push(StyleProperty::FontSize(v as f32), r.clone());
        }
        if let Some(w) = s.font_weight {
            b.push(StyleProperty::FontWeight(FontWeight::new(w as f32)), r.clone());
        }
        if let Some(it) = s.italic {
            b.push(StyleProperty::FontStyle(if it { FontStyle::Italic } else { FontStyle::Normal }), r.clone());
        }
        if let Some(d) = s.decoration {
            b.push(StyleProperty::Underline(d == TextDecoration::Underline), r.clone());
            b.push(StyleProperty::Strikethrough(d == TextDecoration::Strikethrough), r.clone());
        }
    }
    let mut layout: Layout<u32> = b.build(&text);
    layout.break_all_lines(max_width);
    let alignment = match input.align {
        TextAlign::Left => Alignment::Left,
        TextAlign::Center => Alignment::Center,
        TextAlign::Right => Alignment::Right,
        TextAlign::Justify => Alignment::Justify,
    };
    layout.align(alignment, AlignmentOptions::default());
    let first_baseline = layout.lines().next().map_or(0.0, |l| l.metrics().baseline);
    let has_rtl = layout.lines().any(|l| {
        l.items().any(|it| matches!(it, PositionedLayoutItem::GlyphRun(g) if g.run().is_rtl()))
    });
    ShapedText { width: layout.width(), height: layout.height(), first_baseline, missing_families: missing, has_rtl, text: text.into(), layout, draw: OnceLock::new() }
}
