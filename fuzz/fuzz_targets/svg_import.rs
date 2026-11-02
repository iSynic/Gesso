//! Arbitrary text as SVG: import must return Ok or Err, never panic; anything imported must
//! insert into a document (valid nodes) with every image it references returned.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(source) = std::str::from_utf8(data) else { return };
    let Ok(imp) = export::import::import_svg(source, "Fuzz", &[]) else { return };
    let mut store = doc::Store::default();
    store
        .transact(doc::TxMeta::user("import"), |tx| {
            for bytes in &imp.images {
                let (id, meta) = format::identify_asset(bytes).expect("imported images are decodable");
                tx.put_asset(id, meta)?;
            }
            let page = tx.insert_json(None, None, serde_json::json!({"type": "page"}))?;
            tx.insert_json(Some(page), None, imp.node.clone())
        })
        .expect("imported nodes are valid");
    store.doc().check_invariants().unwrap();
});
