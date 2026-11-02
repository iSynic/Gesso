//! Text spike: Parley (+fontique) vs cosmic-text on the same corpus.
//!
//! Measures cold font discovery, shaping+layout throughput for short labels and long paragraphs,
//! rich text (5 styled spans), per-keystroke edit latency in a 2,000-word paragraph, and which
//! fonts each stack picks for mixed-script / emoji text (fallback + bidi).

use common::{percentiles, write_json, Args, Host, Rng, Timer};
use serde_json::json;

const LATIN: &[&str] = &["design", "system", "layout", "canvas", "render", "quarterly", "report", "users", "billing", "settings", "team", "release"];
const MIXED: &str = "Revenue مرحبا بالعالم 数据分析 ダッシュボード 프로젝트 👍🏽 done";

fn words(rng: &mut Rng, n: usize) -> String {
    (0..n).map(|_| LATIN[rng.below(LATIN.len())]).collect::<Vec<_>>().join(" ")
}

fn median(mut f: impl FnMut() -> (), reps: usize) -> f64 {
    let mut v = Vec::new();
    for _ in 0..reps {
        let t = Timer::start();
        f();
        v.push(t.ms());
    }
    percentiles(v).p50
}

struct Corpus {
    labels: Vec<String>,
    paragraphs: Vec<String>,
    long: String,
}

fn corpus() -> Corpus {
    let mut rng = Rng::new(11);
    let labels = (0..10_000).map(|i| format!("{} {i}", words(&mut rng, 2))).collect();
    let paragraphs = (0..100).map(|i| if i % 10 == 0 { format!("{} {MIXED} {}", words(&mut rng, 80), words(&mut rng, 80)) } else { words(&mut rng, 200) }).collect();
    let long = words(&mut rng, 2000);
    Corpus { labels, paragraphs, long }
}

mod par {
    use super::*;
    use parley::{FontContext, FontFamily, FontWeight, LayoutContext, StyleProperty};

    pub struct P {
        pub fcx: FontContext,
        pub lcx: LayoutContext<()>,
    }

    pub fn layout(p: &mut P, text: &str, width: Option<f32>, rich: bool) -> (f32, usize) {
        let mut b = p.lcx.ranged_builder(&mut p.fcx, text, 1.0, true);
        b.push_default(StyleProperty::FontSize(14.0));
        b.push_default(StyleProperty::FontFamily(FontFamily::Source("system-ui, sans-serif".into())));
        if rich {
            // 5 styled spans spread over the text.
            let n = text.len();
            let cut = |k: usize| {
                let mut i = n * k / 6;
                while !text.is_char_boundary(i) {
                    i += 1;
                }
                i
            };
            let (a, b1, c, d, e, f) = (cut(0), cut(1), cut(2), cut(3), cut(4), cut(5));
            b.push(StyleProperty::FontWeight(FontWeight::new(700.0)), a..b1);
            b.push(StyleProperty::FontSize(20.0), b1..c);
            b.push(StyleProperty::FontStyle(parley::FontStyle::Italic), c..d);
            b.push(StyleProperty::Underline(true), d..e);
            b.push(StyleProperty::LetterSpacing(1.5), e..f);
        }
        let mut l: parley::Layout<()> = b.build(text);
        l.break_all_lines(width);
        l.align(parley::Alignment::Start, parley::AlignmentOptions::default());
        (l.height(), l.len())
    }

    pub fn fonts_used(p: &mut P, text: &str) -> Vec<String> {
        let mut b = p.lcx.ranged_builder(&mut p.fcx, text, 1.0, true);
        b.push_default(StyleProperty::FontSize(14.0));
        b.push_default(StyleProperty::FontFamily(FontFamily::Source("system-ui, sans-serif".into())));
        let mut l: parley::Layout<()> = b.build(text);
        l.break_all_lines(None);
        let mut out = Vec::new();
        for line in l.lines() {
            for item in line.items() {
                if let parley::PositionedLayoutItem::GlyphRun(g) = item {
                    let run = g.run();
                    let font = run.font();
                    let name = skrifa_name(font.data.data(), font.index);
                    out.push(format!("{name} (rtl={}, glyphs={})", run.is_rtl(), g.glyphs().count()));
                }
            }
        }
        out
    }

