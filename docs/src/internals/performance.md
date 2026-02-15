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
| `sessionize` | 1 billion | 1.20 s | 830 Melem/s |
| `retention` (combine) | 100 million | 274 ms | 365 Melem/s |
| `window_funnel` | 100 million | 791 ms | 126 Melem/s |
| `sequence_match` | 100 million | 1.05 s | 95 Melem/s |
| `sequence_count` | 100 million | 1.18 s | 85 Melem/s |
| `sequence_match_events` | 100 million | 1.07 s | 93 Melem/s |
| `sequence_next_node` | 10 million | 546 ms | 18 Melem/s |

### Per-Element Cost

| Function | Cost per Element | Bound |
|---|---|---|
| `sessionize` | 1.2 ns | CPU-bound (register-only loop) |
| `retention` | 2.7 ns | CPU-bound (bitmask OR) |
| `window_funnel` | 7.9 ns | Memory-bound at 100M (1.6 GB working set) |
| `sequence_match` | 10.5 ns | Memory-bound (NFA + event traversal) |
| `sequence_count` | 11.8 ns | Memory-bound (NFA restart overhead) |

## Algorithmic Complexity

| Function | Update | Combine | Finalize | Space |
|---|---|---|---|---|
| `sessionize` | O(1) | O(1) | O(1) | O(1) |
| `retention` | O(k) | O(1) | O(k) | O(1) |
| `window_funnel` | O(1) | O(m) | O(n*k) | O(n) |
| `sequence_match` | O(1) | O(m) | O(n*s) | O(n) |
| `sequence_count` | O(1) | O(m) | O(n*s) | O(n) |
| `sequence_match_events` | O(1) | O(m) | O(n*s) | O(n) |
| `sequence_next_node` | O(1) | O(m) | O(n*k) | O(n) |

Where n = events, m = events in other state, k = conditions (up to 32),
s = pattern steps.

## Optimization History

Fifteen engineering sessions have produced the current performance profile. Each
optimization below was measured independently with before/after Criterion data
and non-overlapping confidence intervals (except where noted as negative results).

### Event Bitmask Optimization

Replaced `Vec<bool>` per event with a `u32` bitmask, eliminating per-event heap
allocation and enabling `Copy` semantics. The `Event` struct shrank from 32+
bytes with heap allocation to 16 bytes with zero heap allocation.

| Function | Scale | Before | After | Speedup |
|---|---|---|---|---|
| `window_funnel` | 1,000 events | 18.6 us | 1.65 us | 11.3x |
| `window_funnel` | 100,000 events | 2.35 ms | 188 us | 12.5x |
| `sequence_match` | 10,000 events | 260 us | 28.0 us | 9.3x |

### In-Place Combine

Replaced O(N^2) merge-allocate combine with O(N) in-place extend. Switched from
stable sort (TimSort) to unstable sort (pdqsort). Eliminated redundant pattern
cloning.

| Function | Scale | Before | After | Speedup |
|---|---|---|---|---|
| `window_funnel_combine` | 100 states | 10.9 us | 466 ns | 23x |
| `window_funnel_combine` | 1,000 states | 781 us | 3.03 us | 258x |
| `window_funnel_combine` | 10,000 states | 75.3 ms | 30.9 us | 2,436x |

### NFA Lazy Matching

Fixed `.*` exploration order in the NFA executor. The NFA was using greedy
matching (consume event first), causing O(n^2) backtracking. Switching to lazy
matching (advance pattern first) reduced complexity to O(n * pattern_len).

| Function | Scale | Before | After | Speedup |
|---|---|---|---|---|
| `sequence_match` | 100 events | 750 ns | 454 ns | 1.76x |
| `sequence_match` | 1,000,000 events | 4.43 s | 2.31 ms | 1,961x |

### Billion-Row Benchmarks

Expanded benchmarks to 100M for event-collecting functions and 1B for O(1)-state
functions. Validated that all functions scale as documented with no hidden
bottlenecks at extreme scale.

| Function | Scale | Wall Clock | Throughput |
|---|---|---|---|
| `sessionize_update` | 1 billion | 1.21 s | 824 Melem/s |
| `retention_combine` | 1 billion | 3.06 s | 327 Melem/s |
| `window_funnel` | 100 million | 898 ms | 111 Melem/s |
| `sequence_match` | 100 million | 1.08 s | 92.7 Melem/s |
| `sequence_count` | 100 million | 1.57 s | 63.5 Melem/s |

### ClickHouse Feature Parity

Architectural refactoring session. Implemented combinable `FunnelMode` bitflags,
three new `window_funnel` modes (`strict_increase`, `strict_once`,
`allow_reentry`), multi-step advancement per event, and the
`sequence_match_events` function. No performance optimization targets.

### Presorted Detection + 32-Condition Support

