//! PDF export: every render fixture exports; structure (page box, embedded subset fonts with
//! ToUnicode, balanced graphics state) and content-stream goldens.
//! `UPDATE_GOLDENS=1` re-blesses the content goldens in `tests/golden/`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "../../render-cpu/tests/common/mod.rs"]
mod common;

use std::io::Read;
use std::path::Path;

/// The PDF's streams, inflated where compressed, with their dictionaries.
fn streams(pdf: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(i) = find(&pdf[at..], b"stream\n") {
        let start = at + i + b"stream\n".len();
        let Some(len) = find(&pdf[start..], b"\nendstream") else { break };
        let dict_start = rfind(&pdf[..at + i], b"<<").unwrap_or(0);
        let dict = String::from_utf8_lossy(&pdf[dict_start..at + i]).into_owned();
        let raw = &pdf[start..start + len];
        let data = if dict.contains("/FlateDecode") {
            let mut v = Vec::new();
            flate2::read::ZlibDecoder::new(raw).read_to_end(&mut v).unwrap();
            v
        } else {
            raw.to_vec()
        };
        out.push((dict, data));
        at = start + len + b"
endstream".len();
    }
    out
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

fn rfind(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).rposition(|w| w == n)
}

fn export(name: &str) -> (export::Content, Vec<u8>) {
    let f = common::load(name);
    let page = f.doc.key(f.page).unwrap();
    let ids: Vec<_> = f.doc.node(page).unwrap().children.iter().map(|k| f.doc.node(*k).unwrap().id).collect();
    let content = export::collect(&f.doc, &ids, text::FontDb::bundled_only(), &common::assets()).unwrap();
    let pdf = export::pdf::write(&content, 1.0).unwrap();
    (content, pdf)
}

/// Content streams (page and forms) have balanced q/Q and BT/ET.
fn balanced(data: &[u8]) -> bool {
    let s = String::from_utf8_lossy(data);
    let (mut q, mut bt) = (0i32, 0i32);
    for tok in s.split_ascii_whitespace() {
        match tok {
            "q" => q += 1,
            "Q" => q -= 1,
            "BT" => bt += 1,
            "ET" => bt -= 1,
            _ => {}
        }
        if q < 0 || bt < 0 {
            return false;
        }
    }
    q == 0 && bt == 0
}

#[test]
fn every_fixture_exports_a_well_formed_pdf() {
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/pdf-out");
    std::fs::create_dir_all(&out).unwrap();
    for name in common::names() {
        let (content, pdf) = export(&name);
        std::fs::write(out.join(format!("{name}.pdf")), &pdf).unwrap();
        assert!(pdf.starts_with(b"%PDF-1.7"), "{name}: header");
        assert!(pdf.ends_with(b"%%EOF\n") || pdf.ends_with(b"%%EOF"), "{name}: trailer");
        let mb = format!("/MediaBox [0 0 {} {}]", content.width, content.height);
        assert!(find(&pdf, mb.as_bytes()).is_some(), "{name}: page box {mb}");
        for (dict, data) in streams(&pdf) {
            if !dict.contains("/Subtype /Image") && !dict.contains("/FontFile") && !dict.contains("/Length1") && !dict.contains("/Type /CMap") && !dict.contains("/Subtype /OpenType") {
                assert!(balanced(&data), "{name}: unbalanced content stream:\n{}", String::from_utf8_lossy(&data));
            }
        }
    }
}

#[test]
fn text_is_embedded_and_selectable() {
    let (_, pdf) = export("text_spans");
    let s = String::from_utf8_lossy(&pdf);
    assert!(s.contains("/Subtype /Type0") && s.contains("/Encoding /Identity-H"), "Type0 font");
    assert!(s.contains("/Subtype /CIDFontType2") && s.contains("/CIDToGIDMap /Identity"), "CID font");
    assert!(s.contains("/FontFile2"), "embedded font program");
    // Subset tag: six capitals and a plus.
    let base = s.split("/BaseFont /").nth(1).unwrap();
    assert!(base.as_bytes()[..6].iter().all(u8::is_ascii_uppercase) && base.as_bytes()[6] == b'+', "subset tag in {}", &base[..20]);
    let streams = streams(&pdf);
    let cmaps: Vec<String> = streams.iter().filter(|(d, _)| d.contains("/Type /CMap")).map(|(_, v)| String::from_utf8_lossy(v).into_owned()).collect();
    assert!(!cmaps.is_empty(), "ToUnicode map");
    // Every text drawn is mapped back: the fixture's words appear as UTF-16 in the maps.
    let f = common::load("text_spans");
    let mut words = Vec::new();
    for (_, n) in f.doc.iter() {
        if let doc::NodeKind::Text { spans, .. } = &n.kind {
            for sp in spans {
                words.extend(sp.text.chars().filter(|c| c.is_alphanumeric()));
            }
        }
    }
    assert!(!words.is_empty());
    let all = cmaps.join("\n");
    for c in words {
        let hex: String = c.encode_utf16(&mut [0; 2]).iter().map(|u| format!("{u:04X}")).collect();
        assert!(all.contains(&format!("<{hex}>")), "{c:?} has no ToUnicode entry");
    }
    // The subset is small: Inter in full is over 300 KB.
    let font = streams.iter().find(|(d, _)| d.contains("/Length1") || d.contains("FontFile")).map(|(_, v)| v.len());
    let font = font.or_else(|| streams.iter().map(|(_, v)| v.len()).max()).unwrap();
    assert!(font < 120_000, "font stream {font} bytes");
}

#[test]
fn content_stream_goldens() {
    let update = std::env::var("UPDATE_GOLDENS").is_ok_and(|v| v == "1");
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    std::fs::create_dir_all(&dir).unwrap();
    let mut failures = Vec::new();
    for name in ["shapes_basic", "strokes_caps_joins", "gradients_linear", "opacity_nested", "text_basic", "transforms", "clipping"] {
        let (_, pdf) = export(name);
        let page = streams(&pdf).into_iter().find(|(d, data)| !d.contains("/Subtype") && data.starts_with(b"1 0 0 -1")).map(|(_, v)| v).unwrap();
        let text = String::from_utf8_lossy(&page).into_owned();
        let path = dir.join(format!("{name}.pdf.txt"));
        if update {
            std::fs::write(&path, &text).unwrap();
            continue;
        }
        match std::fs::read_to_string(&path) {
            Ok(g) if g.replace("\r\n", "\n") == text => {}
            Ok(_) => failures.push(format!("{name}: content differs from {}", path.display())),
            Err(_) => failures.push(format!("{name}: no golden (run with UPDATE_GOLDENS=1)")),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
