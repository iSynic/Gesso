//! Placing SVG and images: where they land, their assets, and that each is one undo step.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use doc::{Document, NodeKind, Store, TxMeta};
use editor::{Session, place};
use kurbo::{Point, Vec2};
use serde_json::json;
use text::FontDb;

fn session() -> Session {
    let mut s = Store::new(Document::new());
    s.transact(TxMeta::user("setup"), |tx| {
        let page = tx.insert_json(None, None, json!({"type": "page"}))?;
        tx.insert_json(Some(page), None, json!({"type": "frame", "name": "Box", "x": 600, "y": 600, "width": 200, "height": 200}))?;
        Ok(())
    })
    .unwrap();
    let mut s = Session::new(s.into_doc(), None, FontDb::bundled_only()).unwrap();
    s.viewport = Vec2::new(1000.0, 800.0);
    s.camera = editor::Camera { zoom: 1.0, origin: Point::new(0.0, 0.0) };
    s
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(w, h, image::Rgba([255, 0, 0, 255]));
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
    out
}

#[test]
fn svg_lands_centred_and_undoes_in_one_step() {
    let mut s = session();
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="40"><rect width="40" height="40" fill="#f00"/><circle cx="80" cy="20" r="20"/></svg>"##;
    let (id, warnings) = place::svg(&mut s, svg, "Logo", None).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    let n = s.doc().get(id).unwrap();
    assert_eq!((n.name.as_str(), n.props.x, n.props.y, n.props.width), ("Logo", 450.0, 380.0, 100.0));
    assert_eq!(s.selection(), &[id]);
    assert_eq!(s.doc().key(id).and_then(|k| s.doc().node(k)).unwrap().children.len(), 2);
    editor::commands::execute(&mut s, editor::Command::Undo).unwrap();
    assert!(s.doc().get(id).is_none());
}

#[test]
fn images_keep_their_size_or_fit_the_view_and_go_into_a_selected_frame() {
    let mut s = session();
    let id = place::image(&mut s, png(64, 32), "Small", Some(Point::new(100.0, 100.0))).unwrap();
    let n = s.doc().get(id).unwrap();
    assert!(matches!(n.kind, NodeKind::Image { .. }));
    assert_eq!((n.props.x, n.props.y, n.props.width, n.props.height), (68.0, 84.0, 64.0, 32.0));
    assert_eq!(s.doc().assets().len(), 1);
    // Larger than the view: scaled to 80% of it.
    let big = place::image(&mut s, png(2000, 1000), "Big", None).unwrap();
    assert_eq!(s.doc().get(big).unwrap().props.width, 800.0);
    // Into the selected frame, at the drop point in its coordinates.
    let frame = s.doc().iter().find(|(_, n)| n.name == "Box").map(|(_, n)| n.id).unwrap();
    s.select(vec![frame]);
    let inner = place::image(&mut s, png(64, 32), "Inner", Some(Point::new(700.0, 700.0))).unwrap();
    let n = s.doc().get(inner).unwrap();
    assert_eq!((n.props.x, n.props.y), (68.0, 84.0));
    let doc = s.doc();
    assert!(doc.node(doc.key(frame).unwrap()).unwrap().children.contains(&doc.key(inner).unwrap()));
    // The same bytes are one asset.
    assert_eq!(s.doc().assets().len(), 2);
}
