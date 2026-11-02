//! Text shaping with bundled fonts only, so results are identical on every machine.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use doc::model::{TextAlign, TextSpan, TextStyle};
use text::*;

fn style(size: f64) -> TextStyle {
    TextStyle { font_size: size, ..Default::default() }
}

fn span(t: &str) -> TextSpan {
    TextSpan { text: t.into(), ..Default::default() }
}

fn input<'a>(spans: &'a [TextSpan], style: &'a TextStyle) -> TextInput<'a> {
    TextInput { spans, style, align: TextAlign::Left }
}

fn engine() -> TextEngine {
    TextEngine::new(FontDb::bundled_only())
}

#[test]
fn bundled_inter_is_available_and_deterministic() {
    let mut db = FontDb::bundled_only();
    assert!(db.has_family("Inter"));
    assert!(!db.uses_system_fonts());
    let (s, st) = ([span("Hello, world")], style(16.0));
    let a = engine().shape(input(&s, &st), None);
    let b = engine().shape(input(&s, &st), None);
    assert_eq!((a.width, a.height, a.first_baseline), (b.width, b.height, b.first_baseline));
    // Inter 16px: "Hello, world" is roughly 90 px wide and one line of ~19 px.
    assert!((80.0..105.0).contains(&a.width), "width {}", a.width);
    assert!((17.0..22.0).contains(&a.height), "height {}", a.height);
    assert!(a.missing_families.is_empty());
    assert_eq!(a.line_count(), 1);
}

#[test]
fn spans_change_metrics() {
    let st = style(16.0);
    let plain = [span("Design canvas")];
    let bold = [TextSpan { font_weight: Some(800), ..span("Design canvas") }];
    let big = [span("Design "), TextSpan { font_size: Some(32.0), ..span("canvas") }];
    let mut e = engine();
    let p = e.shape(input(&plain, &st), None);
    let b = e.shape(input(&bold, &st), None);
    let g = e.shape(input(&big, &st), None);
    assert!(b.width > p.width, "bold {} vs regular {}", b.width, p.width);
    assert!(g.width > p.width && g.height > p.height);
    let lh = TextStyle { line_height: Some(40.0), ..style(16.0) };
    assert_eq!(e.shape(input(&plain, &lh), None).height, 40.0);
}

#[test]
fn wraps_at_width() {
    let st = style(14.0);
    let s = [span("The quick brown fox jumps over the lazy dog and keeps running across the canvas")];
    let t = engine().shape(input(&s, &st), Some(120.0));
    assert!(t.line_count() >= 4, "{} lines", t.line_count());
    assert!(t.width <= 120.0);
}

#[test]
fn rtl_runs_and_missing_families_are_reported() {
    let st = TextStyle { font_family: "No Such Family".into(), ..style(14.0) };
    let s = [span("Hello "), span("\u{0645}\u{0631}\u{062d}\u{0628}\u{0627}")];
    let t = engine().shape(input(&s, &st), None);
    assert!(t.has_rtl);
    assert_eq!(t.missing_families, vec!["No Such Family".to_string()]);
    assert!(t.width > 0.0, "fallback still shapes");
}

#[test]
fn caret_hit_and_selection_geometry() {
    let st = style(16.0);
    let s = [span("abcdef")];
    let t = engine().shape(input(&s, &st), None);
    for i in 0..=6 {
        let c = t.caret(i);
        let mid_y = ((c[1] + c[3]) / 2.0) as f32;
        assert_eq!(t.hit(c[0] as f32 + 0.1, mid_y), i, "caret {i} at {c:?}");
    }
    let sel = t.selection(1, 4);
    assert_eq!(sel.len(), 1);
    let (x0, x1) = (t.caret(1)[0], t.caret(4)[0]);
    assert!((sel[0][0] - x0).abs() < 0.01 && (sel[0][2] - x1).abs() < 0.01, "{sel:?} vs {x0}..{x1}");
}

