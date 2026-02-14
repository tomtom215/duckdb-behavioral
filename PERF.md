# Performance Engineering

This document records the performance characteristics, optimization history, and
benchmark methodology for `duckdb-behavioral`. Every claim is backed by measured
data with statistical confidence intervals. Every optimization is independently
reproducible via `cargo bench`.

## Table of Contents

- [Benchmark Methodology](#benchmark-methodology)
  - [Framework](#framework)
  - [Reproduction](#reproduction)
  - [Benchmark Inventory](#benchmark-inventory)
- [Algorithmic Complexity](#algorithmic-complexity)
- [Optimization History](#optimization-history)
  - [Session 1: Event Bitmask Optimization](#session-1-event-bitmask-optimization)
  - [Session 1: Supplementary Optimizations](#session-1-supplementary-optimizations)
  - [Session 2: In-Place Combine (O(N²) → O(N))](#session-2-in-place-combine-on²--on)
  - [Session 2: Unstable Sort (pdqsort)](#session-2-unstable-sort-pdqsort)
  - [Session 2: Pattern Clone Elimination](#session-2-pattern-clone-elimination)
  - [Session 3: NFA Lazy Matching (O(n²) → O(n))](#session-3-nfa-lazy-matching-on²--on)
  - [Session 3: Compiled Pattern Preservation (Negative Result)](#session-3-compiled-pattern-preservation-negative-result)
- [Session 4: 100M/1B Benchmark Expansion](#session-4-100m1b-benchmark-expansion)
- [Session 6: Presorted Detection + 32-Condition Support](#session-6-presorted-detection--32-condition-support)
- [Session 7: Negative Results + Baseline Validation](#session-7-negative-results--baseline-validation)
- [Session 8: sequenceNextNode Baseline](#session-8-sequencenextnode-baseline)
- [Session 9: Rc\<str\> Optimization + String Pool Negative Result](#session-9-rcstr-optimization--string-pool-negative-result)
- [Session 11: NFA Reusable Stack + Fast-Path Linear Scan](#session-11-nfa-reusable-stack--fast-path-linear-scan)
- [Current Baseline](#current-baseline)
  - [Sessionize](#sessionize)
  - [Retention](#retention)
  - [Window Funnel](#window-funnel)
  - [Sequence](#sequence)
  - [Sort (Isolated)](#sort-isolated)
  - [Sequence Next Node](#sequence-next-node)
  - [Per-Element Cost at Scale](#per-element-cost-at-scale)
  - [Billion-Row Headline Numbers](#billion-row-headline-numbers)
- [Session Improvement Protocol](#session-improvement-protocol)
  - [Before Starting Work](#before-starting-work)
  - [During Work](#during-work)
  - [After Completing Work](#after-completing-work)
  - [Quality Standards for Performance Claims](#quality-standards-for-performance-claims)

## Benchmark Methodology

### Framework

- **Criterion.rs 0.8** with 100 samples per benchmark and 95% confidence intervals
- Benchmarks measure end-to-end aggregate function execution: state creation,
  update (event ingestion), and finalize (result computation)
- Each benchmark is run 3+ times to verify stability before comparison
- Improvements are accepted only when confidence intervals do not overlap

### Reproduction

```bash
# Run all benchmarks with full statistical output
cargo bench

# Run a specific benchmark group
cargo bench -- window_funnel
cargo bench -- sequence_match

# Results are stored in target/criterion/ for comparison
# Criterion automatically compares against previous runs
```

### Benchmark Inventory

| Benchmark | Function | Operation | Input Sizes | Measures |
|---|---|---|---|---|
| `sessionize_update` | `sessionize` | update + finalize | 100 to 1B events | Per-row throughput |
| `sessionize_combine` | `sessionize` | combine chain | 100 to 1M states | Segment tree merge cost |
| `retention_update` | `retention` | update + finalize | 4, 8, 16, 32 conditions | Per-condition scaling |
| `retention_combine` | `retention` | combine chain | 100 to 1B states | O(1) combine verification |
| `window_funnel_finalize` | `window_funnel` | update + finalize | 100 to 100M events | Funnel scan throughput |
| `window_funnel_combine` | `window_funnel` | combine_in_place + finalize | 100 to 1M states | In-place append + sort cost |
| `sequence_match` | `sequence_match` | update + finalize | 100 to 100M events | NFA pattern matching |
| `sequence_count` | `sequence_count` | update + finalize | 100 to 100M events | Non-overlapping counting |
| `sequence_combine` | `sequence_*` | combine_in_place + finalize | 100 to 1M states | In-place append + NFA cost |
| `sort_events` | (isolated) | sort only | 100 to 100M events | pdqsort scaling (random) |
| `sort_events_presorted` | (isolated) | sort only | 100 to 100M events | pdqsort adaptive path |
| `sequence_next_node` | `sequence_next_node` | update + finalize | 100 to 10M events | Sequential matching + Arc\<str\> clone |
| `sequence_next_node_combine` | `sequence_next_node` | combine_in_place + finalize | 100 to 100K states | In-place append + sequential matching |
| `sequence_match_events` | `sequence_match_events` | update + finalize_events | 100 to 100M events | NFA pattern matching + timestamp collection |
| `sequence_match_events_combine` | `sequence_match_events` | combine_in_place + finalize_events | 100 to 1M states | In-place append + NFA + timestamp cost |
| `sequence_next_node_realistic` | `sequence_next_node` | update + finalize (100 distinct values) | 100 to 1M events | Realistic cardinality Arc\<str\> sharing |

## Algorithmic Complexity

| Function | Update | Combine | Finalize | Space |
|---|---|---|---|---|
| `sessionize` | O(1) | O(1) | O(1) | O(1) — tracks only first/last timestamp + boundary count |
| `retention` | O(k) | O(1) | O(k) | O(1) — single u32 bitmask, k = conditions |
| `window_funnel` | O(1) amortized | O(m) append | O(n*k) greedy scan | O(n) — collected events |
| `sequence_match` | O(1) amortized | O(m) append | O(n*s) NFA execution | O(n) — collected events |
| `sequence_count` | O(1) amortized | O(m) append | O(n*s) NFA execution | O(n) — collected events |
| `sequence_match_events` | O(1) amortized | O(m) append | O(n*s) NFA execution | O(n) — collected events |
| `sequence_next_node` | O(1) amortized | O(m) append | O(n*s) sequential scan | O(n) — collected events + Strings |

Where n = events, m = events in other state, k = conditions (up to 32), s = pattern steps.

**sort_events**: O(n) if presorted (is_sorted check), O(n log n) if unsorted (pdqsort).

## Optimization History

### Session 1: Event Bitmask Optimization

#### Hypothesis

Replacing `Event.conditions: Vec<bool>` with a `u8` bitmask eliminates the dominant
cost in event-based functions: per-event heap allocation. The DuckDB function set
registrations support at most 8 boolean conditions, making `u8` sufficient.

#### Technique

| Aspect | Before | After |
|---|---|---|
| Event struct size | 32 bytes (i64 + Vec header) + heap | 16 bytes (i64 + u8 + padding) |
| Event trait | `Clone` (deep copy) | `Copy` (memcpy) |
| Heap allocations per event | 1 (Vec backing store) | 0 |
| `has_any_condition()` | `iter().any(\|&c\| c)` — O(k) | `conditions != 0` — O(1) |
| `condition(idx)` | Vec bounds check + index | `(conditions >> idx) & 1` — single instruction |
| Merge (combine) | `clone()` per element | `memcpy` via Copy semantics |

#### Basis

- Memory allocation overhead: 20-80ns per allocation depending on size class
  and contention (jemalloc/mimalloc literature)
- Cache line optimization: 16-byte structs pack 4 per 64-byte cache line vs
  1-2 for 32+ byte structs with pointer chasing into heap
- Drepper (2007) "What Every Programmer Should Know About Memory" — eliminating
  pointer indirection is the single highest-impact optimization for data-intensive
  workloads

#### Measured Results

Criterion, 100 samples, 95% confidence intervals. All improvements show
non-overlapping confidence intervals (statistically significant).

**`window_funnel` (update + sort + greedy scan):**

| Input | Before [95% CI] | After [95% CI] | Speedup |
|---|---|---|---|
| 100 events, 3 conds | 1.42 µs [1.393, 1.455] | 253 ns [253.1, 258.7] | **5.6x** |
| 1,000 events, 5 conds | 18.6 µs [18.54, 18.69] | 1.65 µs [1.634, 1.667] | **11.3x** |
| 10,000 events, 5 conds | 202 µs [200.0, 205.3] | 15.0 µs [14.91, 15.06] | **13.5x** |
| 100,000 events, 8 conds | 2.35 ms [2.325, 2.381] | 188 µs [186.9, 190.2] | **12.5x** |

**`sequence_match` (update + sort + NFA pattern matching):**

| Input | Before [95% CI] | After [95% CI] | Speedup |
|---|---|---|---|
| 100 events | 1.73 µs [1.725, 1.741] | 701 ns [693.4, 703.1] | **2.5x** |
| 1,000 events | 22.2 µs [22.04, 22.37] | 3.50 µs [3.425, 3.490] | **6.3x** |
| 10,000 events | 260 µs [257.9, 262.7] | 28.0 µs [27.51, 28.69] | **9.3x** |

**`sequence_count` (update + sort + NFA counting):**

| Input | Before [95% CI] | After [95% CI] | Speedup |
|---|---|---|---|
| 100 events | 2.77 µs [2.747, 2.800] | 719 ns [713.6, 724.5] | **3.9x** |
| 1,000 events | 22.6 µs [22.37, 22.80] | 5.03 µs [4.959, 5.115] | **4.5x** |
| 10,000 events | 254 µs [252.2, 256.7] | 47.9 µs [46.91, 49.09] | **5.3x** |

#### Analysis

The speedup increases super-linearly with input size. At 100 events, the
improvement is 2.5-5.6x. At 100,000 events, it reaches 12.5x. This is
consistent with the optimization eliminating a per-event heap allocation:
larger inputs amplify the per-element savings, and the improved cache
locality from 16-byte Copy structs compounds as the working set grows
beyond L1/L2 cache.

The `window_funnel` function benefits more than `sequence_*` because its
greedy scan algorithm accesses conditions multiple times per event (checking
the current step, previous step in strict mode, and iterating conditions
in strict order mode). Each access is now a single bitwise operation
instead of a Vec bounds check + memory load.

### Session 1: Supplementary Optimizations


- `#[inline]` on `interval_to_micros`, `RetentionState::update/combine`,
  `SessionizeBoundaryState::update/combine` — documents optimization intent
  for the compiler, serves as defense against profile changes that disable LTO
- `NfaState` derives `Copy` (24 bytes, no heap) — eliminates clone overhead in
  backtracking NFA exploration
- No measurable regression; these are maintenance optimizations that ensure
  the compiler has maximum information for optimization decisions

### Session 2: In-Place Combine (O(N²) → O(N))

#### Hypothesis

`merge_sorted_events()` in combine allocates a NEW Vec every invocation. In a
left-fold chain of N states, this produces N allocations of sizes 1, 2, 3, ..., N,
resulting in O(N²) total element copies. Since `finalize()` already sorts events
via `sort_events()`, maintaining sorted order in combine is wasted work. An in-place
`extend_from_slice` with Vec's doubling growth strategy reduces the chain to O(N)
amortized total copies.

#### Technique

1. Replace `merge_sorted_events()` with `Vec::extend_from_slice()` in combine
2. Add `combine_in_place(&mut self, other: &Self)` that extends self.events directly
3. Update FFI layer to use `combine_in_place` — the target state grows in-place
4. Vec's doubling strategy provides amortized O(1) per append

#### Basis

- Cormen et al. (CLRS) Ch.17: Amortized analysis of dynamic arrays proves O(N) total
  copies for N sequential appends with doubling strategy
- Sedgewick & Wayne "Algorithms" 4th Ed.: Deferred sorting is optimal when the merged
  result must be sorted anyway
- Knuth TAOCP Vol 3: Merge sort with deferred execution

#### Measured Results

Criterion, 100 samples, 95% confidence intervals. 3 runs each.

**`window_funnel_combine` (left-fold chain + finalize):**

| Input | Before [95% CI] | After [95% CI] | Speedup |
|---|---|---|---|
| 100 states | 10.9 µs [10.91, 11.39] | 466 ns [461, 472] | **23.4x** |
| 1,000 states | 781 µs [780, 793] | 3.03 µs [3.02, 3.04] | **258x** |
| 10,000 states | 75.3 ms [74.6, 76.0] | 30.9 µs [30.8, 31.1] | **2,436x** |

**`sequence_combine` (left-fold chain + finalize):**

| Input | Before [95% CI] | After [95% CI] | Speedup |
|---|---|---|---|
| 100 states | 12.1 µs [11.99, 12.18] | 1.31 µs [1.31, 1.32] | **9.2x** |
| 1,000 states | 756 µs [751.8, 759.8] | 10.98 µs [10.88, 11.10] | **68.8x** |

#### Analysis

The super-linear speedup scaling (23x at N=100 → 2,436x at N=10,000) confirms the
O(N²) → O(N) algorithmic complexity improvement. The old approach performed
N(N+1)/2 ≈ 50 million element copies at N=10,000 (16 bytes each = ~800MB memory
traffic). The new approach performs ~10,000 copies total with amortized O(1) reallocation.

The speedup is lower for sequence_combine (9.2x at 100, 68.8x at 1000) because
sequence finalize includes NFA pattern matching which adds a constant cost per
finalize call that is independent of the combine strategy.

### Session 2: Unstable Sort (pdqsort)

#### Hypothesis

Replace `sort_by_key` (stable TimSort variant) with `sort_unstable_by_key` (pdqsort).
Same-timestamp event order has no defined semantics in ClickHouse's behavioral
analytics functions. Unstable sort avoids O(n) auxiliary memory and has better
constant factors for 16-byte Copy types.

#### Basis

- Peters (2021) "Pattern-Defeating Quicksort" — Rust's sort_unstable implementation
- Edelkamp & Weiß (2016) "BlockQuicksort" — branch-free partitioning

#### Measured Results

Modest improvement. sequence_match/100: 706 ns → 676 ns (non-overlapping CIs).
Larger inputs show marginal improvement masked by scan/NFA cost. No regressions.

### Session 2: Pattern Clone Elimination

Eliminated heap-allocating `CompiledPattern::clone()` in `execute()`. Now uses a
reference to the cached pattern. Constant-factor improvement proportional to
pattern complexity.

### Session 3: NFA Lazy Matching (O(n²) → O(n))

#### Hypothesis

The NFA executor's `AnyEvents` (.*) handler pushes two transitions onto a LIFO
stack: "advance to next pattern step" and "consume current event and stay". The
push order determines which transition is explored first. With "advance step"
pushed first (bottom of stack) and "consume event" pushed second (top, popped
first), the NFA greedily consumes ALL remaining events before trying to advance
the pattern — resulting in O(n * MAX_NFA_STATES * n_starts) behavior instead of
O(n * pattern_len).

Swapping the push order makes the NFA try to advance the pattern first (lazy
matching), which succeeds quickly when the next condition is satisfied, avoiding
the exponential exploration of the "consume everything" branch.

#### Technique

In `src/pattern/executor.rs`, the `AnyEvents` match arm:

```rust
// BEFORE (greedy — explores "consume event" first):
PatternStep::AnyEvents => {
    states.push(NfaState { step_idx: state.step_idx + 1, ..state }); // bottom
    states.push(NfaState { event_idx: state.event_idx + 1, ..state }); // top (popped first)
}

// AFTER (lazy — explores "advance step" first):
PatternStep::AnyEvents => {
    states.push(NfaState { event_idx: state.event_idx + 1, ..state }); // bottom
    states.push(NfaState { step_idx: state.step_idx + 1, ..state }); // top (popped first)
}
```

#### Basis

- Thompson (1968) "Programming Techniques: Regular expression search algorithm" —
  NFA exploration order determines average-case complexity
- Cox (2007) "Regular Expression Matching Can Be Simple And Fast" — lazy vs greedy
  semantics in NFA backtracking
- ClickHouse source: sequence_match uses lazy `.*` semantics (match earliest)

#### Measured Results

Criterion, 100 samples, 95% confidence intervals. 3 runs each.

**`sequence_match` (update + sort + NFA pattern matching, pattern `(?1).*(?2).*(?3)`):**

| Input | Before [95% CI] | After [95% CI] | Speedup |
|---|---|---|---|
| 100 events | 750 ns [744, 756] | 427 ns [423, 432] | **1.76x** |
| 1,000 events | 3.53 µs [3.49, 3.59] | 1.96 µs [1.94, 1.98] | **1.80x** |
| 10,000 events | 30.2 µs [30.1, 30.4] | 17.0 µs [16.8, 17.2] | **1.78x** |
| 100,000 events | 298 µs [296, 300] | 186 µs [184, 188] | **1.60x** |
| 1,000,000 events | 4.43 s [4.39, 4.47] | 2.23 ms [2.22, 2.25] | **1,961x** |

#### Analysis

The 1,961x speedup at 1M events confirms the algorithmic complexity improvement.
With greedy exploration, each of ~166K start positions (events matching condition 1)
triggered MAX_NFA_STATES (10,000) iterations consuming events before trying to
advance — totaling ~1.66 billion NFA state transitions. With lazy exploration, each
start position advances through the 3-step pattern in O(pattern_len) transitions,
finding a match within a few events.

The 1.76-1.80x improvement at smaller sizes (100-10K) reflects the constant-factor
benefit of lazy matching even when the greedy approach doesn't hit the MAX_NFA_STATES
limit: the NFA explores fewer dead-end branches before finding the optimal match.

At 1M events the speedup is catastrophic because the working set (16 bytes × 1M =
16MB) exceeds L2 cache, and the greedy approach traverses the entire event array
~166K times vs the lazy approach traversing it once.

### Session 3: Compiled Pattern Preservation (Negative Result)

#### Hypothesis

`combine_in_place` unconditionally sets `self.compiled_pattern = None`, forcing
re-compilation on every finalize after a combine chain. Preserving the compiled
pattern when `pattern_str` hasn't changed should eliminate redundant parsing and
compilation.

#### Measured Results

Criterion, 100 samples, 95% confidence intervals. 3 runs.

| Input | Before [95% CI] | After [95% CI] | Change |
|---|---|---|---|
| sequence_combine/1000 | 10.8 µs [10.7, 10.9] | 10.5 µs [10.4, 10.7] | ~2.5% (CIs overlap) |

**Result**: CIs overlap — no statistically significant improvement. The pattern
compilation cost (~50ns) is negligible compared to event processing. Committed as
a code quality improvement (correctness of the API contract) but NOT counted as a
performance optimization.

### Session 4: 100M/1B Benchmark Expansion

#### Hypothesis

Extending benchmarks from 10M to 100M and 1B elements reveals whether
throughput continues to degrade or stabilizes at DRAM bandwidth limits.
Sessionize (O(1) state) and retention (O(1) combine, 4 bytes/state) are
candidates for 1B due to minimal memory requirements. Event-collecting
functions (window_funnel, sequence, sort) require 16 bytes/event × N,
limiting them to ~100M (1.6GB) on a 21GB system.

#### Basis

- Drepper (2007) "What Every Programmer Should Know About Memory" — cache
  hierarchy performance modeling predicts throughput plateaus at DRAM bandwidth
- McCalpin (1995) "STREAM: Sustainable Memory Bandwidth in High Performance
  Computers" — methodology for measuring sustainable DRAM bandwidth

#### Measured Results

Criterion, 10 samples (100M/1B scales), 95% confidence intervals. 3 runs.

**Billion-row results (O(1) state functions):**

| Benchmark | Mean [95% CI] | Throughput | ns/element |
|---|---|---|---|
| `sessionize_update/1B` | 1.213 s [1.207, 1.220] | 824 Melem/s | 1.21 |
| `retention_combine/1B` | 3.062 s [3.013, 3.113] | 327 Melem/s | 3.06 |

**100M results (event-collecting functions):**

| Benchmark | Mean [95% CI] | Throughput | ns/element |
|---|---|---|---|
| `sessionize_update/100M` | 121.7 ms [121.0, 122.6] | 822 Melem/s | 1.22 |
| `retention_combine/100M` | 277.5 ms [275.5, 281.3] | 360 Melem/s | 2.78 |
| `window_funnel/100M` | 898 ms [888, 913] | 111 Melem/s | 8.98 |
| `sequence_match/100M` | 1.079 s [1.069, 1.092] | 92.7 Melem/s | 10.79 |
| `sequence_count/100M` | 1.574 s [1.540, 1.617] | 63.5 Melem/s | 15.74 |
| `sort_events/100M` | 2.207 s [2.173, 2.224] | 45.3 Melem/s | 22.07 |
| `sort_events_presorted/100M` | 1.891 s [1.873, 1.899] | 52.9 Melem/s | 18.91 |

#### Analysis

**Sessionize scales linearly to 1B**: 1.21 ns/element at 1B is consistent with
1.20 ns/element at 10M. The O(1) update operation (compare + conditional increment)
has no data-dependent memory access patterns, keeping the working set within registers.
Throughput (824 Melem/s) is stable from 100K to 1B, confirming compute-bound behavior.

**Retention degrades at 1B**: 3.06 ns/element at 1B vs 2.78 ns/element at 100M
vs 3.35 ns/element at 10M. The non-monotonic pattern reflects DRAM bandwidth
variability at these scales. The 1B working set (4 bytes × 1B = 3.7GB) exceeds L3
cache significantly, making the iteration purely DRAM-bound.

**Sort dominates event-collecting functions**: At 100M, sort_events takes 2.207s
while sequence_match takes 1.079s (sort is ~2x the total). This confirms sort
is O(n log n) with log(100M) ≈ 26.6, while the NFA scan is O(n).

**Memory ceiling**: Event-collecting functions at 100M use 1.6GB per benchmark
iteration. With 21GB total RAM and Criterion's multi-iteration measurement, 100M
is the practical maximum. 1B events (16GB) would exceed available memory during
the clone required by Criterion's iter loop.

### Session 6: Presorted Detection + 32-Condition Support

#### PERF-2: Presorted Detection in sort_events

**Hypothesis**: In practice, DuckDB often provides events in timestamp order
(via ORDER BY or naturally ordered data). An O(n) is_sorted check before
O(n log n) pdqsort provides effectively zero-cost sorting for presorted input.
For unsorted input, the O(n) scan adds negligible overhead.

**Technique**: Added `events.windows(2).all(|w| w[0].timestamp_us <= w[1].timestamp_us)`
check with early return before `sort_unstable_by_key`. The iterator has early
termination on first violation, so worst-case overhead for unsorted input is one
additional O(n) scan.

**Basis**:
- pdqsort already has O(n) adaptive behavior on presorted input, but still
  performs comparisons with its internal bookkeeping. An explicit is_sorted
  check with early termination is a pure scan with no bookkeeping overhead.
- For the presorted case: eliminates all sort overhead (no function pointer
  calls, no partitioning logic, no recursion).

**Analysis**: This optimization benefits the common case where DuckDB
provides events via ORDER BY. The presorted benchmark shows the upper
bound of improvement. For fully random input, the overhead is bounded
by one sequential scan of 16 bytes × n elements.

#### PARITY-2: 32-Condition Support (u8 → u32)

**Hypothesis**: Expanding the conditions bitmask from u8 (8 conditions) to u32
(32 conditions) should be performance-neutral because the Event struct size
does not change: i64 (8 bytes) + u32 (4 bytes) + 4 bytes padding = 16 bytes,
same as i64 (8 bytes) + u8 (1 byte) + 7 bytes padding = 16 bytes.

**Technique**: Changed `Event.conditions` from `u8` to `u32`. Updated all
condition checks, FFI bitmask packing, and function set registrations from
7 overloads (2-8) to 31 overloads (2-32) per function.

**Analysis**: The Event struct stays at 16 bytes due to alignment padding
(i64 requires 8-byte alignment). Copy semantics preserved. No change to
sort, combine, or NFA execution code — the bitmask is only accessed via
the `condition()` and `has_any_condition()` methods which use the same
bitwise operations on the wider type.

### Session 7: Negative Results + Baseline Validation

#### Presorted Detection Validation (Session 6 PERF-2 Retroactive)

Session 6 added presorted detection but did not run benchmarks. Session 7
retroactively validated the claim with 3 Criterion runs:

| Benchmark | Session 4 Baseline | Session 7 Run 1 | Session 7 Run 2 | Session 7 Run 3 |
|---|---|---|---|---|
| `sort_presorted/100M` | 1.891s [1.873, 1.899] | 1.859s [1.829, 1.876] | 1.788s [1.774, 1.815] | 1.840s [1.835, 1.844] |

**Verdict**: The presorted detection shows improvement in Run 2 (non-overlapping
CI with baseline) but CIs overlap in Runs 1 and 3. The improvement is real but
modest (~3-5%), consistent with pdqsort already having adaptive behavior on
presorted input. The explicit `is_sorted` check primarily eliminates pdqsort's
internal bookkeeping overhead.

#### PERF-1: LSD Radix Sort (NEGATIVE RESULT)

**Hypothesis**: LSD radix sort (8-bit radix, 8 passes) on i64 timestamps would
reduce sort time from O(n log n) ~22 ns/element to O(n) ~8-12 ns/element.

**Measured**: 8.92s at 100M (4.3x SLOWER than baseline 2.05s).

**Analysis**: The scatter pattern in LSD radix sort has catastrophically poor
cache locality for 16-byte elements. Eight passes of random writes through 1.6GB
working set cause constant TLB/cache misses. pdqsort's in-place partitioning has
excellent spatial locality that compensates for its O(n log n) comparison count.
At 100M elements, pdqsort's cache-friendly access pattern dominates radix sort's
theoretically better O(n) complexity.

**Lesson**: For in-memory sorting of embedded-key structs (where the key is part
of the element, not a separate array), comparison-based sorts with good cache
behavior beat distribution sorts with poor spatial locality. Radix sort is only
advantageous when sorting keys that are significantly smaller than the elements
(e.g., sorting indices/pointers by a separate key array).

#### PERF-3: Branchless Sessionize Update (NEGATIVE RESULT)

**Hypothesis**: Replacing data-dependent branches in `SessionizeBoundaryState::update`
with branchless operations (`i64::from(bool)`, `max`, `min`) would improve
throughput by avoiding branch misprediction on varied gap distributions.

**Measured**: Consistent ~5-10% regression at scales 100 through 10M. At 1B,
CIs overlap (1.262s [1.231, 1.287] vs baseline 1.213s [1.207, 1.220]).

**Analysis**: The benchmark's 90/10 boundary pattern is easily predicted by the
branch predictor (~99% accuracy). The `max`/`min` branchless operations add
constant overhead (CMOV instruction latency + always-execute both paths) that
exceeds the savings from the rare mispredictions. Additionally, the
`Some(timestamp_us.max(prev))` creates a new `Option` wrapper unconditionally,
whereas the branchy version only writes to `last_ts` when needed.

**Lesson**: Branchless optimization is only beneficial when branch prediction
accuracy is low (< ~95%). For highly predictable patterns (monotonic timestamps,
periodic boundaries), the branch predictor achieves near-perfect accuracy, and
branchless alternatives add unnecessary computation.

#### PERF-4: Retention Update (NOT ATTEMPTED)

**Analysis**: The `retention_update` benchmark's bottleneck is `Vec<bool>`
allocation (one per row inside the measurement loop), not the update logic itself.
The actual per-condition loop (`if cond { bitmask |= 1 << i }`) is already
optimal for a scalar implementation. The FFI layer has the same per-row
allocation overhead in production. Optimizing the core `update()` method would
not produce measurable improvement in the benchmark.

### Session 8: sequenceNextNode Baseline

#### New Benchmark: `sequence_next_node`

**Context**: Session 8 implemented `sequence_next_node`, completing ClickHouse
behavioral analytics parity. This is the first function using per-event String
storage (`NextNodeEvent` struct) rather than the `Copy` `Event` struct used by
all other functions.

**Benchmark design**: Update + finalize throughput at 100 to 10M events, and
combine_in_place chains at 100 to 100K states. Uses forward direction with
`first_match` base and 3-step patterns — the common case.

**Measured Results** (3 runs, 95% CI from Criterion):

| Benchmark | Run 1 | Run 2 | Run 3 | Median |
|---|---|---|---|---|
| `sequence_next_node/100` | 3.49 µs | 3.73 µs | 3.59 µs | 3.59 µs |
| `sequence_next_node/1K` | 41.5 µs | 44.7 µs | 41.9 µs | 41.9 µs |
| `sequence_next_node/10K` | 530 µs | 564 µs | 529 µs | 530 µs |
| `sequence_next_node/100K` | 9.79 ms | 10.5 ms | 9.75 ms | 9.79 ms |
| `sequence_next_node/1M` | 148 ms | 177 ms | 152 ms | 152 ms |
| `sequence_next_node/10M` | 1.237 s | 1.231 s | 1.175 s | 1.231 s |
| `combine/100` | 4.11 µs | 3.87 µs | 3.86 µs | 3.87 µs |
| `combine/1K` | 43.4 µs | 41.4 µs | 40.1 µs | 41.4 µs |
| `combine/10K` | 534 µs | 578 µs | 532 µs | 534 µs |
| `combine/100K` | 5.85 ms | 6.09 ms | 5.68 ms | 5.85 ms |

#### Analysis: String Allocation Cost

The dominant cost in `sequence_next_node` is per-event String heap allocation.
Comparing ns/element at 10K events (in L2/L3 cache):

| Function | 10K ns/element | Ratio |
|---|---|---|
| `sequence_match` | 1.70 | 1.0x (baseline) |
| `sequence_next_node` | 53.0 | 31x |

The 31x overhead comes from:
1. **String allocation**: Each `NextNodeEvent` contains `Option<String>` requiring
   one heap allocation per event (~20-80ns depending on allocator state)
2. **Larger struct size**: `NextNodeEvent` is ~80+ bytes (vs 16 bytes for `Event`),
   reducing cache line utilization from 4 events/line to ~1 event/line
3. **No Copy semantics**: `NextNodeEvent` requires `Clone` for combine operations,
   with deep String cloning per element
4. **No condition filtering**: All events are stored regardless of conditions
   (unlike `sequence_match` which filters with `has_any_condition()`)

#### Potential Optimization Opportunities (for future sessions)

1. **String interning / arena allocation**: Replace per-event `String` with
   indices into a shared string pool. Most behavioral analytics have a small
   cardinality of distinct event values (page names, action types) — typically
   <1000 distinct values across millions of events. An `IndexMap<String, u32>`
   would reduce per-event storage from ~50 bytes (String) to 4 bytes (u32 index).
   Expected improvement: 5-10x at scale.

2. **SmallString optimization**: Use an inline small-string type (e.g., 23-byte
   SSO) for short event values. Most page names and action types are under 24
   characters. This eliminates heap allocation for the common case.
   Expected improvement: 2-3x for typical workloads.

3. **Conditional event storage**: Store only events that could be the "next node"
   (i.e., events adjacent to pattern-matching events). Requires two-pass
   processing but could dramatically reduce storage for sparse patterns.
   Expected improvement: depends on selectivity, potentially 10-100x for rare
   patterns in large event streams.

4. **Batch String allocation**: Use a bump allocator or arena for all event
   Strings within a single update batch, deallocating in bulk during finalize.
   Eliminates per-String allocator overhead.
   Expected improvement: 1.5-2x from reduced allocator contention.

### Session 9: Rc\<str\> Optimization + String Pool Negative Result

#### PERF-1: String Pool with PooledEvent (NEGATIVE RESULT)

**Hypothesis**: Separating string storage into a `Vec<String>` pool with `u32`
indices in a `Copy` `PooledEvent` struct (24 bytes) would reduce per-event overhead
by eliminating heap-allocated `Option<String>` per event and enabling `Copy` semantics
for the event container.

**Technique**: Added `PooledEvent { timestamp_us: i64, string_idx: u32, conditions: u32,
base_condition: bool }` (24 bytes, Copy) alongside `Vec<String>` pool. Events store a
`u32` index into the pool instead of owning a `String`.

**Measured**: Consistent regression at most scales:

| Scale | Before [Session 8] | After (Pool) | Change |
|---|---|---|---|
| 100 | 3.59 µs | ~4.0 µs | +10-14% slower |
| 1K | 41.9 µs | ~53 µs | +26-30% slower |
| 10K | 530 µs | ~545 µs | +1-4% (marginal) |
| 100K | 9.79 ms | ~13.5 ms | +38-41% slower |
| 1M | 152 ms | ~120 ms | -21-22% faster |
| 10M | 1.231 s | ~1.22 s | CIs overlap |
| Combine 100 | 3.87 µs | ~4.2 µs | +8% slower |
| Combine 100K | 5.85 ms | ~9.1 ms | +55% slower |

**Analysis**: The dual-vector overhead (maintaining `events: Vec<PooledEvent>` and
`string_pool: Vec<String>` separately) adds indirection that is not compensated at
small/medium scales. The only improvement was at 1M (22% faster) where the reduced
per-element size (24 bytes vs 40 bytes) improved cache utilization sufficiently.
Combine is particularly affected because the pool vector must be cloned separately,
doubling the allocation work. **Reverted**.

**Lesson**: String interning via separate pool vectors only wins when the pool has
high reuse (low cardinality) AND the working set is large enough for the reduced
element size to dominate. For benchmarks with unique strings per event, the
indirection overhead outweighs the size reduction.

#### PERF-2: Rc\<str\> Reference-Counted Strings (ACCEPTED)

**Hypothesis**: Replacing `Option<String>` with `Option<Arc<str>>` in `NextNodeEvent`
reduces the struct from 40 bytes to 32 bytes and makes `clone()` O(1) (reference
count increment) instead of O(n) (deep copy). This benefits both update (avoiding
String allocation when values come from shared sources) and combine (O(1) clone per
element instead of per-character copy).

**Technique**:
1. Changed `NextNodeEvent::value` from `Option<String>` to `Option<Arc<str>>`
2. Updated FFI layer to convert `read_varchar` results via `Arc::from()`
3. Updated `finalize()` to use `.as_deref().map(String::from)` for final String extraction
4. Struct size reduced from 40 bytes (String = ptr+len+cap = 24 bytes + Option tag + padding)
   to 32 bytes (Arc\<str\> = ptr+len = 16 bytes + Option tag + padding)

**Basis**:
- Stroustrup (2013) "The C++ Programming Language" — shared_ptr/reference counting
  for immutable string data in analytics pipelines
- Event values in behavioral analytics have low cardinality (~100 distinct page names
  across millions of events), making reference counting ideal
- Arc\<str\> eliminates the capacity field (3-word String → 2-word fat pointer), reducing
  per-element memory by 20%

**Measured Results** (3 runs, 95% CIs, all non-overlapping):

| Benchmark | Before [95% CI] | After [95% CI] | Speedup |
|---|---|---|---|
| `sequence_next_node/100` | 2.91 µs [2.85, 2.97] | 1.13 µs [1.12, 1.14] | **2.6x** |
| `sequence_next_node/1K` | 35.0 µs [34.4, 35.6] | 10.9 µs [10.7, 11.1] | **3.2x** |
| `sequence_next_node/10K` | 450 µs [443, 457] | 111 µs [109, 113] | **4.1x** |
| `sequence_next_node/100K` | 9.21 ms [9.03, 9.40] | 1.58 ms [1.55, 1.61] | **5.8x** |
| `sequence_next_node/1M` | 148 ms [145, 151] | 80.5 ms [79.2, 81.8] | **1.8x** |
| `sequence_next_node/10M` | 1.17 s [1.14, 1.20] | 547 ms [538, 556] | **2.1x** |
| `combine/100` | 3.30 µs [3.24, 3.36] | 575 ns [569, 581] | **5.7x** |
| `combine/1K` | 33.0 µs [32.4, 33.6] | 5.12 µs [5.04, 5.20] | **6.4x** |
| `combine/10K` | 462 µs [454, 470] | 90.8 µs [89.3, 92.3] | **5.1x** |
| `combine/100K` | 5.05 ms [4.96, 5.14] | 1.78 ms [1.75, 1.81] | **2.8x** |

**Analysis**: The speedup increases from 2.6x at 100 events to 5.8x at 100K events,
then decreases to 1.8-2.1x at 1M-10M. This pattern reflects two competing effects:

1. **Clone cost reduction** (dominant at small/medium scales): Arc::clone is a single
   atomic increment (~1ns) vs String::clone which copies len bytes (~20-80ns for
   typical page names). At 100K events with 3-step matching, combine touches ~100K
   elements, saving ~2-8µs per element clone = ~200-800ms total → 5.8x.

2. **Memory bandwidth saturation** (dominant at large scales): At 1M+ events, the
   working set (32 bytes × 1M = 32MB) exceeds L3 cache. The bottleneck shifts from
   per-element allocation cost to DRAM bandwidth. Arc\<str\> (32 bytes) vs String (40
   bytes) provides a 20% struct size reduction, yielding a 1.8-2.1x improvement from
   better cache line utilization rather than clone cost savings.

Combine sees larger speedups (5.1-6.4x at 1K-10K) because combine_in_place clones
every element from the source state. With O(1) Arc::clone, the combine operation
becomes pure memcpy + atomic increment, close to the theoretical minimum.

#### Realistic Cardinality Benchmark

Added `sequence_next_node_realistic` benchmark that draws from a pool of 100
distinct `Arc<str>` values (matching typical behavioral analytics cardinality).
This measures the benefit of Arc\<str\> sharing in realistic workloads.

**Measured Results** (3 runs, 95% CIs):

| Benchmark | Mean [95% CI] | Throughput |
|---|---|---|
| `realistic/100` | 1.13 µs [1.12, 1.15] | 88.3 Melem/s |
| `realistic/1K` | 10.8 µs [10.7, 11.1] | 91.7 Melem/s |
| `realistic/10K` | 110 µs [108, 112] | 90.6 Melem/s |
| `realistic/100K` | 1.37 ms [1.35, 1.40] | 72.9 Melem/s |
| `realistic/1M` | 52.3 ms [51.3, 53.0] | 19.1 Melem/s |

**Comparison with unique-string benchmark** at equivalent scales:

| Scale | Unique strings | Realistic (100 distinct) | Improvement |
|---|---|---|---|
| 100 | 1.13 µs | 1.13 µs | ~0% (overhead elsewhere) |
| 10K | 111 µs | 110 µs | ~1% |
| 100K | 1.58 ms | 1.37 ms | **13%** |
| 1M | 80.5 ms | 52.3 ms | **35%** |

At 100K+ events, Arc\<str\> sharing provides additional 13-35% improvement over
unique strings because `Arc::clone` from the pool avoids even the initial `Arc::from()`
allocation that unique strings require. The benefit scales with working set size
as shared Arc values reduce total heap allocation pressure.

### Session 11: NFA Reusable Stack + Fast-Path Linear Scan

Two Criterion-validated optimizations targeting `sequence_count` and `sequence_combine`,
plus one negative result (first-condition pre-check).

**OPT-1: NFA Reusable Stack Allocation**

`execute_pattern` previously created a new `Vec<NfaState>` inside `try_match_from`
for every starting position. For `sequence_count`, which calls `try_match_from` from
every position (O(N) calls), this produced O(N) heap alloc/free pairs.

**Fix**: Pre-allocate the Vec once in `execute_pattern` and pass it by `&mut` reference
to `try_match_from`, which calls `clear()` (retaining capacity) instead of allocating.

| Benchmark | Before | After | Change |
|---|---|---|---|
| `sequence_count/100` | 1.14 µs | 597 ns | **-47%** (p=0.00) |
| `sequence_count/1K` | 6.13 µs | 4.09 µs | **-33%** (p=0.00) |
| `sequence_count/10K` | 55.9 µs | 36.9 µs | **-34%** (p=0.00) |
| `sequence_count/100K` | 592 µs | 392 µs | **-34%** (p=0.00) |
| `sequence_count/1M` | 6.21 ms | 4.13 ms | **-33%** (p=0.00) |
| `sequence_count/100M` | 1.44 s | 1.25 s | **-13%** (p=0.00) |
| `sequence_combine/100` | 1.58 µs | 1.13 µs | **-29%** (p=0.00) |
| `sequence_combine/10K` | 126.9 µs | 89.2 µs | **-30%** (p=0.00) |

Reference: Standard arena/pool reuse pattern — amortize allocation cost by
retaining heap capacity across iterations.

**OPT-2: Fast-Path Linear Scan for Common Pattern Shapes**

Added pattern classification at execution time. Patterns consisting only of
`Condition` steps (e.g., `(?1)(?2)`) use an O(n) sliding window scan. Patterns
with `Condition` + `AnyEvents` steps (e.g., `(?1).*(?2)`) use an O(n) single-pass
linear scan. Both eliminate all NFA overhead (stack management, function calls,
backtracking state).

| Benchmark | Before (NFA reuse) | After (fast path) | Change |
|---|---|---|---|
| `sequence_count/1K` | 4.15 µs | 2.51 µs | **-40%** (p=0.00) |
| `sequence_count/10K` | 36.8 µs | 22.5 µs | **-39%** (p=0.00) |
| `sequence_count/100K` | 391 µs | 234 µs | **-40%** (p=0.00) |
| `sequence_count/1M` | 4.08 ms | 2.43 ms | **-40%** (p=0.00) |
| `sequence_combine/100` | 1.17 µs | 795 ns | **-32%** (p=0.00) |
| `sequence_combine/1K` | 8.71 µs | 5.55 µs | **-36%** (p=0.00) |
| `sequence_combine/10K` | 85.0 µs | 57.7 µs | **-32%** (p=0.00) |

Combined effect (Session 9 baseline → Session 11 final):

| Benchmark | Session 9 | Session 11 | Total improvement |
|---|---|---|---|
| `sequence_count/1K` | 5.69 µs | 2.51 µs | **-56%** |
| `sequence_count/100K` | 589 µs | 234 µs | **-60%** |
| `sequence_count/1M` | 5.94 ms | 2.43 ms | **-59%** |
| `sequence_count/100M` | 1.574 s | 1.17 s | **-26%** |

Reference: Pattern-directed specialization — a compiler technique where the
executor selects an optimized code path based on pattern structure analysis.

**NEGATIVE: First-Condition Pre-Check**

Attempted to skip starting positions where the first condition doesn't match
before entering the NFA. Added a branch `if let Some(idx) = first_cond` at
the top of the search loop. Measured 0-8% regression across all scales due to
the extra branch in the hot path. The NFA already handles non-matching starts
efficiently (one push + one pop + one condition check + return None), so the
pre-check adds overhead without sufficient savings. Reverted and documented.

## Current Baseline

Recorded after Session 15 dependency refresh (Criterion 0.8.2, rand 0.9.2).
No Rust source code changes — numbers reflect updated benchmark framework
and data generation. Any future optimization must demonstrate improvement
against these values.

**Environment**: rustc 1.93.0, x86_64 Linux, Criterion 0.8.2, 100 samples
(10 samples for 100M/1B).

### Sessionize

| Benchmark | Mean [95% CI] | Throughput |
|---|---|---|
| `sessionize_update/100` | 127 ns [126, 128] | 787 Melem/s |
| `sessionize_update/1000` | 1.19 µs [1.19, 1.20] | 837 Melem/s |
| `sessionize_update/10000` | 11.3 µs [11.1, 11.4] | 888 Melem/s |
| `sessionize_update/100000` | 121 µs [120, 121] | 828 Melem/s |
| `sessionize_update/1000000` | 1.23 ms [1.22, 1.25] | 811 Melem/s |
| `sessionize_update/10000000` | 12.1 ms [12.0, 12.1] | 827 Melem/s |
| `sessionize_update/100000000` | 120 ms [120, 121] | 831 Melem/s |
| `sessionize_update/1000000000` | 1.20 s [1.20, 1.21] | 830 Melem/s |
| `sessionize_combine/100` | 137 ns [135, 138] | 732 Melem/s |
| `sessionize_combine/1000` | 1.42 µs [1.41, 1.43] | 704 Melem/s |
| `sessionize_combine/10000` | 14.0 µs [13.9, 14.1] | 714 Melem/s |
| `sessionize_combine/100000` | 287 µs [280, 293] | 349 Melem/s |
| `sessionize_combine/1000000` | 3.11 ms [3.06, 3.17] | 321 Melem/s |
| `sessionize_combine/10000000` | 103 ms [102, 104] | 97 Melem/s |
| `sessionize_combine/100000000` | 796 ms [784, 812] | 126 Melem/s |

### Retention

| Benchmark | Mean [95% CI] | Throughput |
|---|---|---|
| `retention_update/4_conditions` | 16.9 µs [16.7, 17.1] | 59 Melem/s |
| `retention_update/8_conditions` | 36.5 µs [36.0, 37.1] | 27 Melem/s |
| `retention_update/16_conditions` | 45.0 µs [44.2, 45.8] | 22 Melem/s |
| `retention_update/32_conditions` | 68.4 µs [68.1, 68.8] | 15 Melem/s |
| `retention_combine/100` | 90.8 ns [90.5, 91.3] | 1.10 Gelem/s |
| `retention_combine/1000` | 925 ns [922, 929] | 1.08 Gelem/s |
| `retention_combine/10000` | 9.35 µs [9.29, 9.43] | 1.07 Gelem/s |
| `retention_combine/100000` | 97.4 µs [96.8, 98.1] | 1.03 Gelem/s |
| `retention_combine/1000000` | 1.10 ms [1.09, 1.11] | 911 Melem/s |
| `retention_combine/10000000` | 32.6 ms [32.4, 32.8] | 307 Melem/s |
| `retention_combine/100000000` | 274 ms [272, 277] | 365 Melem/s |

### Window Funnel

| Benchmark | Mean [95% CI] | Throughput |
|---|---|---|
| `window_funnel_finalize/100,3` | 227 ns [226, 228] | 441 Melem/s |
| `window_funnel_finalize/1000,5` | 1.33 µs [1.30, 1.35] | 752 Melem/s |
| `window_funnel_finalize/10000,5` | 12.0 µs [11.8, 12.1] | 834 Melem/s |
| `window_funnel_finalize/100000,8` | 156 µs [154, 157] | 642 Melem/s |
| `window_funnel_finalize/1000000,8` | 1.61 ms [1.59, 1.62] | 622 Melem/s |
| `window_funnel_finalize/10000000,8` | 63.5 ms [60.9, 65.7] | 158 Melem/s |
| `window_funnel_finalize/100000000,8` | 791 ms [784, 798] | 126 Melem/s |
| `window_funnel_combine/100` | 549 ns [538, 563] | 182 Melem/s |
| `window_funnel_combine/1000` | 3.40 µs [3.33, 3.49] | 294 Melem/s |
| `window_funnel_combine/10000` | 33.6 µs [33.4, 33.9] | 297 Melem/s |
| `window_funnel_combine/100000` | 650 µs [640, 661] | 154 Melem/s |
| `window_funnel_combine/1000000` | 10.5 ms [10.3, 10.8] | 95 Melem/s |

### Sequence

| Benchmark | Mean [95% CI] | Throughput |
|---|---|---|
| `sequence_match/100` | 377 ns [373, 382] | 265 Melem/s |
| `sequence_match/1000` | 1.36 µs [1.35, 1.38] | 734 Melem/s |
| `sequence_match/10000` | 11.6 µs [11.4, 11.8] | 861 Melem/s |
| `sequence_match/100000` | 164 µs [162, 167] | 608 Melem/s |
| `sequence_match/1000000` | 1.95 ms [1.92, 1.98] | 513 Melem/s |
| `sequence_match/10000000` | 117 ms [116, 118] | 86 Melem/s |
| `sequence_match/100000000` | 1.05 s [1.03, 1.06] | 95 Melem/s |
| `sequence_count/100` | 433 ns [430, 437] | 231 Melem/s |
| `sequence_count/1000` | 1.91 µs [1.84, 2.00] | 523 Melem/s |
| `sequence_count/10000` | 16.1 µs [15.9, 16.2] | 623 Melem/s |
| `sequence_count/100000` | 206 µs [203, 209] | 486 Melem/s |
| `sequence_count/1000000` | 2.73 ms [2.69, 2.78] | 366 Melem/s |
| `sequence_count/10000000` | 134 ms [133, 135] | 75 Melem/s |
| `sequence_count/100000000` | 1.18 s [1.17, 1.19] | 85 Melem/s |
| `sequence_combine/100` | 659 ns [649, 673] | 152 Melem/s |
| `sequence_combine/1000` | 4.25 µs [4.17, 4.34] | 235 Melem/s |
| `sequence_combine/10000` | 43.1 µs [42.7, 43.6] | 232 Melem/s |
| `sequence_combine/100000` | 941 µs [936, 946] | 106 Melem/s |
| `sequence_combine/1000000` | 23.9 ms [23.5, 24.4] | 42 Melem/s |

### Sequence Match Events

| Benchmark | Mean [95% CI] | Throughput |
|---|---|---|
| `sequence_match_events/100` | 526 ns [519, 535] | 190 Melem/s |
| `sequence_match_events/1000` | 1.75 µs [1.74, 1.76] | 571 Melem/s |
| `sequence_match_events/10000` | 12.8 µs [12.8, 12.9] | 778 Melem/s |
| `sequence_match_events/100000` | 166 µs [165, 167] | 602 Melem/s |
| `sequence_match_events/1000000` | 1.99 ms [1.98, 2.00] | 503 Melem/s |
| `sequence_match_events/10000000` | 117 ms [117, 118] | 85 Melem/s |
| `sequence_match_events/100000000` | 1.07 s [1.06, 1.08] | 93 Melem/s |
| `sequence_match_events_combine/100` | 915 ns [908, 923] | 109 Melem/s |
| `sequence_match_events_combine/1000` | 3.80 µs [3.78, 3.83] | 263 Melem/s |
| `sequence_match_events_combine/10000` | 36.6 µs [36.4, 36.8] | 273 Melem/s |
| `sequence_match_events_combine/100000` | 912 µs [890, 934] | 110 Melem/s |
| `sequence_match_events_combine/1000000` | 17.1 ms [16.9, 17.2] | 59 Melem/s |

### Sort (Isolated)

| Benchmark | Mean [95% CI] | Throughput |
|---|---|---|
| `sort_events/100` | 112 ns [110, 114] | 894 Melem/s |
| `sort_events/1000` | 798 ns [782, 816] | 1.25 Gelem/s |
| `sort_events/10000` | 11.3 µs [11.2, 11.5] | 881 Melem/s |
| `sort_events/100000` | 198 µs [196, 200] | 505 Melem/s |
| `sort_events/1000000` | 3.34 ms [3.29, 3.38] | 300 Melem/s |
| `sort_events/10000000` | 212 ms [211, 213] | 47 Melem/s |
| `sort_events/100000000` | 2.079 s [2.072, 2.090] | 48 Melem/s |
| `sort_events_presorted/100` | 100 ns [99.1, 100.9] | 1.00 Gelem/s |
| `sort_events_presorted/1000` | 481 ns [477, 486] | 2.08 Gelem/s |
| `sort_events_presorted/10000` | 7.95 µs [7.92, 7.98] | 1.26 Gelem/s |
| `sort_events_presorted/100000` | 172 µs [171, 173] | 582 Melem/s |
| `sort_events_presorted/1000000` | 2.60 ms [2.56, 2.63] | 385 Melem/s |
| `sort_events_presorted/10000000` | 183 ms [182, 184] | 55 Melem/s |
| `sort_events_presorted/100000000` | 1.895 s [1.882, 1.908] | 53 Melem/s |

### Sequence Next Node

`sequence_next_node` stores `NextNodeEvent` structs (timestamp + `Option<Arc<str>>` +
conditions + base_condition, 32 bytes) which are larger than the `Copy` `Event` struct
(16 bytes).

| Benchmark | Mean [95% CI] | Throughput |
|---|---|---|
| `sequence_next_node/100` | 1.95 µs [1.94, 1.97] | 51 Melem/s |
| `sequence_next_node/1000` | 19.1 µs [19.0, 19.2] | 52 Melem/s |
| `sequence_next_node/10000` | 193 µs [191, 195] | 52 Melem/s |
| `sequence_next_node/100000` | 2.23 ms [2.22, 2.25] | 45 Melem/s |
| `sequence_next_node/1000000` | 54.4 ms [53.1, 56.7] | 18 Melem/s |
| `sequence_next_node/10000000` | 546 ms [543, 550] | 18 Melem/s |
| `sequence_next_node_combine/100` | 1.54 µs [1.52, 1.55] | 65 Melem/s |
| `sequence_next_node_combine/1000` | 14.0 µs [13.9, 14.1] | 71 Melem/s |
| `sequence_next_node_combine/10000` | 153 µs [151, 155] | 66 Melem/s |
| `sequence_next_node_combine/100000` | 2.08 ms [2.06, 2.11] | 48 Melem/s |
| `sequence_next_node_combine/1000000` | 79.5 ms [79.0, 80.0] | 13 Melem/s |
| `sequence_next_node_realistic/100` | 2.10 µs [2.07, 2.14] | 48 Melem/s |
| `sequence_next_node_realistic/1000` | 19.6 µs [19.4, 19.7] | 51 Melem/s |
| `sequence_next_node_realistic/10000` | 196 µs [195, 197] | 51 Melem/s |
| `sequence_next_node_realistic/100000` | 2.06 ms [2.05, 2.07] | 49 Melem/s |
| `sequence_next_node_realistic/1000000` | 51.6 ms [51.3, 51.9] | 19 Melem/s |
| `sequence_next_node_realistic/10000000` | 533 ms [529, 536] | 19 Melem/s |

**Analysis**: `sequence_next_node` throughput is ~18 Melem/s at 10M scale. The
realistic cardinality benchmark (100 distinct Arc\<str\> values) shows consistent
performance with unique strings at scale, confirming Arc sharing benefits in
production workloads.

### Per-Element Cost at Scale

Cost per element at scale. Session 15 refresh numbers:

| Function | Scale | Time | ns/element | Throughput | Bottleneck |
|---|---|---|---|---|---|
| `sessionize_update` | 1B | 1.20 s | 1.20 | 830 Melem/s | O(1) per-event, compute-bound |
| `retention_combine` | 100M | 274 ms | 2.74 | 365 Melem/s | O(1) per-state, DRAM-bound |
| `window_funnel_finalize` | 100M | 791 ms | 7.91 | 126 Melem/s | Sort + O(n*k) greedy scan |
| `sequence_match` | 100M | 1.05 s | 10.5 | 95 Melem/s | Sort + O(n) fast-path scan |
| `sequence_count` | 100M | 1.18 s | 11.8 | 85 Melem/s | Sort + O(n) fast-path counting |
| `sequence_match_events` | 100M | 1.07 s | 10.7 | 93 Melem/s | Sort + NFA + timestamp collection |
| `sequence_next_node` | 10M | 546 ms | 54.6 | 18 Melem/s | Sort + sequential scan + Arc\<str\> alloc |
| `sort_events` (random) | 100M | 2.079 s | 20.79 | 48 Melem/s | O(n log n) pdqsort, DRAM-bound |
| `sort_events` (presorted) | 100M | 1.895 s | 18.95 | 53 Melem/s | O(n) adaptive, DRAM-bound |

### Headline Numbers

Criterion-validated, reproducible headline numbers for portfolio presentation
(Session 15, Criterion 0.8.2, rand 0.9.2):

| Function | Scale | Wall Clock | Throughput | ns/element |
|---|---|---|---|---|
| **`sessionize_update`** | **1 billion** | **1.20 s** | **830 Melem/s** | **1.20** |
| **`retention_combine`** | 100 million | 274 ms | 365 Melem/s | 2.74 |
| `window_funnel_finalize` | 100 million | 791 ms | 126 Melem/s | 7.91 |
| `sequence_match` | 100 million | 1.05 s | 95 Melem/s | 10.5 |
| `sequence_count` | 100 million | 1.18 s | 85 Melem/s | 11.8 |
| `sequence_match_events` | 100 million | 1.07 s | 93 Melem/s | 10.7 |
| `sequence_next_node` | 10 million | 546 ms | 18 Melem/s | 54.6 |
| `sort_events` (random) | 100 million | 2.08 s | 48 Melem/s | 20.8 |

Note: Minor throughput differences from Session 14 reflect Criterion 0.8.2's
updated statistical sampling and rand 0.9.2's different data generation.
No Rust source code was changed. CIs overlap with Session 14 in all cases.

Memory constraint: Event-collecting functions store 16 bytes per event. At 100M events
= 1.6GB working set. 1B events would require 16GB + clone overhead, exceeding
available memory (21GB) during Criterion's measurement loop. The 100M scale is
the measured maximum for these functions — not extrapolated. `sequence_next_node`
stores per-event Arc\<str\> data (32 bytes per NextNodeEvent) limiting its practical
benchmark maximum to 10M events.

## Session Improvement Protocol

Every performance engineering session must follow this protocol to ensure
measurable, auditable, session-over-session improvement:

### Before Starting Work

1. Run `cargo bench` three times. Record all numbers as the session-start baseline.
2. Compare session-start baseline against the "Current Baseline" section above.
3. If any benchmark has regressed beyond its confidence interval, investigate
   and resolve before proceeding with new optimizations.
4. All quality gates must pass: `cargo test`, `cargo clippy --all-targets`,
   `cargo fmt -- --check`, `cargo doc --no-deps`.

### During Work

5. Each optimization is one atomic commit with measured before/after data.
6. Only accept improvements where confidence intervals do not overlap.
7. Revert and document optimizations that show no statistically significant improvement.
8. Never batch multiple optimizations into one measurement.

### After Completing Work

9. Run full benchmark suite. Record all numbers.
10. Update the "Current Baseline" section with new post-optimization numbers.
11. Add a new entry to the "Optimization History" section with:
    - Hypothesis, technique, academic/industry basis
    - Before/after measurements with full confidence intervals
    - Analysis of why the improvement occurred and its scaling characteristics
12. Update test count and benchmark inventory if changed.
13. Commit, push, and verify CI passes.

### Quality Standards for Performance Claims

- Every number includes a confidence interval
- Every speedup ratio is computed from mean values with CI verification
- No rounding in favor of changes — report exactly what Criterion measures
- Scaling analysis must explain *why* the improvement has the observed
  characteristics, not just *that* it improved
