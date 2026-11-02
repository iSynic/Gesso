//! In-app copy and paste. The clipboard carries node trees as tagged JSON text, so a paste
//! from another window of the app (or another open document) works through the OS clipboard.
//! SVG text and bitmaps on the clipboard are placed through [`crate::place`].

use doc::{NodeId, NodeKind, NodeTree, TxMeta};
use kurbo::{Affine, Point, Rect, Vec2};
use serde::{Deserialize, Serialize};

use crate::session::{Error, Session};
use crate::transform::top_items;
use crate::xform::{Placement, geometric_bounds};
use crate::select;

/// First line of a clipboard payload.
pub const TAG: &str = "ai-design-engine/nodes v2";

#[derive(Debug, Serialize, Deserialize)]
struct Payload {
    /// Each copied subtree with its world transform at copy time.
    nodes: Vec<Copied>,
    /// World bounds of everything copied.
    bounds: [f64; 4],
    /// Components the copied instances use that weren't copied themselves; a paste into a
    /// document without them adds them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    components: Vec<NodeTree>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Copied {
    tree: NodeTree,
    world: [f64; 6],
}

/// The selection as clipboard text, or `None` if nothing is selected.
pub fn copy(s: &Session) -> Option<String> {
    let doc = s.doc();
    let index = &s.scene.index;
    let ids = top_items(doc, s.selection());
    let mut nodes = Vec::new();
    let mut bounds: Option<Rect> = None;
    for id in ids {
        let k = doc.key(id)?;
        let n = doc.node(k)?;
        let world = index.world(id)?;
        if let Some(b) = geometric_bounds(index, n) {
            bounds = Some(bounds.map_or(b, |a| a.union(b)));
        }
        nodes.push(Copied { tree: doc.to_tree(k)?, world: world.as_coeffs() });
    }
    let b = bounds?;
    let trees: Vec<NodeTree> = nodes.iter().map(|c| c.tree.clone()).collect();
    let components = doc::components::used_components(doc, &trees);
    let p = Payload { nodes, bounds: [b.x0, b.y0, b.x1, b.y1], components };
    Some(format!("{TAG}\n{}", serde_json::to_string(&p).ok()?))
}

/// Pastes clipboard text. Into the selected frame if one frame is selected, otherwise into the
/// current scope or the page. Things land where they were copied if that spot is in view,
/// otherwise centred in the view. Returns the new nodes (also selected), or `None` if the text
/// isn't ours.
pub fn paste(s: &mut Session, text: &str) -> Result<Option<Vec<NodeId>>, Error> {
    let Some(body) = text.strip_prefix(TAG).map(str::trim_start) else { return Ok(None) };
    let p: Payload = serde_json::from_str(body).map_err(|e| Error::Other(format!("clipboard: {e}")))?;
    let parent = target(s);
    let parent_world = s.scene.index.world(parent).unwrap_or(Affine::IDENTITY);
    let b = Rect::new(p.bounds[0], p.bounds[1], p.bounds[2], p.bounds[3]);
    let view = s.view();
    let shift = if view.contains(b.center()) { Vec2::ZERO } else { view.center() - b.center() };
    let page = s.page();
    let ids = s.transact(TxMeta::user("Paste"), |tx| {
        // Missing components go on the page, right of the pasted content, with their ids.
        let mut x = b.x1 + shift.x + 100.0;
        for c in &p.components {
            if tx.doc().get(c.id).is_none() {
                let mut c = c.clone();
                (c.props.x, c.props.y) = (x, b.y0 + shift.y);
                x += c.props.width + 40.0;
                tx.insert(Some(page), None, c)?;
            }
        }
        let mut trees: Vec<NodeTree> = p.nodes.iter().map(|c| c.tree.clone()).collect();
        doc::components::remap_ids(&mut trees, || tx.new_id());
        let mut out = Vec::new();
        for (c, tree) in p.nodes.iter().zip(trees) {
            let id = tx.insert(Some(parent), None, tree)?;
            let n = tx.doc().get(id).ok_or(doc::Error::Input("paste failed".into()))?;
            let old = Placement::of(n);
            let world = Affine::translate(shift) * Affine::new(c.world);
            let new = Placement::from_affine(parent_world.inverse() * world, old.w, old.h, &old);
            for u in new.diff(&old) {
                tx.update(id, u)?;
            }
            out.push(id);
        }
        Ok(out)
    })?;
    s.select(ids.clone());
    Ok(Some(ids))
}

/// Where pasted and placed things go: the selected frame if one frame is selected, otherwise the
/// current scope or the page.
pub(crate) fn target(s: &Session) -> NodeId {
    let doc = s.doc();
    match s.selection() {
        [one] if doc.get(*one).is_some_and(|n| matches!(n.kind, NodeKind::Frame { .. } | NodeKind::Component { .. })) => *one,
        _ => s.scope().unwrap_or(s.page()),
    }
}

/// The selection's bounds centre, for tests and status text.
pub fn centre_of(s: &Session, ids: &[NodeId]) -> Option<Point> {
    select::union_bounds(&s.scene.index, ids).map(|r| r.center())
}
