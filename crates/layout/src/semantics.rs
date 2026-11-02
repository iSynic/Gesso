//! Design-tool layout semantics mapped onto CSS flexbox and grid (Taffy styles). Everything that
//! decides what "hug", "fill", alignment or text auto-resize mean lives here.

use doc::model::{AutoLayout, CrossAlign, LayoutDirection, MainAlign, Sizing, TextAutoResize, Track};
use doc::{Node, NodeKind};
use taffy::prelude::*;

pub(crate) fn auto_layout(kind: &NodeKind) -> Option<&AutoLayout> {
    match kind {
        NodeKind::Frame { auto_layout: Some(a), .. } | NodeKind::Component { auto_layout: Some(a), .. } => Some(a),
        _ => None,
    }
}

/// Effective sizing per axis `(horizontal, vertical)`. Text auto-resize turns a fixed axis into
/// hug, and hug only means something for auto-layout containers and text.
pub fn sizing(node: &Node) -> (Sizing, Sizing) {
    let (mut h, mut v) = (node.props.sizing_h, node.props.sizing_v);
    // A group's size always comes from its children.
    if matches!(node.kind, NodeKind::Group {}) {
        return (Sizing::Fixed, Sizing::Fixed);
    }
    if let NodeKind::Text { auto_resize, .. } = node.kind {
        if auto_resize == TextAutoResize::WidthAndHeight && h == Sizing::Fixed {
            h = Sizing::Hug;
        }
        if auto_resize != TextAutoResize::None && v == Sizing::Fixed {
            v = Sizing::Hug;
        }
    }
    if auto_layout(&node.kind).is_none() && !matches!(node.kind, NodeKind::Text { .. }) {
        if h == Sizing::Hug {
            h = Sizing::Fixed;
        }
        if v == Sizing::Hug {
            v = Sizing::Fixed;
        }
    }
    (h, v)
}

/// Text that is not hugging horizontally wraps at its width.
pub fn text_wraps(node: &Node) -> bool {
    sizing(node).0 != Sizing::Hug
}

fn main_align(a: MainAlign) -> JustifyContent {
    match a {
        MainAlign::Start => JustifyContent::START,
        MainAlign::Center => JustifyContent::CENTER,
        MainAlign::End => JustifyContent::END,
        MainAlign::SpaceBetween => JustifyContent::SPACE_BETWEEN,
    }
}

fn cross_align(a: CrossAlign) -> AlignItems {
    match a {
        CrossAlign::Start => AlignItems::START,
        CrossAlign::Center => AlignItems::CENTER,
        CrossAlign::End => AlignItems::END,
        CrossAlign::Stretch => AlignItems::STRETCH,
        CrossAlign::Baseline => AlignItems::BASELINE,
    }
}

fn track(t: Track) -> GridTemplateComponent<String> {
    match t {
        Track::Fixed(v) => length(v as f32),
        Track::Fr(v) => fr(v as f32),
        Track::Auto => auto(),
    }
}

fn container(s: &mut Style, al: &AutoLayout) {
    let (gap, cross) = (length(al.gap as f32), length(al.cross_gap as f32));
    match al.direction {
        LayoutDirection::Horizontal => {
            s.flex_direction = FlexDirection::Row;
            s.gap = Size { width: gap, height: cross };
        }
        LayoutDirection::Vertical => {
            s.flex_direction = FlexDirection::Column;
            s.gap = Size { width: cross, height: gap };
        }
        LayoutDirection::Grid => {
            s.display = Display::Grid;
            s.gap = Size { width: gap, height: cross };
            s.grid_template_columns = al.columns.iter().map(|t| track(*t)).collect();
            s.grid_template_rows = al.rows.iter().map(|t| track(*t)).collect();
        }
    }
    let [t, r, b, l] = al.padding.map(|v| length(v as f32));
    s.padding = Rect { left: l, right: r, top: t, bottom: b };
    s.justify_content = main_align(al.main_align);
    s.align_items = cross_align(al.cross_align);
    // Wrapped lines and grid rows pack at the start instead of spreading out.
    s.align_content = AlignContent::START;
    s.flex_wrap = if al.wrap { FlexWrap::Wrap } else { FlexWrap::NoWrap };
}