#[test]
fn cache_hits_and_parallel_matches_sequential() {
    let st = style(13.0);
    let spans: Vec<[TextSpan; 1]> = (0..500).map(|i| [span(&format!("Label number {i}"))]).collect();
    let inputs: Vec<(TextInput, Option<f32>)> = spans.iter().map(|s| (input(s, &st), None)).collect();
    let mut seq = engine();
    let a: Vec<f32> = inputs.iter().map(|(i, w)| seq.shape(*i, *w).width).collect();
    let mut par = engine();
    let b: Vec<f32> = par.shape_many(&inputs).iter().map(|t| t.width).collect();
    assert_eq!(a, b);
    assert_eq!(par.stats().misses, 500);
    par.shape_many(&inputs);
    let s = par.stats();
    assert_eq!((s.hits, s.misses, s.entries), (500, 500, 500));
    // Different width is a different entry.
    par.shape(inputs[0].0, Some(50.0));
    assert_eq!(par.stats().entries, 501);
}

#[test]
fn measure_matches_shape_and_keeps_no_glyphs() {
    let st = style(14.0);
    let s = [span("Measured without keeping glyphs around")];
    let mut e = engine();
    let m = e.measure(input(&s, &st), Some(90.0));
    assert_eq!(e.stats().entries, 0);
    let t = e.shape(input(&s, &st), Some(90.0));
    assert_eq!(m, Metrics::from(&*t));
    let many: Vec<_> = (0..50).map(|i| [span(&format!("row {i}"))]).collect();
    let inputs: Vec<_> = many.iter().map(|s| (input(s, &st), None)).collect();
    e.measure_many(&inputs);
    assert_eq!((e.stats().entries, e.stats().metrics), (1, 51));
}

#[test]
fn glyph_runs_cover_the_text_and_decorations_sit_on_the_font_metrics() {
    use doc::model::TextDecoration;
    let st = style(20.0);
    let s = [span("Plain "), TextSpan { text: "under".into(), decoration: Some(TextDecoration::Underline), ..Default::default() }, TextSpan {
        text: " struck".into(),
        decoration: Some(TextDecoration::Strikethrough),
        ..Default::default()
    }];
    let t = engine().shape(input(&s, &st), None);
    let d = t.draw();
    let glyphs: usize = d.runs.iter().map(|r| r.glyphs.len()).sum();
    assert_eq!(glyphs, "Plain under struck".chars().count());
    assert!(d.runs.iter().all(|r| r.size == 20.0 && !r.font.data.as_ref().is_empty()));
    // Span indices come through as brushes.
    let spans: Vec<u32> = d.runs.iter().map(|r| r.span).collect();
    assert!(spans.contains(&0) && spans.contains(&1) && spans.contains(&2));
    // Glyphs advance left to right within the measured width.
    let xs: Vec<f32> = d.runs.iter().flat_map(|r| r.glyphs.iter().map(|g| g.1)).collect();
    assert!(xs.windows(2).all(|w| w[1] >= w[0]));
    assert!(*xs.last().unwrap() < t.width);
    // One underline below the baseline, one strikethrough above it, both on their spans.
    let base = t.first_baseline;
    let u: Vec<_> = d.decorations.iter().filter(|r| r.span == 1).collect();
    let k: Vec<_> = d.decorations.iter().filter(|r| r.span == 2).collect();
    assert_eq!((u.len(), k.len()), (1, 1), "{:?}", d.decorations);
    assert!(u[0].rect[1] > base && u[0].rect[1] < base + 5.0, "underline {:?} baseline {base}", u[0].rect);
    assert!(k[0].rect[3] < base && k[0].rect[1] > base - 15.0, "strike {:?} baseline {base}", k[0].rect);
    assert!(u[0].rect[3] - u[0].rect[1] > 0.5);
}

#[test]
fn vertical_alignment_offsets() {
    use doc::model::VerticalAlign;
    assert_eq!(vertical_offset(VerticalAlign::Top, 100.0, 20.0), 0.0);
    assert_eq!(vertical_offset(VerticalAlign::Middle, 100.0, 20.0), 40.0);
    assert_eq!(vertical_offset(VerticalAlign::Bottom, 100.0, 20.0), 80.0);
}
