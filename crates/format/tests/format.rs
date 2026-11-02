//! File format: round trips, fixtures, schema, migrations, bundle I/O and error reporting.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use doc::testing::{GenConfig, generate};
use doc::*;
use format::*;
use serde_json::{Value, json};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn validator() -> jsonschema::Validator {
    jsonschema::validator_for(&schema()).unwrap()
}

fn assert_valid(v: &Value) {
    let errors: Vec<String> = validator().iter_errors(v).map(|e| format!("{} at {}", e, e.instance_path())).take(5).collect();
    assert!(errors.is_empty(), "schema errors: {errors:#?}");
}

#[test]
fn save_load_save_is_byte_identical() {
    let data = generate(&GenConfig { target_nodes: 3_000, ..Default::default() });
    let doc = Document::from_data(data).unwrap();
    let a = to_json(&doc);
    let b = to_json(&from_json(a.as_bytes()).unwrap());
    assert_eq!(a, b);
    assert!(a.starts_with("{\n  \"formatVersion\": 2,\n"));
}

#[test]
fn checked_in_schema_is_current() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("schema/document.v2.schema.json");
    let current = serde_json::to_string_pretty(&schema()).unwrap() + "\n";
    if std::env::var_os("UPDATE_SCHEMA").is_some() {
        std::fs::write(&path, &current).unwrap();
    }
    let on_disk = std::fs::read_to_string(&path).unwrap_or_default().replace("\r\n", "\n");
    assert!(on_disk == current, "schema/document.v2.schema.json is stale; rerun with UPDATE_SCHEMA=1");
}

#[test]
fn generated_documents_match_the_schema() {
    let doc = Document::from_data(generate(&GenConfig { target_nodes: 500, ..Default::default() })).unwrap();
    assert_valid(&serde_json::from_str(&to_json(&doc)).unwrap());
}

#[test]
fn fixture_with_every_kind_opens_validates_and_round_trips() {
    let dir = fixtures().join("all-kinds.canvas");
    let raw: Value = serde_json::from_slice(&std::fs::read(dir.join(DOCUMENT_FILE)).unwrap()).unwrap();
    assert_valid(&raw);
    let opened = Bundle::open(&dir).unwrap();
    assert!(opened.missing_assets.is_empty(), "{:?}", opened.missing_assets);
    let d = &opened.doc;
    d.check_invariants().unwrap();
    let mut kinds: Vec<&str> = d.iter().map(|(_, n)| n.kind.type_name()).collect();
    kinds.sort();
    kinds.dedup();
    assert_eq!(kinds, ["component", "ellipse", "frame", "group", "image", "instance", "line", "note", "page", "path", "polygon", "rectangle", "text"]);
    let again = from_json(to_json(d).as_bytes()).unwrap();
    assert_eq!(again.to_data(), d.to_data());
    // Canonical form drops nothing: reserialising the fixture keeps every value.
    let canon: Value = serde_json::from_str(&to_json(d)).unwrap();
    assert_eq!(canon["pages"][0]["children"][1]["children"][0]["effects"].as_array().unwrap().len(), 4);
}

#[test]
fn v0_files_are_migrated() {
    let bytes = std::fs::read(fixtures().join("v0.json")).unwrap();
    let doc = from_json(&bytes).unwrap();
    let r1 = doc.get("0Rc00000000000000000R1".parse().unwrap()).unwrap();
    assert_eq!(r1.props.radius, doc::model::Radius::Uniform(8.0));
    let r2 = doc.get("0Rc00000000000000000R2".parse().unwrap()).unwrap();
    assert_eq!(r2.props.radius, doc::model::Radius::Each([1.0, 2.0, 3.0, 4.0]));
    let migrated = migrate(serde_json::from_slice(&bytes).unwrap()).unwrap();
    assert_eq!(migrated["formatVersion"], 2);
    assert_valid(&migrated);
}

#[test]
fn v1_override_targets_become_paths() {
    let bytes = std::fs::read(fixtures().join("v1.json")).unwrap();
    let migrated = migrate(serde_json::from_slice(&bytes).unwrap()).unwrap();
    assert_eq!(migrated["formatVersion"], 2);
    assert_valid(&migrated);
    let v2: Value = serde_json::from_slice(&std::fs::read(fixtures().join("all-kinds.canvas").join(DOCUMENT_FILE)).unwrap()).unwrap();
    assert_eq!(migrated["pages"], v2["pages"], "migrating the v1 copy gives the v2 fixture");
    let doc = from_json(&bytes).unwrap();
    let instances: Vec<_> = doc.iter().filter_map(|(_, n)| match &n.kind {
        NodeKind::Instance { overrides, .. } => Some(overrides.iter().map(|o| o.target.len()).collect::<Vec<_>>()),
        _ => None,
    }).collect();
    assert_eq!(instances, [vec![1, 0]]);
}

