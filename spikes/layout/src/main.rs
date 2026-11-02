//! Layout spike: Taffy on a ~10k-node nested auto-layout document.
//!
//! Tree: root column → 100 sections (60 wrapping rows, 40 3-column grids) → 10 cards each
//! (vertical auto layout, hug height, fill width) → 9 leaves (text with a wrapping measure
//! function, icons, buttons, a nested row). Measures full layout, incremental re-layout after one
//! leaf change, a root width change (everything re-wraps) and a hug/fill-heavy worst case.

use common::{percentiles, write_json, Args, Host, Percentiles, Timer};
use serde_json::json;
use taffy::prelude::*;
use taffy::LayoutOutput;

/// Leaf context: simulated text with this many characters (0 = fixed-size box).
#[derive(Clone, Copy)]
struct Text(u32);

fn measure(known: Size<Option<f32>>, avail: Size<AvailableSpace>, ctx: Option<&mut Text>) -> Size<f32> {
    let Some(&mut Text(chars)) = ctx else { return Size::ZERO };
    if chars == 0 {
        return Size::ZERO;
    }
    let natural = chars as f32 * 7.0;
    let max = match avail.width {
        AvailableSpace::Definite(w) => w,
        AvailableSpace::MinContent => 60.0,
        AvailableSpace::MaxContent => f32::INFINITY,
    };
    let w = known.width.unwrap_or(natural.min(max).max(7.0));
    let lines = (natural / w).ceil().max(1.0);
    Size { width: w, height: known.height.unwrap_or(lines * 18.0) }
}

struct Doc {
    t: TaffyTree<Text>,
    root: NodeId,
    leaves: Vec<NodeId>,
    nodes: usize,
}

fn build(fill_heavy: bool) -> Doc {
    let mut t: TaffyTree<Text> = TaffyTree::new();
    let mut leaves = Vec::new();
    let mut nodes = 0;
    let mut sections = Vec::new();
    for s in 0..100 {
        let mut cards = Vec::new();
        for c in 0..10 {
            let mut kids = Vec::new();
            let text = |_chars| Style { size: Size { width: if fill_heavy { percent(1.0) } else { auto() }, height: auto() }, ..Default::default() };
            // Title, subtitle, body (wrapping text leaves).
            for chars in [12 + (s + c) as u32 % 10, 30, 90 + (c as u32 * 7) % 60] {
                let id = t.new_leaf_with_context(text(chars), Text(chars)).unwrap();
                leaves.push(id);
                kids.push(id);
            }
            // A nested row: icon, label, spacer (fill), button.
            let icon = t.new_leaf(Style { size: Size { width: length(20.0), height: length(20.0) }, ..Default::default() }).unwrap();
            let label = t.new_leaf_with_context(Style::default(), Text(8)).unwrap();
            let spacer = t.new_leaf(Style { flex_grow: 1.0, ..Default::default() }).unwrap();
            let button = t.new_leaf_with_context(Style { padding: Rect { left: length(12.0), right: length(12.0), top: length(6.0), bottom: length(6.0) }, ..Default::default() }, Text(6)).unwrap();
            let row = t
                .new_with_children(
                    Style { flex_direction: FlexDirection::Row, gap: Size { width: length(8.0), height: zero() }, align_items: AlignItems::CENTER, ..Default::default() },
                    &[icon, label, spacer, button],
                )
                .unwrap();
            leaves.extend([icon, label, button]);
            kids.push(row);
            let card = t
                .new_with_children(
                    Style {
                        flex_direction: FlexDirection::Column,
                        gap: Size { width: zero(), height: length(8.0) },
                        padding: Rect { left: length(16.0), right: length(16.0), top: length(16.0), bottom: length(16.0) },
                        size: Size { width: if fill_heavy { auto() } else { length(220.0) }, height: auto() },
                        flex_grow: if fill_heavy { 1.0 } else { 0.0 },
                        flex_basis: if fill_heavy { length(200.0) } else { auto() },
                        min_size: Size { width: length(160.0), height: auto() },
                        ..Default::default()
                    },
                    &kids,
                )
                .unwrap();
            nodes += kids.len() + 4 + 1;
            cards.push(card);
        }
        let section = if s % 5 < 3 {
            t.new_with_children(
                Style { flex_direction: FlexDirection::Row, flex_wrap: FlexWrap::Wrap, gap: length(16.0), padding: length(24.0), ..Default::default() },
                &cards,
            )
            .unwrap()
        } else {
            t.new_with_children(
                Style {
                    display: Display::Grid,
                    grid_template_columns: vec![fr(1.0), fr(1.0), fr(1.0)],
                    gap: length(16.0),
                    padding: length(24.0),
                    ..Default::default()
                },
                &cards,
            )
            .unwrap()
        };
        nodes += 1;
        sections.push(section);
    }
    let root = t.new_with_children(Style { flex_direction: FlexDirection::Column, size: Size { width: length(1440.0), height: auto() }, gap: length(32.0), ..Default::default() }, &sections).unwrap();
    Doc { t, root, leaves, nodes: nodes + 1 }
}

