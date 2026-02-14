// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! Benchmarks for `sequence_next_node`.
//!
//! Measures update + finalize throughput at multiple input sizes.
//! Tests forward matching with `first_match` base (the common case).
//!
//! Event-collecting function with String storage: `NextNodeEvent` is 32 bytes
//! with `Arc<str>`. Memory-limited to ~50M unique-string events or ~100M
//! realistic-cardinality events on a 21GB system.
//!
//! Uses Criterion with 100+ samples and 95% confidence intervals.
#![allow(missing_docs, clippy::cast_possible_truncation)]

use behavioral::sequence_next_node::{Base, Direction, NextNodeEvent, SequenceNextNodeState};
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::sync::Arc;

fn make_next_node_events(num_events: usize, num_steps: usize) -> Vec<NextNodeEvent> {
    (0..num_events)
        .map(|i| {
            let step = i % (num_steps * 2);
            let bitmask = if step < num_steps { 1u32 << step } else { 0u32 };
            NextNodeEvent::new(
                (i as i64) * 1_000_000,
                Some(Arc::from(format!("page_{i}").as_str())),
                step == 0,
                bitmask,
            )
        })
        .collect()
}

fn bench_sequence_next_node(c: &mut Criterion) {
    let mut group = c.benchmark_group("sequence_next_node");

    for &n in &[100, 1_000, 10_000, 100_000, 1_000_000, 10_000_000] {
        group.throughput(Throughput::Elements(n as u64));
        if n >= 1_000_000 {
            group.sample_size(10);
        }
        if n >= 10_000_000 {
            group.measurement_time(std::time::Duration::from_secs(60));
        }
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            let events = make_next_node_events(n, 3);
            b.iter(|| {
                let mut state = SequenceNextNodeState::new();
                state.direction = Some(Direction::Forward);
                state.base = Some(Base::FirstMatch);
                state.num_steps = 3;
                for e in &events {
                    state.update(black_box(e.clone()));
                }
                state.finalize()
            });
        });
    }

    group.finish();
}

fn bench_sequence_next_node_combine(c: &mut Criterion) {
    let mut group = c.benchmark_group("sequence_next_node_combine");

    for &n in &[100_usize, 1_000, 10_000, 100_000, 1_000_000] {
        group.throughput(Throughput::Elements(n as u64));
        if n >= 1_000_000 {
            group.sample_size(10);
            group.measurement_time(std::time::Duration::from_secs(30));
        }
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            let states: Vec<SequenceNextNodeState> = (0..n)
                .map(|i| {
                    let mut s = SequenceNextNodeState::new();
                    s.direction = Some(Direction::Forward);
                    s.base = Some(Base::FirstMatch);
                    s.num_steps = 2;
                    let bitmask = 1u32 << (i % 2);
                    s.update(NextNodeEvent::new(
                        (i as i64) * 1_000_000,
                        Some(Arc::from(format!("page_{i}").as_str())),
                        i % 2 == 0,
                        bitmask,
                    ));
                    s
                })
                .collect();

            b.iter(|| {
                let mut combined = SequenceNextNodeState::new();
                combined.direction = Some(Direction::Forward);
                combined.base = Some(Base::FirstMatch);
                combined.num_steps = 2;
                for s in &states {
                    combined.combine_in_place(black_box(s));
                }
                combined.finalize()
            });
        });
    }

    group.finish();
}

/// Realistic cardinality benchmark: events draw from a pool of 100 distinct
/// string values, matching typical behavioral analytics workloads where page
/// names / action types have low cardinality across millions of events.
fn bench_sequence_next_node_realistic(c: &mut Criterion) {
    let mut group = c.benchmark_group("sequence_next_node_realistic");

    // Pre-generate a pool of 100 distinct Arc<str> values
    let pool: Vec<Arc<str>> = (0..100)
        .map(|i| Arc::from(format!("page_{i}").as_str()))
        .collect();

    for &n in &[100, 1_000, 10_000, 100_000, 1_000_000, 10_000_000] {
        group.throughput(Throughput::Elements(n as u64));
        if n >= 1_000_000 {
            group.sample_size(10);
        }
        if n >= 10_000_000 {
            group.measurement_time(std::time::Duration::from_secs(60));
        }
        let pool_clone = pool.clone();
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            let events: Vec<NextNodeEvent> = (0..n)
                .map(|i| {
                    let step = i % 6; // 3 steps * 2
                    let bitmask = if step < 3 { 1u32 << step } else { 0u32 };
                    NextNodeEvent::new(
                        (i as i64) * 1_000_000,
                        Some(Arc::clone(&pool_clone[i % 100])),
                        step == 0,
                        bitmask,
                    )
                })
                .collect();
            b.iter(|| {
                let mut state = SequenceNextNodeState::new();
                state.direction = Some(Direction::Forward);
                state.base = Some(Base::FirstMatch);
                state.num_steps = 3;
                for e in &events {
                    state.update(black_box(e.clone()));
                }
                state.finalize()
            });
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_sequence_next_node,
    bench_sequence_next_node_combine,
    bench_sequence_next_node_realistic
);
criterion_main!(benches);
