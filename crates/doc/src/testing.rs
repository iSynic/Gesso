//! Seeded generator of realistic documents for tests and benchmarks (feature `testing`).
//! Layout: a components page with a button component, then pages of 1440×1024 artboards, each a
//! vertical auto-layout frame of cards. A card is about 10 nodes: image, title and body text,
//! avatar ellipse, icon path, a tag rectangle and a button instance with an override.

use std::collections::BTreeMap;

use crate::document::DocumentData;
use crate::id::{Id, IdGen};
use crate::model::*;
use crate::node::{NodeKind, NodeTree, Override};
use crate::props::{CommonProps, PropValue};
use crate::variables::*;

pub struct GenConfig {
    pub target_nodes: usize,
    pub seed: u64,
    /// Words of body text per card; raises bytes per node (used to reach ~50 MB files).
    pub body_words: usize,
    /// Adds a "Layout stress" page: one auto-layout tree of about 10k nodes (100 sections of
    /// wrapping rows and grids, 10 cards each, text that wraps and hugs, fill spacers).
    pub auto_layout_heavy: bool,
}

impl Default for GenConfig {
    fn default() -> Self {
        GenConfig { target_nodes: 10_000, seed: 1, body_words: 12, auto_layout_heavy: false }
    }
}

const WORDS: &[&str] = &[
    "design", "canvas", "layout", "frame", "token", "vector", "render", "shadow", "export", "agent", "layer", "variant",
    "grid", "padding", "spacing", "colour", "typography", "component", "instance", "preview", "مرحبا", "设计", "デザイン",
];

const ICON: &str = "M2 12 L12 2 L22 12 L12 22 Z M8 12 C8 9.8 9.8 8 12 8 C14.2 8 16 9.8 16 12 C16 14.2 14.2 16 12 16 C9.8 16 8 14.2 8 12 Z";

struct Gen {
    ids: IdGen,
    rng: u64,
}

impl Gen {
    fn id(&mut self) -> Id {
        self.ids.next_id()
    }
    fn next(&mut self) -> u64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        self.rng
    }
    fn pick<'a>(&mut self, xs: &'a [&'a str]) -> &'a str {
        xs[(self.next() % xs.len() as u64) as usize]
    }
    fn color(&mut self) -> Color {
        let v = self.next();
        Color::rgb(v as u8, (v >> 8) as u8, (v >> 16) as u8)
    }
    fn words(&mut self, n: usize) -> String {
        (0..n).map(|_| self.pick(WORDS)).collect::<Vec<_>>().join(" ")
    }
    fn node(&mut self, kind: NodeKind, name: &str, props: CommonProps) -> NodeTree {
        NodeTree { id: self.id(), kind, name: name.into(), props, children: Vec::new() }
    }
}

fn sized(x: f64, y: f64, w: f64, h: f64) -> CommonProps {
    CommonProps { x, y, width: w, height: h, ..Default::default() }
}

fn text(spans: Vec<TextSpan>, size: f64, weight: u16) -> NodeKind {
    NodeKind::Text {
        spans,
        style: TextStyle { font_size: size, font_weight: weight, ..Default::default() },
        align: TextAlign::Left,
        vertical_align: VerticalAlign::Top,
        auto_resize: TextAutoResize::Height,
    }
}

pub fn asset_id(i: u64) -> AssetId {
    AssetId(format!("{:064x}", 0xA55E7_u64.wrapping_mul(i + 1)))
}

