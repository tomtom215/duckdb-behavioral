//! Benchmarks for the `sessionize` function.
//!
//! Measures update + finalize throughput and combine overhead at multiple
//! input sizes. Sessionize is O(1) state, enabling benchmarks up to 1 billion
//! elements without memory constraints.
//!
//! Uses Criterion with 100+ samples and 95% confidence intervals.
#![allow(missing_docs)]

use behavioral::sessionize::SessionizeBoundaryState;
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

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

    // SessionizeBoundaryState is O(1) per state (~32 bytes), but combine
    // benchmarks pre-allocate all states in a Vec. At 1B states this requires
    // ~32GB which exceeds available RAM. Capped at 100M (3.2GB).
    for &n in &[
        100,
        1_000,
        10_000,
        100_000,
        1_000_000,
        10_000_000,
        100_000_000,
    ] {
        group.throughput(Throughput::Elements(n as u64));
        if n >= 100_000_000 {
            group.sample_size(10);
            group.measurement_time(std::time::Duration::from_secs(30));
        }
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            // Pre-build states: each state is ~32 bytes (O(1) per state)
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