/// The Taffy style of `node`. `parent` is its auto-layout parent, `None` for a layout root.
pub fn style(node: &Node, parent: Option<&AutoLayout>) -> Style {
    let p = &node.props;
    let mut s = Style::default();
    if !p.visible {
        s.display = Display::None;
        return s;
    }
    if let Some(al) = auto_layout(&node.kind) {
        container(&mut s, al);
    }
    let (mut h, mut v) = sizing(node);
    if parent.is_none() {
        // A root's parent doesn't lay it out, so there is nothing to fill.
        h = if h == Sizing::Fill { Sizing::Fixed } else { h };
        v = if v == Sizing::Fill { Sizing::Fixed } else { v };
    }
    let dim = |sz: Sizing, value: f64| if sz == Sizing::Fixed { length(value as f32) } else { auto() };
    s.size = Size { width: dim(h, p.width), height: dim(v, p.height) };
    s.min_size = Size { width: p.min_width.map_or(auto(), |v| length(v as f32)), height: p.min_height.map_or(auto(), |v| length(v as f32)) };
    s.max_size = Size { width: p.max_width.map_or(auto(), |v| length(v as f32)), height: p.max_height.map_or(auto(), |v| length(v as f32)) };
    // Design tools don't squash fixed or hugging children when space runs out.
    s.flex_shrink = 0.0;
    // A group's size always comes from its children; flow may only position it.
    let group = matches!(node.kind, NodeKind::Group {});
    if group {
        s.min_size = Size { width: auto(), height: auto() };
        s.max_size = Size { width: auto(), height: auto() };
    }
    let lc = &p.layout_child;
    if let Some(al) = parent {
        let horizontal = match al.direction {
            LayoutDirection::Horizontal => Some(true),
            LayoutDirection::Vertical => Some(false),
            LayoutDirection::Grid => None,
        };
        // `grow` is a weighted fill. As a grow on a fixed size it would feed back: the grown
        // size is written, becomes the next fixed size, and grows again.
        if lc.grow > 0.0 && !group {
            match horizontal {
                Some(true) => h = Sizing::Fill,
                Some(false) => v = Sizing::Fill,
                None => {}
            }
            s.size = Size { width: dim(h, p.width), height: dim(v, p.height) };
        }
        let (main, cross) = match horizontal {
            Some(true) => (h, v),
            Some(false) => (v, h),
            None => (Sizing::Fixed, Sizing::Fixed),
        };
        if main == Sizing::Fill {
            s.flex_grow = if lc.grow > 0.0 && !group { lc.grow as f32 } else { 1.0 };
            s.flex_shrink = 1.0;
            s.flex_basis = length(0.0);
            if horizontal == Some(true) && p.min_width.is_none() {
                s.min_size.width = length(0.0);
            }
            if horizontal == Some(false) && p.min_height.is_none() {
                s.min_size.height = length(0.0);
            }
        }
        if cross == Sizing::Fill {
            s.align_self = Some(AlignSelf::STRETCH);
        }
        if horizontal.is_none() {
            s.justify_self = Some(if h == Sizing::Fill { JustifySelf::STRETCH } else { JustifySelf::START });
            s.align_self = Some(if v == Sizing::Fill { AlignSelf::STRETCH } else { AlignSelf::START });
            if let Some([c, r]) = lc.grid_cell {
                s.grid_column = Line { start: line(c as i16 + 1), end: span(1) };
                s.grid_row = Line { start: line(r as i16 + 1), end: span(1) };
            }
        }
        if let Some(a) = lc.align_self
            && cross != Sizing::Fill
        {
            s.align_self = Some(cross_align(a));
        }
        if lc.absolute {
            s.position = Position::Absolute;
            s.inset = Rect { left: length(p.x as f32), top: length(p.y as f32), right: auto(), bottom: auto() };
        }
    }
    s
}