pub fn generate(cfg: &GenConfig) -> DocumentData {
    let mut g = Gen { ids: IdGen::seeded(cfg.seed), rng: cfg.seed | 1 };
    let assets: BTreeMap<AssetId, AssetMeta> =
        (0..8).map(|i| (asset_id(i), AssetMeta { mime: "image/png".into(), width: 640, height: 400 })).collect();

    // Variables: Theme collection with light and dark modes.
    let (coll, light, dark) = (g.id(), g.id(), g.id());
    let (surface, ink, accent, spacing) = (g.id(), g.id(), g.id(), g.id());
    let var = |id, name: &str, ty, l: VariableValue, d: VariableValue| Variable {
        id,
        name: name.into(),
        ty,
        values: BTreeMap::from([(light, l), (dark, d)]),
    };
    let variables = Variables {
        collections: vec![Collection {
            id: coll,
            name: "Theme".into(),
            modes: vec![Mode { id: light, name: "Light".into() }, Mode { id: dark, name: "Dark".into() }],
            default_mode: light,
            variables: vec![
                var(surface, "surface", VariableType::Color, VariableValue::String("#ffffff".into()), VariableValue::String("#1e1e24".into())),
                var(ink, "ink", VariableType::Color, VariableValue::String("#111827".into()), VariableValue::String("#f3f4f6".into())),
                var(accent, "accent", VariableType::Color, VariableValue::String("#0d99ff".into()), VariableValue::Alias { alias: ink }),
                var(spacing, "spacing/md", VariableType::Number, VariableValue::Number(16.0), VariableValue::Number(16.0)),
            ],
        }],
    };

    // Components page: one button component.
    let mut button = g.node(
        NodeKind::Component {
            auto_layout: Some(AutoLayout { padding: [8.0, 16.0, 8.0, 16.0], cross_align: CrossAlign::Center, ..Default::default() }),
            description: "Primary button".into(),
        },
        "Button",
        CommonProps { radius: Radius::Uniform(6.0), fills: vec![Paint::solid(Color::rgb(13, 153, 255))], sizing_h: Sizing::Hug, ..sized(0.0, 0.0, 120.0, 36.0) },
    );
    let label = g.node(text(vec![TextSpan { text: "Button".into(), ..Default::default() }], 14.0, 600), "Label", sized(16.0, 8.0, 88.0, 20.0));
    let label_id = label.id;
    button.children.push(label);
    let button_id = button.id;
    let mut comp_page = g.node(NodeKind::Page { background: Some(Color::rgb(245, 245, 245)) }, "Components", CommonProps::default());
    comp_page.children.push(button);

    let mut pages = vec![comp_page];
    let mut count = 3usize;
    let mut page_no = 0;
    while count < cfg.target_nodes {
        page_no += 1;
        let mut page = g.node(NodeKind::Page { background: None }, &format!("Page {page_no}"), CommonProps::default());
        count += 1;
        let mut board_no = 0;
        while count < cfg.target_nodes && page.children.len() < 100 {
            let (bx, by) = ((board_no % 10) as f64 * 1600.0, (board_no / 10) as f64 * 1200.0);
            board_no += 1;
            let mut board = g.node(
                NodeKind::Frame {
                    auto_layout: Some(AutoLayout {
                        direction: LayoutDirection::Horizontal,
                        wrap: true,
                        gap: 24.0,
                        cross_gap: 24.0,
                        padding: [40.0; 4],
                        ..Default::default()
                    }),
                    clip: true,
                },
                &format!("Screen {board_no}"),
                CommonProps { fills: vec![Paint::solid(Color::WHITE)], bindings: BTreeMap::from([("fills/0/color".into(), surface)]), ..sized(bx, by, 1440.0, 1024.0) },
            );
            count += 1;
            for c in 0..24 {
                if count >= cfg.target_nodes {
                    break;
                }
                let mut card = g.node(
                    NodeKind::Frame {
                        auto_layout: Some(AutoLayout { direction: LayoutDirection::Vertical, gap: 8.0, padding: [12.0; 4], ..Default::default() }),
                        clip: true,
                    },
                    &format!("Card {c}"),
                    CommonProps {
                        radius: Radius::Uniform(12.0),
                        fills: vec![Paint::solid(Color::rgb(250, 250, 252))],
                        strokes: vec![Stroke { paint: Paint::solid(Color::rgb(226, 232, 240)), weight: 1.0, align: StrokeAlign::Inside, dash: vec![], cap: StrokeCap::Butt, join: StrokeJoin::Miter }],
                        effects: vec![Effect::DropShadow(Shadow { color: Color::rgba(0, 0, 0, 30), offset: [0.0, 4.0], blur: 12.0, spread: 0.0, visible: true })],
                        sizing_v: Sizing::Hug,
                        ..sized(0.0, 0.0, 320.0, 300.0)
                    },
                );
                let asset = asset_id(g.next() % 8);
                let title = g.words(3);
                let body = g.words(cfg.body_words);
                let accent_color = g.color();
                let (avatar_c, grad_a, grad_b) = (g.color(), g.color(), g.color());
                let kids = vec![
                    g.node(NodeKind::Image { asset, fit: ImageFit::Fill }, "Cover", CommonProps { radius: Radius::Each([8.0, 8.0, 0.0, 0.0]), sizing_h: Sizing::Fill, ..sized(0.0, 0.0, 296.0, 140.0) }),
                    g.node(text(vec![TextSpan { text: title, ..Default::default() }], 18.0, 700), "Title", CommonProps { sizing_h: Sizing::Fill, ..sized(0.0, 0.0, 296.0, 24.0) }),
                    g.node(
                        text(
                            vec![
                                TextSpan { text: body, ..Default::default() },
                                TextSpan { text: " more".into(), color: Some(accent_color), link: Some("https://example.com".into()), ..Default::default() },
                            ],
                            14.0,
                            400,
                        ),
                        "Body",
                        CommonProps { sizing_h: Sizing::Fill, bindings: BTreeMap::from([("spans/0/color".into(), ink)]), ..sized(0.0, 0.0, 296.0, 40.0) },
                    ),
                    g.node(NodeKind::Ellipse { arc: None }, "Avatar", CommonProps { fills: vec![Paint::solid(avatar_c)], ..sized(0.0, 0.0, 32.0, 32.0) }),
                    g.node(
                        NodeKind::Path { paths: vec![PathData { d: ICON.into(), fill_rule: FillRule::EvenOdd }] },
                        "Icon",
                        CommonProps { fills: vec![Paint::solid(Color::rgb(100, 116, 139))], layout_child: LayoutChild { absolute: true, ..Default::default() }, ..sized(280.0, 12.0, 24.0, 24.0) },
                    ),
                    g.node(
                        NodeKind::Rectangle {},
                        "Tag",
                        CommonProps {
                            radius: Radius::Uniform(4.0),
                            fills: vec![Paint {
                                kind: PaintKind::LinearGradient {
                                    stops: vec![GradientStop { offset: 0.0, color: grad_a }, GradientStop { offset: 1.0, color: grad_b }],
                                    start: [0.0, 0.0],
                                    end: [1.0, 1.0],
                                },
                                opacity: 0.9,
                                visible: true,
                                blend: BlendMode::Normal,
                            }],
                            ..sized(0.0, 0.0, 64.0, 20.0)
                        },
                    ),
                    g.node(
                        NodeKind::Instance {
                            component: button_id,
                            overrides: vec![Override {
                                target: vec![label_id],
                                value: PropValue::Kind(text(vec![TextSpan { text: "Open".into(), ..Default::default() }], 14.0, 600)),
                            }],
                        },
                        "Open button",
                        sized(0.0, 0.0, 120.0, 36.0),
                    ),
                ];
                count += 1 + kids.len();
                card.children = kids;
                board.children.push(card);
            }
            page.children.push(board);
        }
        pages.push(page);
    }
    if cfg.auto_layout_heavy {
        pages.push(stress_page(&mut g));
    }
    DocumentData { pages, variables, assets }
}

