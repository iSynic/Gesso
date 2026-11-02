//! Rejects NaN and infinities before they enter the document (JSON cannot represent them).

use crate::model::*;
use crate::node::{NodeKind, Override};
use crate::props::{CommonProps, PropValue};

pub(crate) trait Finite {
    fn finite(&self) -> bool;
}

impl Finite for f64 {
    fn finite(&self) -> bool {
        self.is_finite()
    }
}

impl<T: Finite> Finite for Option<T> {
    fn finite(&self) -> bool {
        self.as_ref().is_none_or(Finite::finite)
    }
}

impl<T: Finite> Finite for Vec<T> {
    fn finite(&self) -> bool {
        self.iter().all(Finite::finite)
    }
}

impl<T: Finite, const N: usize> Finite for [T; N] {
    fn finite(&self) -> bool {
        self.iter().all(Finite::finite)
    }
}

macro_rules! no_numbers {
    ($($t:ty),*) => { $( impl Finite for $t { fn finite(&self) -> bool { true } } )* };
}

no_numbers!(
    bool, u16, u32, String, Color, AssetId, Sizing, Constraints, BlendMode, ImageFit, StrokeAlign, StrokeCap,
    StrokeJoin, LayoutDirection, MainAlign, CrossAlign, TextDecoration, TextAlign, VerticalAlign, TextAutoResize,
    FillRule, PathData, crate::id::Id
);

impl<K, V> Finite for std::collections::BTreeMap<K, V> {
    fn finite(&self) -> bool {
        true
    }
}

macro_rules! fields {
    ($($t:ty: $($f:ident),*;)*) => {
        $( impl Finite for $t { fn finite(&self) -> bool { true $( && self.$f.finite() )* } } )*
    };
}

fields! {
    GradientStop: offset;
    Paint: kind, opacity;
    Stroke: paint, weight, dash;
    Shadow: offset, blur, spread;
    AutoLayout: gap, cross_gap, padding, columns, rows;
    LayoutChild: grow;
    TextStyle: font_size, line_height, letter_spacing;
    TextSpan: font_size;
    Arc: start, sweep, inner_ratio;
    Override: value;
    CommonProps: x, y, rotation, width, height, min_width, max_width, min_height, max_height, radius, fills,
        strokes, effects, opacity, layout_child;
}

impl Finite for Radius {
    fn finite(&self) -> bool {
        match self {
            Radius::Uniform(r) => r.finite(),
            Radius::Each(r) => r.finite(),
        }
    }
}

impl Finite for Track {
    fn finite(&self) -> bool {
        match self {
            Track::Fixed(v) | Track::Fr(v) => v.finite(),
            Track::Auto => true,
        }
    }
}

impl Finite for PaintKind {
    fn finite(&self) -> bool {
        match self {
            PaintKind::Solid { .. } | PaintKind::Image { .. } => true,
            PaintKind::LinearGradient { stops, start, end } => stops.finite() && start.finite() && end.finite(),
            PaintKind::RadialGradient { stops, center, radius } => stops.finite() && center.finite() && radius.finite(),
            PaintKind::AngularGradient { stops, center, angle } => stops.finite() && center.finite() && angle.finite(),
        }
    }
}

impl Finite for Effect {
    fn finite(&self) -> bool {
        match self {
            Effect::DropShadow(s) | Effect::InnerShadow(s) => s.finite(),
            Effect::LayerBlur { radius, .. } | Effect::BackgroundBlur { radius, .. } => radius.finite(),
        }
    }
}

impl Finite for NodeKind {
    fn finite(&self) -> bool {
        match self {
            NodeKind::Frame { auto_layout, .. } | NodeKind::Component { auto_layout, .. } => auto_layout.finite(),
            NodeKind::Ellipse { arc } => arc.finite(),
            NodeKind::Polygon { star_inner_ratio, .. } => star_inner_ratio.finite(),
            NodeKind::Text { spans, style, .. } => spans.finite() && style.finite(),
            NodeKind::Instance { overrides, .. } => overrides.finite(),
            _ => true,
        }
    }
}

impl Finite for PropValue {
    fn finite(&self) -> bool {
        match self {
            PropValue::Kind(k) => k.finite(),
            other => {
                // Every common property type implements Finite; check via a scratch struct.
                let mut p = CommonProps::default();
                match p.set(other.clone()) {
                    Ok(_) => p.finite(),
                    Err(_) => true,
                }
            }
        }
    }
}