fn layout(d: &mut Doc, width: f32) {
    d.t.compute_layout_with_measure(d.root, Size { width: AvailableSpace::Definite(width), height: AvailableSpace::MaxContent }, |input, _id, ctx, _s| LayoutOutput::from_outer_size(measure(input.known_dimensions, input.available_space, ctx))).unwrap();
}

fn bench(fill_heavy: bool) -> serde_json::Value {
    let reps = 30;
    let t = Timer::start();
    let mut d = build(fill_heavy);
    let build_ms = t.ms();
    // Full: fresh tree each time (no caches).
    let mut full = Vec::new();
    for _ in 0..10 {
        let mut fresh = build(fill_heavy);
        let t = Timer::start();
        layout(&mut fresh, 1440.0);
        full.push(t.ms());
    }
    layout(&mut d, 1440.0);
    // Incremental: change one leaf's text length (mark_dirty walks to the root).
    let mut inc = Vec::new();
    let mut deep = Vec::new();
    for i in 0..reps {
        let leaf = d.leaves[(i * 7919) % d.leaves.len()];
        if let Some(c) = d.t.get_node_context_mut(leaf) {
            *c = Text(10 + (i as u32 * 13) % 120);
        }
        d.t.mark_dirty(leaf).unwrap();
        let t = Timer::start();
        layout(&mut d, 1440.0);
        inc.push(t.ms());
        // Fixed-size change: a style change on an icon.
        let icon = d.leaves[((i * 104729) % (d.leaves.len() / 6)) * 6 + 3];
        let mut st = d.t.style(icon).unwrap().clone();
        st.size.width = length(20.0 + (i % 3) as f32);
        d.t.set_style(icon, st).unwrap();
        let t = Timer::start();
        layout(&mut d, 1440.0);
        deep.push(t.ms());
    }
    // Root width change: everything re-wraps.
    let mut resize = Vec::new();
    for i in 0..10 {
        let mut st = d.t.style(d.root).unwrap().clone();
        let w = if i % 2 == 0 { 1280.0 } else { 1440.0 };
        st.size.width = length(w);
        d.t.set_style(d.root, st).unwrap();
        let t = Timer::start();
        layout(&mut d, w);
        resize.push(t.ms());
    }
    let p = |v: Vec<f64>| -> Percentiles { percentiles(v) };
    let (fp, ip, dp, rp) = (p(full), p(inc), p(deep), p(resize));
    eprintln!(
        "{:<10} nodes {:>6}  build {:>6.1}  full p50 {:>6.2}  text-leaf edit p50 {:>5.2} p95 {:>5.2}  icon edit p50 {:>5.2}  root resize p50 {:>6.2}",
        if fill_heavy { "fill-heavy" } else { "fixed" },
        d.nodes,
        build_ms,
        fp.p50,
        ip.p50,
        ip.p95,
        dp.p50,
        rp.p50
    );
    json!({ "fill_heavy": fill_heavy, "nodes": d.nodes, "build_ms": build_ms, "full": fp, "incremental_text_leaf": ip, "incremental_icon_style": dp, "root_resize": rp })
}

fn main() {
    let args = Args::from_env();
    let a = bench(false);
    let b = bench(true);
    write_json(args.value("--out"), &json!({ "taffy": "0.15.1", "host": Host::detect(String::new(), "cpu".into()), "cases": [a, b] }));
}
