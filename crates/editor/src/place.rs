//! Placing outside content on the canvas: SVG (imported as shapes) and raster images. Each lands
//! in the paste target, centred on a world point (where it was dropped) or in the view, and is
//! selected, in one undo step.

use doc::model::{AssetId, AssetMeta};
use doc::{NodeId, TxMeta};
use kurbo::{Affine, Point};
use serde_json::{Value, json};

use crate::clipboard::target;
use crate::session::{Error, Session};

/// Images larger than this share of the view are scaled down to it when placed.
const FIT: f64 = 0.8;

/// Imports SVG source as a frame named `name`. Returns the frame and what could not be
/// imported exactly.
pub fn svg(s: &mut Session, source: &str, name: &str, at: Option<Point>) -> Result<(NodeId, Vec<String>), Error> {
    let imp = s.import_svg(source, name)?;
    let id = insert(s, "Import SVG", imp.assets, imp.node, at)?;
    Ok((id, imp.warnings))
}

/// Places image bytes (PNG, JPEG, WebP or GIF) as an image node at its pixel size, scaled down
/// to fit the view.
pub fn image(s: &mut Session, bytes: Vec<u8>, name: &str, at: Option<Point>) -> Result<NodeId, Error> {
    let (id, meta) = s.add_asset(bytes)?;
    let (mut w, mut h) = (f64::from(meta.width.max(1)), f64::from(meta.height.max(1)));
    let view = s.view();
    let k = (view.width() * FIT / w).min(view.height() * FIT / h).min(1.0);
    if k > 0.0 {
        (w, h) = ((w * k).round().max(1.0), (h * k).round().max(1.0));
    }
    let node = json!({"type": "image", "name": name, "asset": id.0, "fit": "fill", "width": w, "height": h});
    insert(s, "Place image", vec![(id, meta)], node, at)
}

fn insert(s: &mut Session, label: &str, assets: Vec<(AssetId, AssetMeta)>, mut node: Value, at: Option<Point>) -> Result<NodeId, Error> {
    let parent = target(s);
    let centre = at.unwrap_or_else(|| s.view().center());
    let local = s.scene.index.world(parent).unwrap_or(Affine::IDENTITY).inverse() * centre;
    let (w, h) = (node["width"].as_f64().unwrap_or(0.0), node["height"].as_f64().unwrap_or(0.0));
    if let Some(o) = node.as_object_mut() {
        o.insert("x".into(), json!((local.x - w / 2.0).round()));
        o.insert("y".into(), json!((local.y - h / 2.0).round()));
    }
    let id = s.transact(TxMeta::user(label), |tx| {
        for (id, meta) in assets {
            tx.put_asset(id, meta)?;
        }
        tx.insert_json(Some(parent), None, node)
    })?;
    s.select(vec![id]);
    Ok(id)
}
