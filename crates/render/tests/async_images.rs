//! Background image decoding: frames don't wait, draw a placeholder, and redraw the image once
//! the waker reports the decode finished. Complete frames (screenshots) wait for it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::mpsc::channel;
use std::time::Duration;

use doc::{Document, Store, TxMeta};
use kurbo::Affine;
use render::{Brush, Cmd, Frame, MemoryAssets, Scene};
use serde_json::json;
use text::FontDb;

fn has_image(f: &Frame) -> bool {
    f.items.iter().any(|it| it.list.cmds.iter().any(|c| matches!(c, Cmd::Fill { brush: Brush::Image(_), .. })))
}

fn scene() -> (Document, Scene) {
    let img = image::RgbaImage::from_pixel(256, 256, image::Rgba([0, 128, 255, 255]));
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
    let (id, meta) = format::identify_asset(&png).unwrap();
    let mut assets = MemoryAssets::default();
    assets.0.insert(id.clone(), png);
    let mut s = Store::new(Document::new());
    let page = s
        .transact(TxMeta::user("setup"), |tx| {
            tx.put_asset(id.clone(), meta)?;
            let page = tx.insert_json(None, None, json!({"type": "page"}))?;
            tx.insert_json(Some(page), None, json!({"type": "image", "asset": id.0, "width": 100, "height": 100}))?;
            Ok(page)
        })
        .unwrap()
        .0;
    let doc = s.into_doc();
    let scene = Scene::new(&doc, page, FontDb::bundled_only(), Arc::new(assets));
    (doc, scene)
}

#[test]
fn frames_draw_a_placeholder_until_the_decode_lands() {
    let (doc, mut scene) = scene();
    let (tx, rx) = channel();
    let tx = std::sync::Mutex::new(tx);
    scene.set_image_waker(Some(Arc::new(move || {
        let _ = tx.lock().unwrap().send(());
    })));
    let f = scene.frame(&doc, Affine::IDENTITY, 200, 200, None);
    assert!(!has_image(&f), "the first frame must not wait for the decode");
    rx.recv_timeout(Duration::from_secs(10)).expect("the waker runs when the decode finishes");
    let f = scene.frame(&doc, Affine::IDENTITY, 200, 200, None);
    assert!(has_image(&f), "the fragment is rebuilt with the image");
    assert_eq!(scene.images_pending(), 0);
}

#[test]
fn complete_frames_wait_for_background_decodes() {
    let (doc, mut scene) = scene();
    scene.set_image_waker(Some(Arc::new(|| {})));
    let _ = scene.frame(&doc, Affine::IDENTITY, 200, 200, None);
    let f = scene.frame_complete(&doc, Affine::IDENTITY, 200, 200);
    assert!(has_image(&f));
    // Still asynchronous afterwards.
    assert!(scene.images.is_async());
}

#[test]
fn without_a_waker_frames_decode_first() {
    let (doc, mut scene) = scene();
    assert!(has_image(&scene.frame(&doc, Affine::IDENTITY, 200, 200, None)));
}
