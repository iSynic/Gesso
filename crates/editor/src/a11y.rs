//! What the canvas exposes to assistive technology: the layers around the selection (its
//! siblings, so Tab moves among announced nodes), or the page's top-level layers when nothing
//! is selected, each with a role, a name, screen bounds and whether it is selected. The shell
//! turns these into AccessKit nodes under the canvas.

use doc::{NodeId, NodeKind};
use kurbo::Rect;

use crate::session::Session;

/// Most nodes exposed at once (a container with thousands of children lists the first ones).
pub const MAX_NODES: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Frames, groups, components and instances.
    Group,
    Text,
    Image,
    /// Rectangles, ellipses, lines, paths and other shapes.
    Shape,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub id: NodeId,
    pub role: Role,
    /// The layer name, with its type ("Rectangle “Card”"), or the text content for text.
    pub label: String,
    /// The text's content, for text nodes.
    pub value: Option<String>,
    /// Bounds in canvas-local screen coordinates (`None` when not on screen).
    pub bounds: Option<Rect>,
    pub selected: bool,
    pub has_children: bool,
}

fn role(k: &NodeKind) -> Role {
    match k {
        NodeKind::Frame { .. } | NodeKind::Group {} | NodeKind::Component { .. } | NodeKind::Instance { .. } | NodeKind::Page { .. } => Role::Group,
        NodeKind::Text { .. } => Role::Text,
        NodeKind::Image { .. } => Role::Image,
        _ => Role::Shape,
    }
}

fn title(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// The nodes to expose, in stacking order (bottom first).
pub fn canvas_nodes(s: &Session) -> Vec<Node> {
    let doc = s.doc();
    let parent = s.selection().first().and_then(|&id| crate::select::parent_of(doc, id)).unwrap_or(s.page());
    let Some(p) = doc.get(parent) else { return Vec::new() };
    let view = s.view();
    let cam = s.camera.affine();
    p.children
        .iter()
        .filter_map(|&k| doc.node(k))
        .filter(|n| n.props.visible)
        .take(MAX_NODES)
        .map(|n| {
            let value = match &n.kind {
                NodeKind::Text { spans, .. } => Some(doc::richtext::plain(spans)),
                _ => None,
            };
            let kind = title(n.kind.type_name());
            let label = if n.name.is_empty() { kind } else { format!("{kind} \u{201c}{}\u{201d}", n.name) };
            let bounds = s.scene.index.bounds(n.id).filter(|b| b.overlaps(view)).map(|b| cam.transform_rect_bbox(b));
            Node { id: n.id, role: role(&n.kind), label, value, bounds, selected: s.selection().contains(&n.id), has_children: !n.children.is_empty() }
        })
        .collect()
}