#[test]
fn newer_versions_and_bad_input_give_clear_errors() {
    let e = from_json(br#"{"formatVersion": 9, "pages": []}"#).unwrap_err();
    assert!(matches!(e, FormatError::TooNew(9)), "{e}");
    let e = from_json(b"{\"formatVersion\": 1, \"pages\": [ {").unwrap_err();
    assert!(e.to_string().contains("invalid JSON"), "{e}");
    let bad = json!({"formatVersion": 1, "pages": [{"id": "0Pg00000000000000000P1", "type": "page", "children": [{"id": "0Pg00000000000000000P2", "type": "page"}]}]});
    let e = from_json(bad.to_string().as_bytes()).unwrap_err();
    assert!(e.to_string().contains("must be at the top level"), "{e}");
    let bad = json!({"formatVersion": 1, "pages": [{"id": "short", "type": "page"}]});
    let e = from_json(bad.to_string().as_bytes()).unwrap_err();
    assert!(e.to_string().contains("22 base62"), "{e}");
    // Files whose header was moved away from the start still load.
    let moved = json!({"pages": [], "formatVersion": 1});
    assert!(from_json(moved.to_string().as_bytes()).unwrap().is_empty());
}

#[test]
fn bundle_save_open_and_assets() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Demo.canvas");
    let bundle = Bundle::create(&root).unwrap();
    let png = std::fs::read(fixtures().join("tiny.png")).unwrap();
    let (id, meta) = bundle.add_asset(&png).unwrap();
    assert_eq!((meta.mime.as_str(), meta.width, meta.height), ("image/png", 2, 2));
    let (id2, _) = bundle.add_asset(&png).unwrap();
    assert_eq!(id, id2);
    assert_eq!(std::fs::read_dir(root.join(ASSETS_DIR)).unwrap().count(), 1);
    assert!(bundle.add_asset(b"not an image").is_err());

    let mut s = Store::default().with_ids(IdGen::seeded(3));
    s.transact(TxMeta::user("build"), |tx| {
        tx.put_asset(id.clone(), meta.clone())?;
        let page = tx.insert_json(None, None, json!({"type": "page", "name": "Home"}))?;
        tx.insert_json(Some(page), None, json!({"type": "image", "asset": id.0, "width": 2, "height": 2}))
    })
    .unwrap();
    bundle.save(s.doc()).unwrap();
    assert!(!root.join("document.tmp").exists());
    let opened = Bundle::open(&root).unwrap();
    assert_eq!(opened.doc.to_data(), s.doc().to_data());
    assert!(opened.missing_assets.is_empty());
    assert_eq!(opened.bundle.read_asset(&id, &meta).unwrap(), png);

    std::fs::remove_file(bundle.asset_path(&id, &meta)).unwrap();
    assert_eq!(Bundle::open(&root).unwrap().missing_assets, vec![id]);
}

#[test]
fn file_reads_well_for_people() {
    let mut s = Store::default().with_ids(IdGen::seeded(1));
    s.transact(TxMeta::user("x"), |tx| {
        let page = tx.insert_json(None, None, json!({"type": "page", "name": "Home"}))?;
        tx.insert_json(Some(page), None, json!({"type": "rectangle", "name": "Box", "width": 100, "height": 50, "fills": [{"type": "solid", "color": "#ff0000"}]}))
    })
    .unwrap();
    let text = to_json(s.doc());
    let expected = r##"{
  "formatVersion": 2,
  "generator": "GEN",
  "pages": [
    {
      "id": "ID1",
      "type": "page",
      "name": "Home",
      "width": 0.0,
      "height": 0.0,
      "children": [
        {
          "id": "ID2",
          "type": "rectangle",
          "name": "Box",
          "width": 100.0,
          "height": 50.0,
          "fills": [
            {
              "type": "solid",
              "color": "#ff0000"
            }
          ]
        }
      ]
    }
  ]
}
"##;
    let d = s.doc();
    let page = d.node(d.pages()[0]).unwrap();
    let rect = d.node(page.children[0]).unwrap();
    let expected = expected.replace("GEN", GENERATOR).replace("ID1", &page.id.to_string()).replace("ID2", &rect.id.to_string());
    assert_eq!(text, expected);
}