Added an O(n) `is_sorted` check before sorting. When events arrive in timestamp
order (the common case for `ORDER BY` queries), the O(n log n) sort is skipped
entirely. The O(n) overhead for unsorted input is negligible.

Also expanded condition support from `u8` (8 conditions) to `u32` (32
conditions). This was a zero-cost change: the `Event` struct remains 16 bytes
due to alignment padding from the `i64` field.

### Negative Results

Three optimization hypotheses were tested and all produced negative results.
These are documented to prevent redundant investigation:

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

### sequence_next_node Implementation

Implemented `sequence_next_node` with full direction (forward/backward) and
base (head/tail/first_match/last_match) support. Uses a dedicated
`NextNodeEvent` struct with per-event string storage (separate from the
`Copy` `Event` struct). Simple sequential matching algorithm.

Added a dedicated benchmark (`benches/sequence_next_node_bench.rs`) measuring
update + finalize throughput and combine operations at multiple scales. No
performance optimization targets — this session focused on feature
completeness, achieving complete ClickHouse behavioral analytics parity.

### Arc\<str\> Optimization

Replaced `Option<String>` with `Option<Arc<str>>` in `NextNodeEvent`, reducing
struct size from 40 to 32 bytes and enabling O(1) clone via reference counting.

| Function | Scale | Improvement |
|---|---|---|
| `sequence_next_node` update+finalize | all scales | 2.1x-5.8x |
| `sequence_next_node` combine | all scales | 2.8x-6.4x |

A string pool attempt (`PooledEvent` with `Copy` semantics, 24 bytes) was
measured 10-55% slower at most scales due to dual-vector overhead. The key
insight: below one cache line (64 bytes), clone cost matters more than absolute
struct size. `Arc::clone` (~1ns atomic increment) consistently beats
`String::clone` (copies len bytes).

Also added a realistic cardinality benchmark using a pool of 100 distinct
`Arc<str>` values, showing 35% improvement over unique strings at 1M events.

### E2E Validation + Custom C Entry Point

Discovered and fixed 3 critical bugs that all 375 passing unit tests at the time (now 434) missed:

1. **SEGFAULT on extension load**: `extract_raw_connection` used incorrect
   pointer arithmetic. Replaced with a custom C entry point
   (`behavioral_init_c_api`) that obtains `duckdb_connection` via
   `duckdb_connect` directly from `duckdb_extension_access`.

2. **Silent function registration failure**: `duckdb_aggregate_function_set_name`
   must be called on each function in a set. Without this, 6 of 7 functions
   silently failed to register.

3. **Incorrect combine propagation**: `combine_in_place` did not propagate
   `window_size_us` or `mode`. DuckDB's segment tree creates fresh
   zero-initialized target states, so these fields remained zero at finalize
   time.

No performance optimization targets. 11 E2E tests added against real DuckDB
v1.4.4 CLI.

### NFA Fast Paths

Two Criterion-validated optimizations for the NFA pattern executor:

1. **NFA reusable stack**: Pre-allocate the NFA state Vec once and reuse
   across all starting positions via `clear()`. Eliminates O(N) alloc/free
   pairs in `sequence_count`.

2. **Fast-path linear scan**: Pattern classification dispatches common shapes
   to specialized O(n) scans:
   - Adjacent conditions (`(?1)(?2)`): sliding window scan
   - Wildcard-separated (`(?1).*(?2)`): single-pass step counter
   - Complex patterns: fall back to NFA

| Function | Scale | Improvement |
|---|---|---|
| `sequence_count` | 100-1M events | 33-47% (NFA reusable stack) |
| `sequence_count` | 100-1M events | 39-40% additional (fast-path linear scan) |
| `sequence_count` | 100-1M events | 56-61% combined improvement |

A first-condition pre-check was tested and produced negative results (0-8%
regression). The NFA already handles non-matching starts efficiently (~5ns per
check), so the extra branch in the hot loop costs more than it saves. Reverted.

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
| `sequence_match_events` | `sequence_match_events` | 100 to 100 million |
| `sequence_match_events_combine` | `sequence_match_events` | 100 to 1 million |
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
cargo bench -- sequence_match_events

# Results stored in target/criterion/ for comparison
```

Criterion automatically compares against previous runs and reports confidence
intervals. Run benchmarks three or more times before drawing conclusions from
the data.

## Methodology

- **Framework**: Criterion.rs 0.8, 100 samples per benchmark, 95% CI
- **Large-scale benchmarks**: 10 samples with 60-second measurement time for
  100M and 1B element counts
- **Validation**: Every claim requires three consecutive runs with non-overlapping
  confidence intervals
- **Negative results**: Documented with the same rigor as positive results,
  including measured data and root cause analysis

Full optimization history with raw Criterion data, confidence intervals, and
detailed analysis is maintained in
[`PERF.md`](https://github.com/tomtom215/duckdb-behavioral/blob/main/PERF.md)
in the repository.
