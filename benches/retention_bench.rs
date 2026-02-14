// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! Benchmarks for the `retention` function.
//!
//! Measures update throughput across condition counts and combine overhead.
//! Retention uses O(1) state (u32 bitmask), enabling combine benchmarks
//! up to 1 billion elements without memory constraints.
//!
//! Uses Criterion with 100+ samples and 95% confidence intervals.
#![allow(missing_docs)]

use behavioral::retention::RetentionState;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::hint::black_box;

fn bench_retention_update(c: &mut Criterion) {
    let mut group = c.benchmark_group("retention_update");

    // Test update throughput across different condition counts
    for &num_conditions in &[4, 8, 16, 32] {
        group.throughput(Throughput::Elements(1000));
        group.bench_with_input(
            BenchmarkId::new("conditions", num_conditions),
            &num_conditions,
            |b, &num_conditions| {
                b.iter(|| {
                    let mut state = RetentionState::new();
                    for i in 0..1000 {
                        let mut conds = vec![false; num_conditions];
                        conds[i % num_conditions] = true;
                        state.update(black_box(&conds));
                    }
                    state.finalize()
                });
            },
        );
    }

    group.finish();
}

fn bench_retention_combine(c: &mut Criterion) {
    let mut group = c.benchmark_group("retention_combine");

    // RetentionState is O(1) per state (~8 bytes), but combine benchmarks
    // pre-allocate all states in a Vec. At 1B states this requires ~8GB
    // plus Criterion iteration overhead. Capped at 100M for stability.
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
            group.measurement_time(std::time::Duration::from_secs(60));
        }
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            // Pre-build states using direct bitmask construction to avoid
            // 1B heap allocations for temporary Vec<bool> in large benchmarks
            let states: Vec<RetentionState> = (0..n)
                .map(|i| {
                    let mut s = RetentionState::new();
                    s.conditions_met = 1u32 << (i % 8);
                    s.num_conditions = 8;
                    s
                })
                .collect();

            b.iter(|| {
                let mut combined = RetentionState::new();
                for s in &states {
                    combined = combined.combine(black_box(s));
                }
                combined.finalize()
            });
        });
    }

    group.finish();
}

criterion_group!(benches, bench_retention_update, bench_retention_combine);
criterion_main!(benches);
