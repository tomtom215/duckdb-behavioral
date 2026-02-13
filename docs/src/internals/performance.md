# Performance

All performance claims in this project are backed by measured data from
[Criterion.rs](https://bheisler.github.io/criterion.rs/book/) benchmarks with
95% confidence intervals, validated across three or more consecutive runs.
Improvements are accepted only when confidence intervals do not overlap.

## Current Baseline

Measured on the development machine. Absolute numbers vary by hardware; relative
improvements and algorithmic scaling are hardware-independent.

### Headline Numbers

| Function | Scale | Wall Clock | Throughput |
|---|---|---|---|
| `sessionize` | 1 billion | 1.16 s | 862 Melem/s |
| `retention` (combine) | 1 billion | 2.96 s | 338 Melem/s |
| `window_funnel` | 100 million | 761 ms | 131 Melem/s |
| `sequence_match` | 100 million | 1.05 s | 95 Melem/s |
| `sequence_count` | 100 million | 1.45 s | 69 Melem/s |
| `sequence_next_node` | 10 million | 1.23 s | 8.1 Melem/s |

### Per-Element Cost

| Function | Cost per Element | Bound |
|---|---|---|
| `sessionize` | 1.2 ns | CPU-bound (register-only loop) |
| `retention` | 3.0 ns | CPU-bound (bitmask OR) |
| `window_funnel` | 7.6 ns | Memory-bound at 100M (1.6 GB working set) |
| `sequence_match` | 10.5 ns | Memory-bound (NFA + event traversal) |
| `sequence_count` | 14.5 ns | Memory-bound (NFA restart overhead) |

## Algorithmic Complexity

| Function | Update | Combine | Finalize | Space |
|---|---|---|---|---|
| `sessionize` | O(1) | O(1) | O(1) | O(1) |
| `retention` | O(k) | O(1) | O(k) | O(1) |
| `window_funnel` | O(1) | O(m) | O(n*k) | O(n) |
| `sequence_match` | O(1) | O(m) | O(n*s) | O(n) |
| `sequence_count` | O(1) | O(m) | O(n*s) | O(n) |
| `sequence_next_node` | O(1) | O(m) | O(n*k) | O(n) |

Where n = events, m = events in other state, k = conditions (up to 32),
s = pattern steps.

## Optimization History

Eleven engineering sessions have produced the current performance profile. Each
optimization below was measured independently with before/after Criterion data
and non-overlapping confidence intervals (except where noted as negative results).

### Session 1: Event Bitmask

Replaced `Vec<bool>` per event with a `u32` bitmask, eliminating per-event heap
allocation and enabling `Copy` semantics. The `Event` struct shrank from 32+
bytes with heap allocation to 16 bytes with zero heap allocation.

| Function | Scale | Before | After | Speedup |
|---|---|---|---|---|
| `window_funnel` | 1,000 events | 18.6 us | 1.65 us | 11.3x |
| `window_funnel` | 100,000 events | 2.35 ms | 188 us | 12.5x |
| `sequence_match` | 10,000 events | 260 us | 28.0 us | 9.3x |

### Session 2: In-Place Combine

Replaced O(N^2) merge-allocate combine with O(N) in-place extend. Switched from
stable sort (TimSort) to unstable sort (pdqsort). Eliminated redundant pattern
cloning.

| Function | Scale | Before | After | Speedup |
|---|---|---|---|---|
| `window_funnel_combine` | 100 states | 10.9 us | 466 ns | 23x |
| `window_funnel_combine` | 1,000 states | 781 us | 3.03 us | 258x |
| `window_funnel_combine` | 10,000 states | 75.3 ms | 30.9 us | 2,436x |

### Session 3: NFA Lazy Matching

Fixed `.*` exploration order in the NFA executor. The NFA was using greedy
matching (consume event first), causing O(n^2) backtracking. Switching to lazy
matching (advance pattern first) reduced complexity to O(n * pattern_len).

| Function | Scale | Before | After | Speedup |
|---|---|---|---|---|
| `sequence_match` | 100 events | 750 ns | 454 ns | 1.76x |
| `sequence_match` | 1,000,000 events | 4.43 s | 2.31 ms | 1,961x |

### Session 4: Billion-Row Benchmarks

Expanded benchmarks to 100M for event-collecting functions and 1B for O(1)-state
functions. Validated that all functions scale as documented with no hidden
bottlenecks at extreme scale.

### Session 6: Presorted Detection

Added an O(n) `is_sorted` check before sorting. When events arrive in timestamp
order (the common case for `ORDER BY` queries), the O(n log n) sort is skipped
entirely. The O(n) overhead for unsorted input is negligible.

### Session 7: Negative Results

Three optimization hypotheses were tested and all produced negative results.
These are documented to prevent redundant investigation in future sessions:

- **LSD Radix Sort**: 8-bit radix, 8 passes. Measured 4.3x slower at 100M
  elements. The scatter pattern has poor cache locality for 16-byte structs.
  pdqsort's in-place partitioning is superior for embedded-key structs.

- **Branchless Sessionize**: Replaced conditional branches with `i64::from(bool)`
  and `max`/`min`. Measured 5-10% slower. The branch predictor handles the
  90/10 gap-exceeds-threshold pattern with near-perfect accuracy. CMOV adds
  fixed overhead that exceeds the near-zero cost of correctly predicted branches.

- **Retention Update Micro-optimization**: Analysis showed the benchmark
  bottleneck is `Vec<bool>` allocation in the test harness, not the bitmask
  update loop. No optimization possible without changing the benchmark structure.

### Session 8: sequenceNextNode

Implemented `sequence_next_node` with full direction/base support. No performance
optimization targets — this session focused on feature completeness. Added
dedicated benchmarks for the new function.

### Session 9: Rc\<str\> Optimization

Replaced `Option<String>` with `Option<Rc<str>>` in `NextNodeEvent`, reducing
struct size from 40 to 32 bytes and enabling O(1) clone via reference counting.

| Function | Scale | Before | After | Speedup |
|---|---|---|---|---|
| `sequence_next_node` | 10K events | 2.1x-5.8x across scales | | |
| `sequence_next_node` combine | all scales | 2.8x-6.4x | | |

A string pool attempt (`PooledEvent` with `Copy` semantics) was measured
10-55% slower at most scales and was reverted.

### Session 10: E2E Validation + Custom C Entry Point

Discovered and fixed 3 critical bugs: SEGFAULT on extension load, 6 of 7
functions silently failing to register, and `window_funnel` returning wrong
results due to combine not propagating configuration fields. No performance
optimization targets.

### Session 11: NFA Fast Paths

Pattern classification dispatches common shapes to specialized O(n) scans,
eliminating NFA overhead for the most common patterns.

| Function | Scale | Improvement |
|---|---|---|
| `sequence_count` | 100-1M events | 33-47% (NFA reusable stack) |
| `sequence_count` | 100-1M events | 39-40% additional (fast-path linear scan) |
| `sequence_count` | 100-1M events | 56-61% combined improvement |

A first-condition pre-check was tested and produced negative results (reverted).

## Benchmark Inventory

| Benchmark | Function | Input Sizes |
|---|---|---|
| `sessionize_update` | `sessionize` | 100 to 1 billion |
| `sessionize_combine` | `sessionize` | 100 to 1 million |
| `retention_update` | `retention` | 4, 8, 16, 32 conditions |
| `retention_combine` | `retention` | 100 to 1 billion |
| `window_funnel_finalize` | `window_funnel` | 100 to 100 million |
| `window_funnel_combine` | `window_funnel` | 100 to 1 million |
| `sequence_match` | `sequence_match` | 100 to 100 million |
| `sequence_count` | `sequence_count` | 100 to 100 million |
| `sequence_combine` | `sequence_*` | 100 to 1 million |
| `sort_events` | (isolated) | 100 to 100 million |
| `sort_events_presorted` | (isolated) | 100 to 100 million |
| `sequence_next_node` | `sequence_next_node` | 100 to 10 million |
| `sequence_next_node_combine` | `sequence_next_node` | 100 to 1 million |
| `sequence_next_node_realistic` | `sequence_next_node` | 1,000 to 1 million |

## Reproduction

```bash
# Run all benchmarks
cargo bench

# Run a specific group
cargo bench -- sessionize
cargo bench -- window_funnel

# Results stored in target/criterion/ for comparison
```

Criterion automatically compares against previous runs and reports confidence
intervals. Run benchmarks three or more times before drawing conclusions from
the data.

## Methodology

- **Framework**: Criterion.rs 0.5, 100 samples per benchmark, 95% CI
- **Large-scale benchmarks**: 10 samples with 60-second measurement time for
  100M and 1B element counts
- **Validation**: Every claim requires three consecutive runs with non-overlapping
  confidence intervals
- **Negative results**: Documented with the same rigor as positive results,
  including measured data and root cause analysis

Full optimization history with raw Criterion data, confidence intervals, and
session-by-session analysis is maintained in
[`PERF.md`](https://github.com/tomtom215/duckdb-behavioral/blob/main/PERF.md)
in the repository.