    fn skrifa_name(data: &[u8], index: u32) -> String {
        // Read the family name from the 'name' table with a minimal parser.
        crate::name_table::family(data, index).unwrap_or_else(|| "?".into())
    }
}

mod cos {
    use super::*;
    use cosmic_text::{Attrs, Buffer, Edit, Editor, Family, FontSystem, Metrics, Shaping, Style, Weight};

    pub fn layout(fs: &mut FontSystem, text: &str, width: Option<f32>, rich: bool) -> usize {
        let mut buf = Buffer::new(fs, Metrics::new(14.0, 18.0));
        buf.set_size(width, None);
        let attrs = Attrs::new().family(Family::SansSerif);
        if rich {
            let n = text.len();
            let cut = |k: usize| {
                let mut i = n * k / 6;
                while !text.is_char_boundary(i) {
                    i += 1;
                }
                i
            };
            let c: Vec<usize> = (0..6).map(cut).chain([n]).collect();
            let spans = [
                (&text[c[0]..c[1]], attrs.clone().weight(Weight::BOLD)),
                (&text[c[1]..c[2]], attrs.clone().metrics(Metrics::new(20.0, 24.0))),
                (&text[c[2]..c[3]], attrs.clone().style(Style::Italic)),
                (&text[c[3]..c[4]], attrs.clone().weight(Weight::SEMIBOLD)),
                (&text[c[4]..c[5]], attrs.clone().letter_spacing(1.5)),
                (&text[c[5]..c[6]], attrs.clone()),
            ];
            buf.set_rich_text(spans.iter().map(|(t, a)| (*t, a.clone())), &attrs, Shaping::Advanced, None);
        } else {
            buf.set_text(text, &attrs, Shaping::Advanced, None);
        }
        buf.shape_until_scroll(fs, false);
        buf.layout_runs().count()
    }

    pub fn fonts_used(fs: &mut FontSystem, text: &str) -> Vec<String> {
        let mut buf = Buffer::new(fs, Metrics::new(14.0, 18.0));
        buf.set_text(text, &Attrs::new().family(Family::SansSerif), Shaping::Advanced, None);
        buf.shape_until_scroll(fs, false);
        let mut out: Vec<String> = Vec::new();
        for run in buf.layout_runs() {
            let mut last = None;
            for g in run.glyphs {
                if last != Some(g.font_id) {
                    last = Some(g.font_id);
                    let name = fs.db().face(g.font_id).map(|f| f.families.first().map(|x| x.0.clone()).unwrap_or_default()).unwrap_or_default();
                    out.push(format!("{name} (rtl={})", run.rtl || g.level.is_rtl()));
                }
            }
        }
        out
    }

    /// Insert one char in the middle of a long paragraph and re-shape, as an editor would.
    pub fn edit_latency(fs: &mut FontSystem, text: &str, reps: usize) -> f64 {
        let mut buf = Buffer::new(fs, Metrics::new(14.0, 18.0));
        buf.set_size(Some(600.0), None);
        buf.set_text(text, &Attrs::new().family(Family::SansSerif), Shaping::Advanced, None);
        buf.shape_until_scroll(fs, false);
        let mut ed = Editor::new(buf);
        ed.set_cursor(cosmic_text::Cursor::new(0, text.len() / 2));
        let mut v = Vec::new();
        for i in 0..reps {
            let t = Timer::start();
            ed.insert_string(if i % 2 == 0 { "x" } else { "y" }, None);
            ed.shape_as_needed(fs, false);
            v.push(t.ms());
        }
        percentiles(v).p50
    }
}

