// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! Benchmarks for the `window_funnel` function.
//!
//! Measures update + finalize throughput and combine overhead at multiple
//! input sizes to validate O(n*k) complexity where n = events, k = conditions.
//!
//! Event-collecting function: limited to 100M elements by memory (16 bytes/event,
//! 1.6GB at 100M, requires ~3.2GB with Criterion clone per iteration).
//!
//! Uses Criterion with 100+ samples and 95% confidence intervals.
#![allow(missing_docs, clippy::cast_possible_truncation)]

use behavioral::common::event::Event;
use behavioral::window_funnel::WindowFunnelState;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::hint::black_box;

fn make_funnel_events(num_events: usize, num_conditions: usize) -> Vec<Event> {
    (0..num_events)
        .map(|i| {
            // Each condition fires for roughly 1/num_conditions of events
            let step = i % (num_conditions * 3);
            let bitmask = if step < num_conditions {
                1u32 << step
            } else {
                0u32
            };
            Event::new((i as i64) * 1_000_000, bitmask)
        })
        .collect()
}

fn bench_window_funnel_finalize(c: &mut Criterion) {
    let mut group = c.benchmark_group("window_funnel_finalize");

    for &(events, conditions) in &[
        (100, 3),
        (1_000, 5),
        (10_000, 5),
        (100_000, 8),
        (1_000_000, 8),
        (10_000_000, 8),
        (100_000_000, 8),
    ] {
        group.throughput(Throughput::Elements(events as u64));
        if events >= 10_000_000 {
            group.sample_size(10);
        }
        if events >= 100_000_000 {
            group.measurement_time(std::time::Duration::from_secs(60));
        }
        group.bench_with_input(
            BenchmarkId::new(format!("events={events},conds={conditions}"), events),
            &(events, conditions),
            |b, &(events, conditions)| {
                let event_data = make_funnel_events(events, conditions);
                b.iter(|| {
                    let mut state = WindowFunnelState::new();
                    state.window_size_us = 3_600_000_000; // 1 hour
                    for e in &event_data {
                        state.update(black_box(*e), conditions);
                    }
                    state.finalize()
                });
            },
        );
    }

    group.finish();
}

fn bench_window_funnel_combine(c: &mut Criterion) {
    let mut group = c.benchmark_group("window_funnel_combine");

    for &n in &[100_usize, 1_000, 10_000, 100_000, 1_000_000] {
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            let states: Vec<WindowFunnelState> = (0..n)
                .map(|i| {
                    let mut s = WindowFunnelState::new();
                    s.window_size_us = 3_600_000_000;
                    let bitmask = 1u32 << (i % 5);
                    s.update(Event::new((i as i64) * 1_000_000, bitmask), 5);
                    s
                })
                .collect();

            b.iter(|| {
                let mut combined = WindowFunnelState::new();
                combined.window_size_us = 3_600_000_000;
                combined.num_conditions = 5;
                for s in &states {
                    combined.combine_in_place(black_box(s));
                }
                combined.finalize()
            });
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_window_funnel_finalize,
    bench_window_funnel_combine
);
criterion_main!(benches);
