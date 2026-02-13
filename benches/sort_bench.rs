//! Isolated benchmark for `sort_events()` — decomposes sort cost from scan/NFA cost.
//!
//! This benchmark measures only the sort phase of finalize, enabling attribution
//! of improvements to sort vs algorithm components. Combined with the sequence
//! and `window_funnel` benchmarks, enables Amdahl's Law analysis.
#![allow(missing_docs, clippy::cast_possible_truncation)]

use behavioral::common::event::{sort_events, Event};
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

fn make_random_events(num_events: usize) -> Vec<Event> {
    // Create events with reverse-order timestamps and varying conditions
    // to exercise the sort (not already-sorted fast path)
    (0..num_events)
        .map(|i| {
            let base = (num_events - i) as i64;
            let jitter = (i % 7) as i64;
            let ts = base * 1_000_000 + jitter * 100;
            let bitmask = 1u32 << (i % 3);
            Event::new(ts, bitmask)
        })
        .collect()
}

fn bench_sort_events(c: &mut Criterion) {
    let mut group = c.benchmark_group("sort_events");

    for &n in &[
        100_usize,
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
            let events = make_random_events(n);
            b.iter(|| {
                let mut data = events.clone();
                sort_events(black_box(&mut data));
                data
            });
        });
    }

    group.finish();
}

fn bench_sort_events_presorted(c: &mut Criterion) {
    let mut group = c.benchmark_group("sort_events_presorted");

    for &n in &[
        100_usize,
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
            // Already-sorted input — tests pdqsort's adaptive O(n) fast path
            let events: Vec<Event> = (0..n)
                .map(|i| {
                    let ts = (i as i64) * 1_000_000;
                    Event::new(ts, 1u32 << (i % 3))
                })
                .collect();
            b.iter(|| {
                let mut data = events.clone();
                sort_events(black_box(&mut data));
                data
            });
        });
    }

    group.finish();
}

criterion_group!(benches, bench_sort_events, bench_sort_events_presorted);
criterion_main!(benches);
