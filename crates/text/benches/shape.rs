//! Shaping cost: one label cold, a cache hit, and 2,000 labels sequential vs parallel.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use criterion::{Criterion, criterion_group, criterion_main};
use doc::model::{TextAlign, TextSpan, TextStyle};
use text::{FontDb, TextEngine, TextInput};

fn labels(n: usize) -> Vec<[TextSpan; 1]> {
    (0..n).map(|i| [TextSpan { text: format!("Card title number {i}"), ..Default::default() }]).collect()
}

fn bench(c: &mut Criterion) {
    let st = TextStyle::default();
    let one = labels(1);
    let input = TextInput { spans: &one[0], style: &st, align: TextAlign::Left };
    c.bench_function("shape_label_cold", |b| {
        let mut e = TextEngine::new(FontDb::bundled_only());
        b.iter(|| {
            e.clear_cache();
            e.shape(input, Some(200.0))
        })
    });
    c.bench_function("shape_label_cached", |b| {
        let mut e = TextEngine::new(FontDb::bundled_only());
        b.iter(|| e.shape(input, Some(200.0)))
    });
    let many = labels(2000);
    let inputs: Vec<_> = many.iter().map(|s| (TextInput { spans: s, style: &st, align: TextAlign::Left }, None)).collect();
    let mut g = c.benchmark_group("shape_2000_labels");
    g.sample_size(10);
    g.bench_function("sequential", |b| {
        let mut e = TextEngine::new(FontDb::bundled_only());
        b.iter(|| {
            e.clear_cache();
            inputs.iter().map(|(i, w)| e.shape(*i, *w).width).sum::<f32>()
        })
    });
    g.bench_function("parallel", |b| {
        let mut e = TextEngine::new(FontDb::bundled_only());
        b.iter(|| {
            e.clear_cache();
            e.shape_many(&inputs).len()
        })
    });
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
