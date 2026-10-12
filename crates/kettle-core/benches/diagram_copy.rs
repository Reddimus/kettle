use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use kettle_core::diagram_sources;
use std::hint::black_box;

fn copied_diagrams(c: &mut Criterion) {
    let mut group = c.benchmark_group("diagram_copy");
    for bytes in [64 * 1024, 256 * 1024, 1024 * 1024] {
        for (name, unit) in [
            ("front_matter", "---\ngraph\n"),
            ("directives", "%%{\n}%%\n"),
        ] {
            let text = unit.repeat(bytes / unit.len());
            group.throughput(Throughput::Bytes(text.len() as u64));
            group.bench_with_input(BenchmarkId::new(name, bytes), &text, |b, text| {
                b.iter(|| diagram_sources(black_box(text), Some(80)));
            });
        }
    }
    group.finish();
}

criterion_group!(benches, copied_diagrams);
criterion_main!(benches);
