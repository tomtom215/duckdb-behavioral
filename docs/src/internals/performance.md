# Performance

All performance claims in this project are backed by measured data from
[Criterion.rs](https://bheisler.github.io/criterion.rs/book/) benchmarks with
95% confidence intervals, validated across three or more consecutive runs.
Improvements are accepted only when confidence intervals do not overlap.

The Criterion benchmarks are microbenchmarks of the Rust aggregate state
(update, combine, finalize), not end-to-end SQL queries through DuckDB.
`scripts/bench-sql.sh` times the SQL queries (10 million rows, 100,000 groups,
4 threads); its numbers are in the [SQL timings](#sql-timings) below.

## Current Baseline

Measured in PERF.md Session 21 on one shared 4-vCPU Intel Xeon @ 2.80 GHz,
rustc 1.97.0, `bench` profile, Criterion 0.8.2 (95% CI of the mean). Absolute
numbers vary by hardware; compare only with numbers taken on the same machine
in the same session. The Session 15 table that preceded these came from another
machine and, for `window_funnel`, from the engine replaced in 0.10.0.

### Headline Numbers

| Benchmark | Scale | Time [95% CI] | Throughput |
|---|---|---|---|
| `sessionize_update` | 1 billion | 1.68 s [1.66, 1.70] | 595 Melem/s |
| `retention_combine` | 100 million | 251 ms [246, 256] | 398 Melem/s |
| `window_funnel_finalize` (8 conditions) | 100 million | 2.12 s [1.93, 2.27] | 47 Melem/s |
| `window_funnel_events_finalize` | 100 million | 1.49 s [1.31, 1.73] | 67 Melem/s |
| `sequence_match` | 100 million | 1.49 s [1.26, 1.70] | 67 Melem/s |
| `sequence_count` | 100 million | 1.50 s [1.28, 1.72] | 67 Melem/s |
| `sequence_match_events` | 100 million | 1.30 s [1.18, 1.52] | 77 Melem/s |
| `sequence_next_node` | 10 million | 587 ms (median of 3 runs, 569 to 593) | 17 Melem/s |

The 100-million rows have 10 samples each, so their intervals are wide.

### Per-Element Cost

| Function | Cost per Element |
|---|---|
| `sessionize` | 1.7 ns |
| `retention` | 2.5 ns |
| `window_funnel` | 21.2 ns |
| `window_funnel_events` | 14.9 ns |
| `sequence_match` | 14.9 ns |
| `sequence_count` | 15.0 ns |
| `sequence_match_events` | 13.0 ns |
| `sequence_next_node` | 58.7 ns |

### SQL timings

`scripts/bench-sql.sh`: 10 million rows in 100,000 groups of 100 events,
4 threads, wall time of a fresh `duckdb` 2.0.0 process, median [min, max] of 9
runs (25 for `sessionize`). These include DuckDB's scan and grouping. Measured
in PERF.md Session 22 on one shared 4-vCPU Intel Xeon @ 2.10 GHz, a different
machine from the Criterion numbers above, so the two sets do not compare. The
base column is the build from before this release's changes, run alternately
with the new one.

| Query | Base | New |
|---|---|---|
| `window_funnel`, 3 steps | 1.16 [1.04, 1.39] | 1.07 [0.99, 1.41] |
| `window_funnel_events` | 0.98 [0.97, 1.04] | 1.02 [0.96, 1.09] |
| `sequence_count('(?1).*(?2).*(?3)')` | 1.17 [1.08, 1.31] | 1.20 [1.06, 1.47] |
| `sequence_match_events('(?1).*(?2)')` | 1.16 [1.02, 1.21] | 1.10 [1.06, 1.21] |
| `sequence_next_node` | 2.52 [2.34, 2.94] | 2.60 [2.43, 2.93] |
| `retention`, 3 conditions | 0.27 [0.24, 0.31] | 0.26 [0.24, 0.29] |
| `sessionize` over `PARTITION BY u ORDER BY ts` | 1.39 [1.30, 1.53] | 1.42 [1.31, 1.58] |
| `window_funnel` over one 10-million-row group | 0.41 [0.38, 0.46] | 0.41 [0.39, 0.48] |

## Algorithmic Complexity

| Function | Update | Combine | Finalize | Space |
|---|---|---|---|---|
| `sessionize` | O(1) | O(1) | O(1) | O(1) |
| `retention` | O(k) | O(1) | O(k) | O(1) |
| `window_funnel` | O(1) | O(m) | O(n*k) | O(n) |
| `sequence_match` | O(1) | O(m) | O(n*s) | O(n) |
| `sequence_count` | O(1) | O(m) | O(n*s) | O(n) |
| `sequence_match_events` | O(1) | O(m) | O(n*s) | O(n) |
| `sequence_next_node` | O(1) | O(m) | O(n log n) | O(n) |

Where n = events, m = events in other state, k = conditions (up to 32),
s = pattern steps. Finalize of every event-collecting function starts by
sorting its events: O(n log n), or O(n) when they already arrive in order,
which DuckDB often provides. For `sequence_next_node` the sort dominates: the
anchor scan is O(n) and the chain check O(k).

## Optimization History

`PERF.md` records the optimization sessions (numbered from Session 1 to
Session 21) that produced the current performance profile. Each
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
| `sequence_match` | 100 events | 750 ns | 427 ns | 1.76x |
| `sequence_match` | 1,000,000 events | 4.43 s | 2.23 ms | 1,961x |

### Billion-Row Benchmarks

Expanded benchmarks to 100M for event-collecting functions and 1B for O(1)-state
functions (PERF.md Session 4; the `retention_combine` benchmark has since been
capped at 100M, so only `sessionize_update` still runs at 1B). Validated that all functions scale as documented with no hidden
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

Added an O(n) `is_sorted` check before sorting. When events already arrive in
timestamp order, the O(n log n) sort is skipped entirely. The O(n) overhead for unsorted input is negligible.

Also expanded condition support from `u8` (8 conditions) to `u32` (32
conditions). This was a zero-cost change: the `Event` struct remains 16 bytes
due to alignment padding from the `i64` field.

### Negative Results

Session 7 measured two optimization hypotheses that produced negative results
and ruled out a third without implementing it. These are documented to prevent
redundant investigation:

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
| `sequence_next_node` update+finalize | all scales | 1.8x-5.8x |
| `sequence_next_node` combine | all scales | 2.8x-6.4x |

A string pool attempt (`PooledEvent` with `Copy` semantics, 24 bytes) was
measured 10-55% slower at most scales due to dual-vector overhead. The key
insight: below one cache line (64 bytes), clone cost matters more than absolute
struct size. `Arc::clone` (~1ns atomic increment) consistently beats
`String::clone` (copies len bytes).

Also added a realistic cardinality benchmark using a pool of 100 distinct
`Arc<str>` values, showing 35% improvement over unique strings at 1M events.

> Superseded in 0.10.1: `sequence_next_node` now stores a state's values in
> one byte buffer and each event is a 24-byte record with a range into it. See
> the 0.10.1 entry at the end of this page.

### E2E Validation + Custom C Entry Point

Discovered and fixed 3 critical bugs that all 375 passing unit tests at the time (now 544) missed:

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
| `sequence_count` | 1K-1M events | 56-60% combined improvement |

A first-condition pre-check was tested and produced negative results (0-8%
regression). The NFA already handles non-matching starts efficiently (~5ns per
check), so the extra branch in the hot loop costs more than it saves. Reverted.

## Benchmark Inventory

| Benchmark | Function | Input Sizes |
|---|---|---|
| `sessionize_update` | `sessionize` | 100 to 1 billion |
| `sessionize_combine` | `sessionize` | 100 to 100 million |
| `retention_update` | `retention` | 4, 8, 16, 32 conditions |
| `retention_combine` | `retention` | 100 to 100 million |
| `window_funnel_finalize` | `window_funnel` | 100 to 100 million |
| `window_funnel_combine` | `window_funnel` | 100 to 1 million |
| `window_funnel_events_finalize` | `window_funnel_events` | 10,000 to 100 million |
| `sequence_match` | `sequence_match` | 100 to 100 million |
| `sequence_count` | `sequence_count` | 100 to 100 million |
| `sequence_combine` | `sequence_*` | 100 to 1 million |
| `sequence_match_time_constraint` | `sequence_match` | 100,000 and 1 million |
| `sequence_match_events` | `sequence_match_events` | 100 to 100 million |
| `sequence_match_events_combine` | `sequence_match_events` | 100 to 1 million |
| `sort_events` | (isolated; reverse-ordered timestamps with jitter) | 100 to 100 million |
| `sort_events_presorted` | (isolated) | 100 to 100 million |
| `sequence_next_node` | `sequence_next_node` | 100 to 10 million |
| `sequence_next_node_combine` | `sequence_next_node` | 100 to 1 million |
| `sequence_next_node_realistic` | `sequence_next_node` | 100 to 10 million |

### Session 21 (unreleased): safe FFI views, edition 2024, byte-buffer store

Base (v0.10.1, commit 63e5a64) and the branch were built and run alternately on
the same machine; the full tables are in `PERF.md`.

- Eight Criterion state benchmarks at 10 million elements, three alternating
  passes: no difference beyond the spread of the passes (ratios 0.96 to 1.06).
  The 1.06 is `retention_combine`: ranges did not overlap in those passes, but
  five more alternating passes gave +2.5% with overlapping ranges, and its
  production code is unchanged.
- Eight SQL queries at 10 million rows: no difference beyond the spread of 9
  runs, except `sequence_next_node`: 6.85 s to 3.33 s (0.49) on the reproducible
  data, 0.58 on a second data set in both wall and CPU time.
- `sequence_next_node` stores values in one byte buffer per state, with a
  24-byte record per event, instead of an `Arc<str>` per event. The per-row
  ingest cost in Criterion (allocating an `Arc` from a borrowed string and
  `update`, against `try_push`) fell to 0.48 at 10 million unique values and
  0.42 with 100 distinct values.

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
the data. For SQL timings:

```bash
scripts/bench-sql.sh "$(command -v duckdb)" build/release/behavioral.duckdb_extension
```

## Methodology

- **Framework**: Criterion.rs 0.8, 100 samples per benchmark, 95% CI
- **Large-scale benchmarks**: reduced sample counts and longer measurement
  times for 100M and 1B element counts
- **Validation**: Every claim requires three consecutive runs with non-overlapping
  confidence intervals
- **Negative results**: Documented with the same rigor as positive results,
  including measured data and root cause analysis

Full optimization history with raw Criterion data, confidence intervals, and
detailed analysis is maintained in
[`PERF.md`](https://github.com/tomtom215/duckdb-behavioral/blob/main/PERF.md)
in the repository.