/// Name of the root frame on the "Layout stress" page.
pub const STRESS_ROOT: &str = "Stress root";

fn stress_page(g: &mut Gen) -> NodeTree {
    let al = |direction, gap: f64, pad: f64| AutoLayout { direction, gap, cross_gap: gap, padding: [pad; 4], ..Default::default() };
    let hug = |w: f64, h: f64| CommonProps { sizing_h: Sizing::Hug, sizing_v: Sizing::Hug, ..sized(0.0, 0.0, w, h) };
    let mut sections = Vec::new();
    for s in 0..100 {
        let mut cards = Vec::new();
        for c in 0..10 {
            let fill_w = CommonProps { sizing_h: Sizing::Fill, ..sized(0.0, 0.0, 188.0, 20.0) };
            let words = [2 + (s + c) % 3, 5, 14 + (c * 3) % 9];
            let mut kids: Vec<NodeTree> = words
                .iter()
                .zip([16.0, 13.0, 13.0])
                .map(|(n, size)| {
                    let t = g.words(*n);
                    g.node(text(vec![TextSpan { text: t, ..Default::default() }], size, 400), "Text", fill_w.clone())
                })
                .collect();
            let mut row = g.node(NodeKind::Frame { auto_layout: Some(AutoLayout { cross_align: CrossAlign::Center, ..al(LayoutDirection::Horizontal, 8.0, 0.0) }), clip: false }, "Row", CommonProps { sizing_v: Sizing::Hug, ..fill_w.clone() });
            let label = g.words(1);
            let mut button = g.node(NodeKind::Frame { auto_layout: Some(AutoLayout { padding: [6.0, 12.0, 6.0, 12.0], ..Default::default() }), clip: false }, "Button", hug(60.0, 28.0));
            button.children.push(g.node(text(vec![TextSpan { text: "Open".into(), ..Default::default() }], 12.0, 600), "Label", hug(30.0, 16.0)));
            row.children = vec![
                g.node(NodeKind::Rectangle {}, "Icon", sized(0.0, 0.0, 20.0, 20.0)),
                g.node(text(vec![TextSpan { text: label, ..Default::default() }], 12.0, 400), "Meta", hug(40.0, 16.0)),
                g.node(NodeKind::Rectangle {}, "Spacer", CommonProps { sizing_h: Sizing::Fill, ..sized(0.0, 0.0, 10.0, 1.0) }),
                button,
            ];
            kids.push(row);
            let width = if s % 2 == 0 { 220.0 } else { 260.0 };
            let mut card = g.node(NodeKind::Frame { auto_layout: Some(al(LayoutDirection::Vertical, 8.0, 16.0)), clip: true }, "Card", CommonProps { sizing_v: Sizing::Hug, min_width: Some(160.0), ..sized(0.0, 0.0, width, 200.0) });
            if s % 5 >= 3 {
                card.props.sizing_h = Sizing::Fill;
            }
            card.children = kids;
            cards.push(card);
        }
        let kind = if s % 5 < 3 {
            AutoLayout { wrap: true, ..al(LayoutDirection::Horizontal, 16.0, 24.0) }
        } else {
            AutoLayout { columns: vec![Track::Fr(1.0); 3], ..al(LayoutDirection::Grid, 16.0, 24.0) }
        };
        let mut section = g.node(NodeKind::Frame { auto_layout: Some(kind), clip: false }, "Section", CommonProps { sizing_h: Sizing::Fill, sizing_v: Sizing::Hug, ..sized(0.0, 0.0, 1440.0, 400.0) });
        section.children = cards;
        sections.push(section);
    }
    let mut root = g.node(NodeKind::Frame { auto_layout: Some(al(LayoutDirection::Vertical, 32.0, 0.0)), clip: false }, STRESS_ROOT, CommonProps { sizing_v: Sizing::Hug, ..sized(0.0, 0.0, 1440.0, 40_000.0) });
    root.children = sections;
    let mut page = g.node(NodeKind::Page { background: None }, "Layout stress", CommonProps::default());
    page.children.push(root);
    page
}

