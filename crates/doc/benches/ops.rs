//! Criterion micro-benchmarks for document ops at 100k nodes (CI regression gate).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use criterion::{Criterion, criterion_group, criterion_main};
use doc::testing::{GenConfig, generate};
use doc::*;

fn store_100k() -> (Store, Vec<NodeId>) {
    let doc = Document::from_data(generate(&GenConfig { target_nodes: 100_000, ..Default::default() })).unwrap();
    let rects = doc.iter().filter(|(_, n)| matches!(n.kind, NodeKind::Rectangle {})).map(|(_, n)| n.id).collect();
    (Store::new(doc), rects)
}

fn ops(c: &mut Criterion) {
    let (mut store, rects) = store_100k();
    let mut i = 0usize;
    c.bench_function("update_one_prop_100k", |b| {
        b.iter(|| {
            i += 1;
            let id = rects[i % rects.len()];
            store.transact(TxMeta::user("x"), |tx| tx.update(id, PropValue::X(i as f64))).unwrap();
        })
    });
    c.bench_function("tx_500_updates_then_undo_100k", |b| {
        b.iter(|| {
            store
                .transact(TxMeta::user("batch"), |tx| {
                    for id in rects.iter().take(500) {
                        tx.update(*id, PropValue::Opacity(0.5))?;
                    }
                    Ok(())
                })
                .unwrap();
            store.undo().unwrap();
        })
    });
    let data = generate(&GenConfig { target_nodes: 10_000, ..Default::default() });
    c.bench_function("from_data_10k", |b| b.iter(|| Document::from_data(data.clone()).unwrap()));
}

criterion_group!(benches, ops);
criterion_main!(benches);
