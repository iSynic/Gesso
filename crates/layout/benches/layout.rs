//! Layout cost on the 10k-node stress tree: full layout with warm text sizes, and one
//! keystroke end to end (transaction, incremental layout, write-back).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use criterion::{Criterion, criterion_group, criterion_main};
use doc::testing::{GenConfig, STRESS_ROOT, generate};
use doc::*;
use layout::{LayoutDeriver, LayoutEngine, TaffyLayout};
use serde_json::json;
use text::{FontDb, TextEngine};

fn laid_out() -> Store {
    let doc = Document::from_data(generate(&GenConfig { target_nodes: 10, auto_layout_heavy: true, ..Default::default() })).unwrap();
    let mut s = Store::new(doc);
    s.set_deriver(Box::new(LayoutDeriver::new(TextEngine::new(FontDb::bundled_only()))));
    s.transact_with_deriver(TxMeta::user("layout"), |tx, d| d.unwrap().derive_all(tx)).unwrap();
    s
}

fn bench(c: &mut Criterion) {
    let mut s = laid_out();
    let mut text = TextEngine::new(FontDb::bundled_only());
    let mut out = Vec::new();
    let mut g = c.benchmark_group("layout");
    g.sample_size(20);
    g.bench_function("full_layout_10k_warm_text", |b| {
        b.iter(|| {
            let mut flow = TaffyLayout::new();
            out.clear();
            flow.invalidate_all(s.doc());
            flow.compute(s.doc(), &mut text, &mut out);
        })
    });
    g.finish();

    let root = s.doc().iter().find(|(_, n)| n.name == STRESS_ROOT).unwrap().1.id;
    let d = s.doc();
    let first = |id: NodeId| d.node(d.get(id).unwrap().children[0]).unwrap().id;
    let card = first(first(root));
    let body = d.node(d.get(card).unwrap().children[2]).unwrap().id;
    let mut typed = String::from("Typing ");
    c.bench_function("keystroke_in_10k_tree", |b| {
        b.iter(|| {
            typed.push(if typed.len() % 6 == 0 { ' ' } else { 'a' });
            if typed.len() > 400 {
                typed.truncate(7);
            }
            let props = json!({"spans": [{"text": typed}]});
            s.transact(TxMeta::user("type").coalesce("typing"), |tx| tx.update_json(body, props.as_object().unwrap())).unwrap();
        })
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