mod name_table {
    /// Minimal OpenType 'name' table reader for family name (nameID 1), Windows Unicode only.
    pub fn family(data: &[u8], index: u32) -> Option<String> {
        let u16at = |o: usize| data.get(o..o + 2).map(|b| u16::from_be_bytes([b[0], b[1]]));
        let u32at = |o: usize| data.get(o..o + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
        let mut base = 0usize;
        if data.get(0..4)? == b"ttcf" {
            base = u32at(12 + 4 * index as usize)? as usize;
        }
        let n = u16at(base + 4)? as usize;
        let name_off = (0..n).find_map(|i| {
            let r = base + 12 + 16 * i;
            (data.get(r..r + 4)? == b"name").then(|| u32at(r + 8)).flatten()
        })? as usize;
        let count = u16at(name_off + 2)? as usize;
        let strings = name_off + u16at(name_off + 4)? as usize;
        for i in 0..count {
            let r = name_off + 6 + 12 * i;
            let (pid, nid, len, off) = (u16at(r)?, u16at(r + 6)?, u16at(r + 8)? as usize, u16at(r + 10)? as usize);
            if pid == 3 && nid == 1 {
                let b = data.get(strings + off..strings + off + len)?;
                let w: Vec<u16> = b.chunks(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
                return String::from_utf16(&w).ok();
            }
        }
        None
    }
}

fn main() {
    let args = Args::from_env();
    let c = corpus();
    let reps = 5;

    // Parley.
    let t = Timer::start();
    let mut p = par::P { fcx: parley::FontContext::new(), lcx: parley::LayoutContext::new() };
    par::layout(&mut p, "warm up", None, false);
    let p_cold = t.ms();
    let p_labels = median(|| for l in &c.labels { par::layout(&mut p, l, None, false); }, reps);
    let p_paras = median(|| for s in &c.paragraphs { par::layout(&mut p, s, Some(400.0), false); }, reps);
    let p_rich = median(|| for s in &c.paragraphs { par::layout(&mut p, s, Some(400.0), true); }, reps);
    // Parley has no incremental re-shaping: an edit rebuilds the paragraph's layout.
    let mut long = c.long.clone();
    let mid = long.len() / 2;
    let mut ev = Vec::new();
    for i in 0..50 {
        let t = Timer::start();
        long.insert(mid, if i % 2 == 0 { 'x' } else { 'y' });
        par::layout(&mut p, &long, Some(600.0), false);
        ev.push(t.ms());
    }
    let p_edit = percentiles(ev).p50;
    let p_fonts = par::fonts_used(&mut p, MIXED);

    // cosmic-text.
    let t = Timer::start();
    let mut fs = cosmic_text::FontSystem::new();
    cos::layout(&mut fs, "warm up", None, false);
    let c_cold = t.ms();
    let c_labels = median(|| for l in &c.labels { cos::layout(&mut fs, l, None, false); }, reps);
    let c_paras = median(|| for s in &c.paragraphs { cos::layout(&mut fs, s, Some(400.0), false); }, reps);
    let c_rich = median(|| for s in &c.paragraphs { cos::layout(&mut fs, s, Some(400.0), true); }, reps);
    let c_edit = cos::edit_latency(&mut fs, &c.long, 50);
    let c_fonts = cos::fonts_used(&mut fs, MIXED);

    eprintln!("{:<34} {:>10} {:>12}", "", "parley", "cosmic-text");
    for (name, a, b) in [
        ("cold font discovery + first shape", p_cold, c_cold),
        ("10k labels (ms)", p_labels, c_labels),
        ("100 paragraphs, 200 words, wrap", p_paras, c_paras),
        ("100 paragraphs, 5 rich spans", p_rich, c_rich),
        ("edit 2k-word paragraph (per key)", p_edit, c_edit),
    ] {
        eprintln!("{name:<34} {a:>10.2} {b:>12.2}");
    }
    eprintln!("parley fonts for mixed: {p_fonts:#?}");
    eprintln!("cosmic fonts for mixed: {c_fonts:#?}");
    write_json(
        args.value("--out"),
        &json!({
            "host": Host::detect(String::new(), "cpu".into()),
            "versions": {"parley": "0.12.0", "cosmic-text": "0.19.0"},
            "parley": {"cold_ms": p_cold, "labels_10k_ms": p_labels, "paragraphs_100_ms": p_paras, "rich_100_ms": p_rich, "edit_per_key_ms": p_edit, "mixed_runs": p_fonts},
            "cosmic_text": {"cold_ms": c_cold, "labels_10k_ms": c_labels, "paragraphs_100_ms": c_paras, "rich_100_ms": c_rich, "edit_per_key_ms": c_edit, "mixed_runs": c_fonts},
        }),
    );
}