/// A small page for render and index property tests: a clipping frame with shapes, text, a
/// rotated child, a group and a nested frame. Returns the page id.
pub fn render_test_page(tx: &mut crate::Tx<'_>) -> Result<crate::NodeId, crate::Error> {
    use serde_json::json;
    let page = tx.insert_json(None, None, json!({"type": "page", "background": "#f4f4f4"}))?;
    tx.insert_json(Some(page), None, json!({
        "type": "frame", "x": 10, "y": 10, "width": 200, "height": 160, "fills": [{"type": "solid", "color": "#ffffff"}],
        "strokes": [{"paint": {"type": "solid", "color": "#333333"}, "weight": 2}],
        "children": [
            {"type": "rectangle", "x": 10, "y": 10, "width": 60, "height": 40, "radius": 8, "fills": [{"type": "solid", "color": "#e04040"}]},
            {"type": "ellipse", "x": 90, "y": 10, "width": 50, "height": 50, "fills": [{"type": "solid", "color": "#4080e0"}]},
            {"type": "text", "x": 10, "y": 70, "width": 120, "height": 20, "spans": [{"text": "Hello render"}]},
            {"type": "rectangle", "x": 150, "y": 100, "width": 80, "height": 30, "rotation": 30, "fills": [{"type": "solid", "color": "#40a040"}]},
            {"type": "frame", "x": 20, "y": 100, "width": 100, "height": 50, "clip": false, "fills": [{"type": "solid", "color": "#eeeeaa"}],
             "children": [{"type": "polygon", "sides": 5, "starInnerRatio": 0.5, "x": 60, "y": 20, "width": 60, "height": 60, "fills": [{"type": "solid", "color": "#a040a0"}]}]}
        ]
    }))?;
    tx.insert_json(Some(page), None, json!({
        "type": "group", "x": 240, "y": 20, "width": 80, "height": 80,
        "children": [
            {"type": "rectangle", "width": 40, "height": 40, "fills": [{"type": "solid", "color": "#202020"}],
             "effects": [{"type": "dropShadow", "color": "#00000080", "offset": [4, 4], "blur": 8}]},
            {"type": "line", "x": 0, "y": 60, "width": 80, "height": 0, "strokes": [{"paint": {"type": "solid", "color": "#000000"}, "weight": 3}]}
        ]
    }))?;
    Ok(page)
}

