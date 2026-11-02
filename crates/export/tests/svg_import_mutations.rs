//! The `svg_import` fuzz target's property on stable Rust: the hand-written SVGs with random
//! byte edits (deterministic) import without panicking, and whatever imports inserts as valid
//! nodes.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use doc::{Store, TxMeta};
use serde_json::json;

/// xorshift64*, so failures reproduce.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// Bytes that make SVG interesting to break.
const TOKENS: &[&str] = &["<", ">", "/>", "\"", "-", "e9", "1e308", "NaN", "url(#a)", "<g>", "</g>", "transform=\"scale(0)\"", "d=\"M0 0", "%", "&#0;", "inf", "0"];

fn mutate(src: &str, rng: &mut Rng) -> String {
    let mut b = src.as_bytes().to_vec();
    for _ in 0..1 + rng.below(3) {
        let at = rng.below(b.len() + 1);
        match rng.below(4) {
            0 if !b.is_empty() => {
                let end = (at + 1 + rng.below(16)).min(b.len());
                b.drain(at.min(end)..end);
            }
            1 => {
                let t = TOKENS[rng.below(TOKENS.len())];
                b.splice(at..at, t.bytes());
            }
            2 if at < b.len() => b[at] = b"0123456789.-e "[rng.below(14)],
            _ => {
                // Duplicate a slice (deep nesting, repeated attributes).
                let from = rng.below(b.len());
                let end = (from + rng.below(64)).min(b.len());
                let piece = b[from..end].to_vec();
                b.splice(at..at, piece);
            }
        }
    }
    String::from_utf8_lossy(&b).into_owned()
}

#[test]
fn mutated_svgs_import_without_panicking() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/svg");
    let mut files: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "svg")).collect();
    files.sort();
    let mut rng = Rng(0x5eed_1234_abcd_ef01);
    let (mut ok, mut total) = (0, 0);
    for f in &files {
        let src = std::fs::read_to_string(f).unwrap();
        for _ in 0..150 {
            let m = mutate(&src, &mut rng);
            total += 1;
            let Ok(imp) = export::import::import_svg(&m, "Fuzz", &[]) else { continue };
            ok += 1;
            let mut store = Store::default();
            store
                .transact(TxMeta::user("import"), |tx| {
                    for bytes in &imp.images {
                        let (id, meta) = format::identify_asset(bytes).unwrap();
                        tx.put_asset(id, meta)?;
                    }
                    let page = tx.insert_json(None, None, json!({"type": "page"}))?;
                    tx.insert_json(Some(page), None, imp.node.clone())
                })
                .unwrap_or_else(|e| panic!("{}: invalid nodes from a mutation: {e}\n{m}", f.display()));
        }
    }
    println!("{ok} of {total} mutations imported");
    assert!(ok > total / 10, "enough mutations should still parse to exercise the importer ({ok} of {total})");
}
