// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! Benchmarks for `sequence_match_events`.
//!
//! Measures update + finalize throughput at multiple input sizes.
//! Uses `finalize_events()` which returns `Vec<i64>` of matched
//! condition timestamps via the timestamp-collecting NFA variant.
#![allow(missing_docs, clippy::cast_possible_truncation)]

use behavioral::common::event::Event;
use behavioral::sequence::SequenceState;
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

fn make_sequence_events(num_events: usize, num_conditions: usize) -> Vec<Event> {
    (0..num_events)
        .map(|i| {
            let step = i % (num_conditions * 2);
            let bitmask = if step < num_conditions {
                1u32 << step
            } else {
                0u32
            };
            Event::new((i as i64) * 1_000_000, bitmask)
        })
        .collect()
}

fn bench_sequence_match_events(c: &mut Criterion) {
    let mut group = c.benchmark_group("sequence_match_events");

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
            let events = make_sequence_events(n, 3);
            b.iter(|| {
                let mut state = SequenceState::new();
                state.set_pattern("(?1).*(?2).*(?3)");
                for e in &events {
                    state.update(black_box(*e));
                }
                state.finalize_events().unwrap()
            });
        });
    }

    group.finish();
}

fn bench_sequence_match_events_combine(c: &mut Criterion) {
    let mut group = c.benchmark_group("sequence_match_events_combine");

    for &n in &[100_usize, 1_000, 10_000, 100_000, 1_000_000] {
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            let states: Vec<SequenceState> = (0..n)
                .map(|i| {
                    let mut s = SequenceState::new();
                    s.set_pattern("(?1).*(?2).*(?3)");
                    let bitmask = 1u32 << (i % 3);
                    s.update(Event::new((i as i64) * 1_000_000, bitmask));
                    s
                })
                .collect();

            b.iter(|| {
                let mut combined = SequenceState::new();
                combined.set_pattern("(?1).*(?2).*(?3)");
                for s in &states {
                    combined.combine_in_place(black_box(s));
                }
                combined.finalize_events().unwrap()
            });
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_sequence_match_events,
    bench_sequence_match_events_combine
);
criterion_main!(benches);