/// One random edit for property tests: `(what, a, b, v)`, `a`/`b` pick nodes, `v` a value. Covers
/// geometry, visibility, paints, strokes, effects, inserts, deletes and moves.
pub fn random_edit(tx: &mut crate::Tx<'_>, page: crate::NodeId, (what, a, b, v): (u8, u8, u8, u16)) -> Result<(), crate::Error> {
    use serde_json::json;
    let d = tx.doc();
    let Some(pk) = d.key(page) else { return Ok(()) };
    let nodes: Vec<crate::NodeId> = d.descendants(pk).into_iter().filter_map(|k| d.node(k)).map(|n| n.id).filter(|id| *id != page).collect();
    let containers: Vec<crate::NodeId> =
        std::iter::once(page).chain(nodes.iter().copied().filter(|id| d.get(*id).is_some_and(|n| n.kind.is_container() && !matches!(n.kind, NodeKind::Group {})))).collect();
    if nodes.is_empty() {
        return tx.insert_json(Some(page), None, json!({"type": "rectangle", "width": 20, "height": 20})).map(|_| ());
    }
    let n = nodes[a as usize % nodes.len()];
    let c = containers[b as usize % containers.len()];
    let fv = (v % 150) as f64 + 1.0;
    let color = format!("#{:06x}", (v as u32).wrapping_mul(2654435761) & 0xffffff);
    let props = match what % 14 {
        0 => json!({"x": fv - 30.0}),
        1 => json!({"y": fv - 30.0}),
        2 => json!({"width": fv}),
        3 => json!({"height": fv}),
        4 => json!({"rotation": (v % 360) as f64}),
        5 => json!({"visible": v % 3 != 0}),
        6 => json!({"fills": [{"type": "solid", "color": color}]}),
        7 => {
            let align = ["inside", "center", "outside"][v as usize % 3];
            json!({"strokes": [{"paint": {"type": "solid", "color": color}, "weight": (v % 6) as f64, "align": align}]})
        }
        8 => json!({"effects": if v % 2 == 0 { json!([{"type": "dropShadow", "color": "#00000066", "offset": [2, 3], "blur": (v % 20) as f64}]) } else { json!([{"type": "layerBlur", "radius": (v % 10) as f64}]) }}),
        9 => json!({"opacity": (v % 10) as f64 / 10.0}),
        10 => {
            let kind = ["rectangle", "ellipse", "frame"][v as usize % 3];
            return tx.insert_json(Some(c), None, json!({"type": kind, "x": fv / 2.0, "y": fv / 3.0, "width": fv, "height": 20, "fills": [{"type": "solid", "color": color}]})).map(|_| ());
        }
        11 => return tx.delete(n),
        12 => {
            if tx.doc().get(c).is_some_and(|cn| cn.kind.is_page()) || tx.doc().key(c).zip(tx.doc().key(n)).is_some_and(|(ck, nk)| tx.doc().is_descendant_of(ck, nk)) {
                return Ok(());
            }
            return tx.move_node(n, Some(c), None);
        }
        _ => json!({"flipX": v % 2 == 0}),
    };
    let Some(obj) = props.as_object() else { return Ok(()) };
    tx.update_json(n, obj)
}
