//! Benchmarks for the `sessionize` function.
//!
//! Uses Criterion with 100+ samples and 95% confidence intervals.
#![allow(missing_docs)]

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use duckdb_behavioral::sessionize::SessionizeBoundaryState;

fn bench_sessionize_update(c: &mut Criterion) {
    let mut group = c.benchmark_group("sessionize_update");

    for &n in &[
        100,
        1_000,
        10_000,
        100_000,
        1_000_000,
        10_000_000,
        100_000_000,
        1_000_000_000,
    ] {
        group.throughput(Throughput::Elements(n as u64));
        // Use fewer samples for large inputs to keep benchmark time reasonable
        if n >= 100_000_000 {
            group.sample_size(10);
            group.measurement_time(std::time::Duration::from_secs(30));
        }
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter(|| {
                let mut state = SessionizeBoundaryState::new();
                state.threshold_us = 1_800_000_000; // 30 minutes
                for i in 0..n {
                    // Simulate events every 5 minutes, with a session break every 10 events
                    let ts = if i % 10 == 0 && i > 0 {
                        i64::from(i) * 300_000_000 + 2_000_000_000 // 33+ min gap
                    } else {
                        i64::from(i) * 300_000_000 // 5 min gap
                    };
                    state.update(black_box(ts));
                }
                state.finalize()
            });
        });
    }

    group.finish();
}

fn bench_sessionize_combine(c: &mut Criterion) {
    let mut group = c.benchmark_group("sessionize_combine");

    for &n in &[100, 1_000, 10_000, 100_000, 1_000_000] {
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            // Pre-build states
            let states: Vec<SessionizeBoundaryState> = (0..n)
                .map(|i| {
                    let mut s = SessionizeBoundaryState::new();
                    s.threshold_us = 1_800_000_000;
                    s.update(i64::from(i) * 300_000_000);
                    s
                })
                .collect();

            b.iter(|| {
                let mut combined = SessionizeBoundaryState::new();
                combined.threshold_us = 1_800_000_000;
                for s in &states {
                    combined = combined.combine(black_box(s));
                }
                combined.finalize()
            });
        });
    }

    group.finish();
}

criterion_group!(benches, bench_sessionize_update, bench_sessionize_combine);
criterion_main!(benches);
