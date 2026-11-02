//! Node kinds and the owned node tree used by the file format, insert ops and the agent API.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::id::{Id, NodeId};
use crate::model::*;
use crate::props::{CommonProps, PropValue};

/// One property override on an instance. `target` is a path of ids from the instance down to
/// the overridden node: each step is a node inside the previous step's component (a nested
/// instance is stepped through by its own id), and `[]` is the instance itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Override {
    #[serde(default)]
    pub target: Vec<Id>,
    #[serde(flatten)]
    pub value: PropValue,
}

/// The node's type and its type-specific data. The JSON tag is `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum NodeKind {
    /// A top-level infinite canvas.
    #[serde(rename_all = "camelCase")]
    Page {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        background: Option<Color>,
    },
    #[serde(rename_all = "camelCase")]
    Frame {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auto_layout: Option<AutoLayout>,
        /// Clip children to the frame bounds.
        #[serde(default = "yes", skip_serializing_if = "is_true")]
        clip: bool,
    },
    Group {},
    Rectangle {},
    #[serde(rename_all = "camelCase")]
    Ellipse {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        arc: Option<Arc>,
    },
    #[serde(rename_all = "camelCase")]
    Polygon {
        sides: u32,
        /// Inner radius ratio for stars (0..1); `None` = regular polygon.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        star_inner_ratio: Option<f64>,
    },
    Line {},
    #[serde(rename_all = "camelCase")]
    Path { paths: Vec<PathData> },
    #[serde(rename_all = "camelCase")]
    Text {
        spans: Vec<TextSpan>,
        #[serde(default)]
        style: TextStyle,
        #[serde(default, skip_serializing_if = "is_default")]
        align: TextAlign,
        #[serde(default, skip_serializing_if = "is_default")]
        vertical_align: VerticalAlign,
        #[serde(default, skip_serializing_if = "is_default")]
        auto_resize: TextAutoResize,
    },
    #[serde(rename_all = "camelCase")]
    Image {
        asset: AssetId,
        #[serde(default, skip_serializing_if = "is_default")]
        fit: ImageFit,
    },
    /// A main component; its children are the component's content.
    #[serde(rename_all = "camelCase")]
    Component {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auto_layout: Option<AutoLayout>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        description: String,
    },
    /// An instance of a component, with per-property overrides. Its content comes from the
    /// component, so it has no children of its own.
    #[serde(rename_all = "camelCase")]
    Instance {
        component: NodeId,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        overrides: Vec<Override>,
    },
    /// A sticky note for comments on the canvas.
    #[serde(rename_all = "camelCase")]
    Note { text: String },
}

/// One number of path data, on the 1/1000 grid.
fn push_coord(d: &mut String, v: f64) {
    if !d.is_empty() && !d.ends_with(|c: char| c.is_ascii_alphabetic()) {
        d.push(' ');
    }
    d.push_str(&((v * 1000.0).round() / 1000.0).to_string());
}

/// SVG path data scaled by `(kx, ky)` about the origin. Data that doesn't parse is kept.
fn scale_d(d: &str, kx: f64, ky: f64) -> String {
    let Ok(mut b) = kurbo::BezPath::from_svg(d) else { return d.to_string() };
    b.apply_affine(kurbo::Affine::scale_non_uniform(kx, ky));
    let mut out = String::new();
    for el in b.elements() {
        let (cmd, points): (char, &[kurbo::Point]) = match el {
            kurbo::PathEl::MoveTo(p) => ('M', std::slice::from_ref(p)),
            kurbo::PathEl::LineTo(p) => ('L', std::slice::from_ref(p)),
            kurbo::PathEl::QuadTo(a, p) => {
                out.push('Q');
                push_coord(&mut out, a.x);
                push_coord(&mut out, a.y);
                ('\0', std::slice::from_ref(p))
            }
            kurbo::PathEl::CurveTo(a, c, p) => {
                out.push('C');
                for q in [a, c] {
                    push_coord(&mut out, q.x);
                    push_coord(&mut out, q.y);
                }
                ('\0', std::slice::from_ref(p))
            }
            kurbo::PathEl::ClosePath => ('Z', &[]),
        };
        if cmd != '\0' {
            out.push(cmd);
        }
        for p in points {
            push_coord(&mut out, p.x);
            push_coord(&mut out, p.y);
        }
    }
    out
}

impl NodeKind {
    /// A path scaled about its box's origin; `None` for anything else. Resizing a path scales
    /// its shape this way, so data with a margin inside its box keeps that margin in proportion.
    pub fn scaled_path(&self, kx: f64, ky: f64) -> Option<NodeKind> {
        let NodeKind::Path { paths } = self else { return None };
        Some(NodeKind::Path { paths: paths.iter().map(|p| PathData { d: scale_d(&p.d, kx, ky), fill_rule: p.fill_rule }).collect() })
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            NodeKind::Page { .. } => "page",
            NodeKind::Frame { .. } => "frame",
            NodeKind::Group {} => "group",
            NodeKind::Rectangle {} => "rectangle",
            NodeKind::Ellipse { .. } => "ellipse",
            NodeKind::Polygon { .. } => "polygon",
            NodeKind::Line {} => "line",
            NodeKind::Path { .. } => "path",
            NodeKind::Text { .. } => "text",
            NodeKind::Image { .. } => "image",
            NodeKind::Component { .. } => "component",
            NodeKind::Instance { .. } => "instance",
            NodeKind::Note { .. } => "note",
        }
    }

    pub fn is_container(&self) -> bool {
        matches!(self, NodeKind::Page { .. } | NodeKind::Frame { .. } | NodeKind::Group {} | NodeKind::Component { .. })
    }

    /// Kinds that may choose variable modes for their content.
    pub fn takes_modes(&self) -> bool {
        matches!(self, NodeKind::Page { .. } | NodeKind::Frame { .. } | NodeKind::Component { .. } | NodeKind::Instance { .. })
    }

    pub fn is_page(&self) -> bool {
        matches!(self, NodeKind::Page { .. })
    }

    pub fn same_type(&self, other: &NodeKind) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }
}

/// An owned node with its subtree, in z-order (first child is drawn first).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NodeTree {
    pub id: NodeId,
    #[serde(flatten)]
    pub kind: NodeKind,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(flatten)]
    pub props: CommonProps,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<NodeTree>,
}

impl NodeTree {
    pub fn new(id: NodeId, kind: NodeKind) -> Self {
        NodeTree { id, kind, name: String::new(), props: CommonProps::default(), children: Vec::new() }
    }

    /// Visits this node and all descendants, parents first.
    pub fn walk<'a>(&'a self, f: &mut impl FnMut(&'a NodeTree)) {
        f(self);
        for c in &self.children {
            c.walk(f);
        }
    }

    pub fn count(&self) -> usize {
        1 + self.children.iter().map(NodeTree::count).sum::<usize>()
    }
}
