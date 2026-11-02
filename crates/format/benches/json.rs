//! Criterion benchmarks for document.json serialise/parse at 10k nodes (CI regression gate).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use criterion::{Criterion, criterion_group, criterion_main};
use doc::Document;
use doc::testing::{GenConfig, generate};

fn json(c: &mut Criterion) {
    let doc = Document::from_data(generate(&GenConfig { target_nodes: 10_000, ..Default::default() })).unwrap();
    let text = format::to_json(&doc);
    c.bench_function("to_json_10k", |b| b.iter(|| format::to_json(&doc)));
    c.bench_function("from_json_10k", |b| b.iter(|| format::from_json(text.as_bytes()).unwrap()));
}

criterion_group!(benches, json);
criterion_main!(benches);
