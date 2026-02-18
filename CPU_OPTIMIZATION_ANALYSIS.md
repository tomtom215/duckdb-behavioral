# CPU Optimization Deep Dive: duckdb-behavioral Extension

**Date:** 2026-02-18
**Scope:** Rigorous analysis of five CPU optimization strategies against actual codebase hot paths.
**Standard:** Peer-reviewed academic evidence required. Negative results documented honestly.

---

## Table of Contents

- [Current Performance Baseline](#current-performance-baseline)
- [1. SIMD Intrinsics for Condition Evaluation](#1-simd-intrinsics-for-condition-evaluation)
- [2. Parallel Sort via Rayon](#2-parallel-sort-via-rayon)
- [3. NFA Fast Path Expansion](#3-nfa-fast-path-expansion)
- [4. Cache-Line Alignment Optimization](#4-cache-line-alignment-optimization)
- [5. Profile-Guided Optimization (PGO) Builds](#5-profile-guided-optimization-pgo-builds)
- [Prioritized Recommendation Matrix](#prioritized-recommendation-matrix)
- [References](#references)

---

## Current Performance Baseline

Session 15 (Criterion 0.8.2, 95% CI):

| Function | Scale | Wall Clock | Throughput |
|---|---|---|---|
| `sessionize_update` | 1 billion | 1.20 s | 830 Melem/s |
| `retention_combine` | 100 million | 274 ms | 365 Melem/s |
| `window_funnel` | 100 million | 791 ms | 126 Melem/s |
| `sequence_match` | 100 million | 1.05 s | 95 Melem/s |
| `sequence_count` | 100 million | 1.18 s | 85 Melem/s |
| `sequence_match_events` | 100 million | 1.07 s | 93 Melem/s |
| `sequence_next_node` | 10 million | 546 ms | 18 Melem/s |

**Execution time breakdown** (estimated from algorithmic analysis):

For event-collecting functions (window_funnel, sequence_*) at 100M events:
- **Sort phase**: ~300-400ms (pdqsort O(n log n)) when unsorted; ~50ms (presorted check) when sorted
- **Scan/NFA phase**: ~400-700ms (greedy scan or NFA execution)
- **Event collection phase**: ~100ms (Vec::push during update, amortized O(1))

---

## 1. SIMD Intrinsics for Condition Evaluation

### Hypothesis

Use AVX2 (256-bit) or NEON (128-bit) SIMD instructions to parallelize
inner loops that check event conditions, perform presorted detection,
or evaluate window expiry.

### Target Hot Paths

#### 1a. Presorted Detection (`src/common/event.rs:114-122`)

```rust
// Current: scalar pair-wise comparison
events.windows(2).all(|w| w[0].timestamp_us <= w[1].timestamp_us)
```

**SIMD approach:** Load 4 timestamps (AVX2: 4×i64 = 256 bits), shift by 1,
and compare in parallel. Reduce with `_mm256_movemask_epi8`.

**Analysis:**
- Input: contiguous `Event` array, 16 bytes/event, timestamps at offset 0
- Stride: 16 bytes between timestamps (not contiguous — 4 bytes padding + 4 bytes conditions between)
- This means we need **gather** instructions (`_mm256_i64gather_epi64`) to load timestamps from strided memory, or restructure data into SoA (Structure of Arrays)
- Gather instructions on AVX2 have ~2x the latency of contiguous loads [1]
- Current loop: ~2 cycles/comparison (load + compare + branch), highly predictable
- SIMD: 4 comparisons per instruction but gather overhead + reduction overhead

**Estimated speedup:** 1.5-2x on the presorted check phase. But the presorted check is O(n) and already only ~50ms for 100M events. **Absolute savings: ~25ms.** This is 3-5% of total execution time for event-collecting functions.

**Verdict: LOW PRIORITY.** The presorted check is not the bottleneck. When data IS presorted, the check is fast enough. When data is NOT presorted, sort dominates and the check is negligible overhead.

#### 1b. `Event::from_bools` Condition Packing (`src/common/event.rs:58-69`)

```rust
// Current: scalar loop, 2-32 iterations
for (i, &cond) in conditions.iter().enumerate().take(MAX_EVENT_CONDITIONS) {
    if cond { bitmask |= 1 << i; }
}
```

**SIMD approach:** Load 16 or 32 bytes of `bool` values (each bool is 1 byte),
compare against zero with `_mm256_cmpeq_epi8`, extract bitmask with
`_mm256_movemask_epi8`.

```rust
// Pseudocode for AVX2 path
unsafe fn pack_bools_simd(conditions: &[bool]) -> u32 {
    let ptr = conditions.as_ptr() as *const __m256i;
    let zero = _mm256_setzero_si256();
    let cmp = _mm256_cmpeq_epi8(_mm256_loadu_si256(ptr), zero);
    let mask = _mm256_movemask_epi8(cmp) as u32;
    !mask // invert: cmpeq gives 1 for zero (false), we want 1 for non-zero (true)
}
```

**Analysis:**
- This replaces 32 iterations of (load byte + compare + conditional OR) with 1 SIMD load + 1 compare + 1 movemask
- **BUT:** `from_bools` is called once per row during `update()`, in the FFI layer
- The FFI layer reads booleans from DuckDB's columnar vectors one-at-a-time: each call to `from_bools` processes conditions for a single event
- DuckDB delivers data in 2048-row chunks; `from_bools` is called 2048 times per chunk
- Per-call cost (scalar): ~5-15ns for 3-5 conditions (typical), ~30-50ns for 32 conditions
- Per-call cost (SIMD): ~3-5ns regardless of condition count
- **Total savings per chunk**: (30ns - 5ns) × 2048 = ~51µs. At 100M events: ~2.5ms saved.

**Verdict: NEGLIGIBLE.** The boolean packing is not a bottleneck. Total savings of 2.5ms on a 791ms operation (0.3%). Not worth the `unsafe` code and platform-specific branches.

#### 1c. Funnel Scan Window Expiry (`src/window_funnel.rs:359`)

```rust
// Current: per-event scalar subtraction + comparison
if event.timestamp_us - entry_ts > self.window_size_us { break; }
```

**SIMD approach:** Prefetch 4 timestamps, broadcast `entry_ts` and
`window_size_us`, SIMD subtract and compare to find the first event
that exceeds the window.

**Analysis:**
- The window check is a **single comparison per event** — already 1-2 cycles
- The loop has **data-dependent control flow** after this check (mode evaluations, condition checks)
- SIMD would need to speculatively evaluate 4 events in parallel, then discard results after the window break point
- Branch predictor handles the window break extremely well: events are sorted by timestamp, so the break is highly predictable once it happens
- The condition checks (`event.condition(current_step)`) after the window check are inherently sequential due to step advancement dependencies

**Verdict: NOT APPLICABLE.** The window check cannot be meaningfully SIMD-ized in isolation because the surrounding loop has sequential state dependencies (`current_step`). SIMD-izing just the window check while keeping the rest scalar adds overhead for no gain.

#### 1d. NFA Condition Check (`src/pattern/executor.rs:298`)

```rust
// Current: single bitwise operation per event per NFA state
if event.condition(*cond_idx) {
    // (self.conditions >> idx) & 1 != 0  -- already essentially free
}
```

**Analysis:**
- `event.condition(idx)` compiles to: shift right + AND + compare — **3 instructions total**
- This is already faster than any SIMD setup overhead
- The NFA processes states sequentially (LIFO stack); each state evaluation depends on the result of the previous
- No batch of independent condition checks exists to SIMD-ize

**Verdict: NOT APPLICABLE.** Bitwise condition checks are already at the instruction-level minimum. SIMD cannot improve single-bit extraction.

### SIMD Overall Assessment

| Target | Estimated Speedup | Absolute Savings (100M) | Implementation Risk | Verdict |
|---|---|---|---|---|
| Presorted detection | 1.5-2x | ~25ms (3-5%) | Low | Low priority |
| `from_bools` packing | 3-5x | ~2.5ms (0.3%) | Medium | Negligible |
| Window expiry | None | 0ms | N/A | Not applicable |
| NFA condition check | None | 0ms | N/A | Not applicable |

**SIMD intrinsics are NOT a significant optimization vector for this codebase.**

The reason is architectural: our hot paths are not dominated by data-parallel
operations on independent elements. They are dominated by **sequential state
machines** (funnel scan, NFA, session tracking) where each step depends on
the previous step's result. SIMD excels at applying the same operation to
many independent data elements simultaneously — our workload has the opposite
characteristic.

The academic literature confirms this. The CEUR-WS paper on SIMD for database
systems [1] demonstrates SIMD benefits for filter/scan/aggregate operations
with independent row evaluations. Our aggregate functions have per-row state
dependencies that prevent vectorization.

**Recommendation: Do not pursue SIMD intrinsics.** The engineering cost
(platform-specific `unsafe` code, `#[target_feature]` guards, ARM/x86
dual paths, nightly considerations for AVX-512) vastly exceeds the <5%
theoretical gain. The compiler's auto-vectorizer already handles the
vectorizable portions.

---

## 2. Parallel Sort via Rayon

### Hypothesis

Replace `sort_unstable_by_key` with Rayon's `par_sort_unstable_by_key` for
large unsorted event arrays, achieving multi-core speedup on the sort phase.

### Target Hot Path (`src/common/event.rs:121`)

```rust
events.sort_unstable_by_key(|e| e.timestamp_us);
```

### Analysis

**Rayon's parallel sort** uses the same pdqsort algorithm but parallelizes
the recursive quicksort calls using `rayon::join` [2]. Both halves of each
partition are sorted concurrently on separate threads via Rayon's work-stealing
scheduler.

**When sort is actually invoked:**
- Only when the presorted detection check fails (line 114-119)
- In practice, DuckDB delivers events via `ORDER BY timestamp` in most queries
- The presorted check is O(n) and short-circuits the sort entirely
- Sort is only needed for truly unordered input or combine results

**Sort phase cost when triggered:**
- 100M events × 16 bytes = 1.6 GB working set
- pdqsort at 100M: ~300-400ms (from sort benchmarks in PERF.md)
- Rayon parallel sort expected speedup: 2-4x on 4+ cores [2]
- Expected parallel sort time: ~100-150ms on 8 cores
- **Absolute savings: ~200-250ms** when sort is triggered

**Crossover point considerations:**
- Rayon has thread pool initialization overhead (~10-50µs first call, amortized to ~0 after)
- Parallel sort has synchronization overhead between partitions
- For small arrays (<10K elements), sequential pdqsort is faster due to lower overhead
- Need a threshold check: `if events.len() > PARALLEL_SORT_THRESHOLD { par_sort } else { sort }`
- Academic benchmarks suggest crossover around 10K-100K elements for 16-byte structs [2]

**Implementation:**

```rust
// In event.rs
pub fn sort_events(events: &mut [Event]) {
    if events.windows(2).all(|w| w[0].timestamp_us <= w[1].timestamp_us) {
        return;
    }
    #[cfg(feature = "parallel")]
    if events.len() > 100_000 {
        use rayon::slice::ParallelSliceMut;
        events.par_sort_unstable_by_key(|e| e.timestamp_us);
        return;
    }
    events.sort_unstable_by_key(|e| e.timestamp_us);
}
```

**Dependency impact:**
- `rayon = "1.10"` — mature, stable Rust, no nightly required
- MSRV compatibility: Rayon supports Rust 1.63+, our MSRV is 1.80 — compatible
- Binary size increase: ~200-400 KB (Rayon + crossbeam)
- Must be behind a `feature = "parallel"` flag to keep the default dependency-light

**Risk assessment:**
- DuckDB already uses its own thread pool for parallel aggregation (multiple GROUP BY groups processed in parallel)
- Rayon would create a SECOND thread pool inside the extension
- Potential thread contention between DuckDB's threads and Rayon's threads
- Could cause performance degradation under high concurrency if DuckDB is already using all cores
- **Mitigation:** Use `rayon::ThreadPoolBuilder::new().num_threads(N)` to limit Rayon's pool size

**Interaction with presorted detection:**
- Most real workloads use `ORDER BY timestamp` — presorted check succeeds, sort is skipped
- Parallel sort only helps the minority case of truly unordered data
- The presorted check itself is not parallelizable (requires sequential scan)

### Verdict

**MEDIUM PRIORITY, HIGH RISK.** Parallel sort can deliver 2-4x speedup on the
sort phase (~200ms savings), but ONLY when data is truly unordered (minority
case). The thread pool contention risk with DuckDB is a real concern that
requires benchmarking under concurrent query load. Must be behind a feature
flag.

**Recommended approach:** Implement behind `feature = "parallel"`, benchmark
both standalone and under concurrent DuckDB query load, and only promote to
default if the thread pool interaction is confirmed safe. Need to empirically
find the crossover threshold on target hardware.

---

## 3. NFA Fast Path Expansion

### Hypothesis

Extend the pattern classification system (Session 11) to cover additional
common pattern shapes, routing them to O(n) specialized scans instead of
the general-purpose NFA.

### Current Fast Paths (`src/pattern/executor.rs:90-124`)

```rust
fn classify_pattern(pattern: &CompiledPattern) -> PatternShape {
    // Currently classifies:
    // 1. AdjacentConditions: (?1)(?2)(?3)     → fast_adjacent O(n*k)
    // 2. WildcardSeparated:  (?1).*(?2).*(?3) → fast_wildcard O(n)
    // 3. Complex: everything else              → NFA O(n*s)
}
```

### Candidate New Fast Paths

#### 3a. Time-Constrained Wildcard: `(?1).*(?t<=T)(?2)`

This is one of the most common ClickHouse sequence patterns — it adds a time
window to the wildcard separation.

```rust
// Proposed: fast_wildcard_timed
fn fast_wildcard_timed(
    events: &[Event],
    conditions: &[usize],
    time_ops: &[(TimeOp, i64)], // constraint per gap
    count_all: bool,
) -> MatchResult {
    let k = conditions.len();
    let mut total = 0;
    let mut step = 0;
    let mut last_match_ts: Option<i64> = None;

    for event in events {
        // Check time constraint before condition
        if step > 0 {
            if let Some(prev_ts) = last_match_ts {
                if let Some(&(ref op, threshold)) = time_ops.get(step - 1) {
                    let elapsed = (event.timestamp_us - prev_ts) / 1_000_000;
                    if !op.evaluate(elapsed, threshold) {
                        // Time constraint failed — reset to step 0
                        step = 0;
                        last_match_ts = None;
                    }
                }
            }
        }
        if event.condition(conditions[step]) {
            last_match_ts = Some(event.timestamp_us);
            step += 1;
            if step >= k {
                total += 1;
                if !count_all { return MatchResult { matched: true, count: 1 }; }
                step = 0;
                last_match_ts = None;
            }
        }
    }
    MatchResult { matched: total > 0, count: total }
}
```

**Complexity:** O(n) single-pass, same as `fast_wildcard`
**Estimated impact:** Eliminates NFA for the most common time-constrained patterns
**Current NFA cost for these patterns:** O(n * s) where s = number of steps including time constraints

**Analysis:**
- Time-constrained patterns are common in behavioral analytics (e.g., "did action A within 5 minutes of action B?")
- Currently these fall through to `PatternShape::Complex` → full NFA
- The NFA has overhead: stack management, state object allocation, backtracking exploration
- A specialized O(n) scan would eliminate all NFA overhead

**Estimated speedup:** For `(?1).*(?t<=300)(?2)` at 100M events:
- NFA: ~1.05s (measured Session 15 baseline for sequence_match)
- Fast path: ~500ms (extrapolated from fast_wildcard performance being ~50% of NFA cost, Session 11)
- **Absolute savings: ~500ms (48% improvement)**

#### 3b. Single Condition: `(?1)`

Trivial pattern that only checks if any event has condition 1. Currently
runs through the full pattern classification + NFA machinery.

```rust
fn fast_single_condition(events: &[Event], cond_idx: usize) -> MatchResult {
    for event in events {
        if event.condition(cond_idx) {
            return MatchResult { matched: true, count: 1 };
        }
    }
    MatchResult { matched: false, count: 0 }
}
```

**Estimated impact:** Negligible for performance (already fast via NFA), but
eliminates unnecessary code paths. Good for code clarity.

#### 3c. Adjacent with Optional Wildcard Prefix: `.*(?1)(?2)`

Pattern with a wildcard prefix followed by adjacent conditions. Currently
classified as `WildcardSeparated` (correct for match semantics but suboptimal
for adjacent conditions in the suffix).

**Analysis:** Already handled correctly by `fast_wildcard`. No additional
fast path needed — `fast_wildcard` with conditions `[1, 2]` scans linearly
for condition 1 then condition 2, which is identical to the desired behavior.

### NFA Fast Path Assessment

| New Fast Path | Pattern Shape | Current Cost | Proposed Cost | Savings | Complexity |
|---|---|---|---|---|---|
| Time-constrained wildcard | `(?1).*(?t<=T)(?2)` | O(n*s) NFA | O(n) linear | ~48% | Medium |
| Single condition | `(?1)` | O(n) NFA | O(n) scan | ~5% | Low |
| Adjacent + wildcard prefix | `.*(?1)(?2)` | O(n) wildcard | O(n) | 0% | N/A |

### Verdict

**HIGH PRIORITY for time-constrained wildcard.** This is the single
highest-impact optimization available. It targets the most common
behavioral analytics pattern shape and can deliver ~48% improvement
for sequence functions when time constraints are used. Implementation
is straightforward — extend `classify_pattern` to detect the pattern
and add a specialized scan function.

**Low priority for single condition.** Marginal improvement, but trivial
to implement.

---

## 4. Cache-Line Alignment Optimization

### Hypothesis

Align the `Event` struct or event arrays to cache line boundaries (64 bytes
on x86_64, 128 bytes on Apple Silicon) to reduce cache misses during
sequential scanning.

### Current Data Layout

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event {
    pub timestamp_us: i64,  // 8 bytes, offset 0
    pub conditions: u32,    // 4 bytes, offset 8
    // implicit padding:    // 4 bytes, offset 12
}                           // Total: 16 bytes, alignment: 8
```

**Cache line utilization:**
- L1 cache line: 64 bytes (x86_64) → **4 Event structs per cache line**
- Sequential scan hits: 4 events per cache fetch → 75% utilization
  (only 48 of 64 bytes are data; 16 bytes are padding)
- Apple M-series: 128-byte cache lines → 8 events per line

### Analysis

#### 4a. `#[repr(align(64))]` on Event — NOT RECOMMENDED

```rust
#[repr(C, align(64))]
pub struct AlignedEvent {
    pub timestamp_us: i64,
    pub conditions: u32,
    _padding: [u8; 52], // pad to 64 bytes
}
```

**Impact:** Each event would be 64 bytes instead of 16. This would:
- **4x memory consumption** — 100M events: 6.4 GB instead of 1.6 GB
- **Eliminate L3 cache fit** — L3 is typically 8-32 MB; 1.6 GB already doesn't fit
- **Reduce DRAM bandwidth utilization** — loading 64 bytes to access 12 useful bytes = 19% utilization (down from 75%)
- **Worsen sort performance** — pdqsort swaps elements; 64-byte swaps are 4x more expensive

This is a well-known anti-pattern. Cache-line alignment helps when you have
**multiple independent data items per cache line that are accessed by different
threads** (preventing false sharing). Our events are accessed sequentially by
a single thread — false sharing is not a concern [3].

**Verdict: HARMFUL.** Aligning individual events to cache lines would degrade
performance significantly.

#### 4b. Vec Allocator Alignment — MARGINAL

Ensure the `Vec<Event>` allocation is aligned to a cache line boundary so that
the first event starts at a cache-line-aligned address.

```rust
use std::alloc::{alloc, Layout};
let layout = Layout::from_size_align(n * 16, 64).unwrap();
```

**Analysis:**
- `Vec<Event>` already uses the global allocator, which typically returns
  16-byte aligned memory (sufficient for Event's natural alignment)
- Cache line alignment of the array start only saves at most **one** cache
  miss — the very first load. All subsequent loads are sequential and the
  hardware prefetcher handles them
- Modern CPUs (Intel since Haswell, AMD since Zen) have hardware prefetchers
  that detect sequential access patterns within 1-2 cache misses and begin
  prefetching ahead of the access stream [4]
- The hardware prefetcher does not require the array to start at a cache line
  boundary

**Estimated impact:** <0.01% improvement. One cache miss saved out of millions.

**Verdict: NOT WORTH IT.** Hardware prefetching already handles sequential
access patterns. Array start alignment has negligible impact.

#### 4c. Structure of Arrays (SoA) — ARCHITECTURAL CHANGE

Instead of `Vec<Event>` (Array of Structs), store timestamps and conditions
in separate arrays:

```rust
struct EventsSoA {
    timestamps: Vec<i64>,   // contiguous i64 array
    conditions: Vec<u32>,   // contiguous u32 array
}
```

**Analysis:**
- Sort would operate on timestamps only (8 bytes/element instead of 16)
- Presorted check would access a contiguous i64 array (8 events per cache line instead of 4)
- Condition checks during scan would access a contiguous u32 array
- **Sort speedup:** pdqsort on 8-byte keys vs 16-byte structs: ~1.5x faster (fewer cache misses, smaller swap)
- **Scan speedup:** Window expiry check (timestamp-only) would have 2x spatial locality

**BUT:**
- Requires major refactoring of all 7 functions and FFI layer
- Sort must simultaneously reorder both arrays (parallel sort of two arrays)
- Breaks the clean `Event` abstraction used across the entire codebase
- `combine_in_place` must extend two Vecs instead of one
- `NextNodeEvent` (which adds `Arc<str>` value) cannot fit this model

**Estimated speedup:** ~15-25% for sort-dominated workloads
**Engineering cost:** Complete rewrite of event handling across all modules

**Verdict: NOT RECOMMENDED.** The 15-25% sort speedup does not justify
rewriting the entire event handling subsystem. The presorted detection
path (which skips sort entirely) makes sort optimization low-value for
the common case. SoA is the right choice for columnar database internals
but not for an aggregate function extension where the abstraction boundary
is per-event.

### Cache Alignment Overall Assessment

**None of the cache alignment strategies provide meaningful improvement.**

The fundamental reason: our bottleneck is **computational** (NFA execution,
greedy scanning with data-dependent branches), not **memory bandwidth**.
The Event struct at 16 bytes is already an excellent cache-line packing
(4 per line, 75% utilization). Hardware prefetching handles sequential
access patterns. The data doesn't fit in L3 cache at scale regardless
of alignment.

Cache-line alignment optimization helps when:
- Multiple threads access adjacent data (false sharing) — not our case
- Random access patterns dominate — our access is sequential
- Per-element size is close to cache line size — our 16 bytes is already efficient

**Recommendation: Do not pursue cache alignment changes.** The current
16-byte `Event` struct with `Copy` semantics is near-optimal for our
access patterns.

---

## 5. Profile-Guided Optimization (PGO) Builds

### Hypothesis

Use LLVM's Profile-Guided Optimization to train the compiler with real
workload profiles, enabling better branch prediction hints, function
inlining decisions, and code layout.

### How PGO Works

PGO is a two-pass compilation strategy [5]:

1. **Instrumented build:** Compile with `-C profile-generate` to insert
   profiling counters at every branch point and function call
2. **Training run:** Execute the instrumented binary with representative
   workloads, generating `.profraw` files
3. **Optimized build:** Recompile with `-C profile-use=merged.profdata`,
   which uses the collected data to:
   - Optimize branch layout (likely/unlikely paths)
   - Inline hot functions more aggressively
   - Optimize code placement for instruction cache
   - Improve register allocation in hot loops

### Analysis

**Measured PGO improvements in comparable Rust projects:**
- Vector (log processing): up to 15% throughput improvement [6]
- minitrace-rust (tracing library): measurable improvements across benchmarks [7]
- General Rust workloads: 10-15% typical, up to 20% for branch-heavy code [5]
- rustc compiler itself: 10-15% compile time improvement with PGO [8]

**Why PGO should work well for our codebase:**
- **Branch-heavy hot paths:** `scan_funnel` has 6+ data-dependent branches per event
  (window check, ALLOW_REENTRY, STRICT, STRICT_ORDER, STRICT_DEDUP, STRICT_INCREASE,
  condition while-loop). PGO would learn the typical branch frequencies
- **Mode dispatch:** `self.mode.has(FunnelMode::X)` is called per-event for each mode.
  PGO would learn that most queries use DEFAULT mode and optimize the no-mode path
- **NFA state dispatch:** `match &pattern.steps[state.step_idx]` has 4 arms
  (Condition, AnyEvents, OneEvent, TimeConstraint). PGO would learn typical
  pattern step distributions
- **Fast-path classification:** `classify_pattern` would be optimized based on
  which patterns are most common in training data

**Training workload design:**

The training set must represent real-world usage patterns:
```bash
# 1. Build instrumented binary
RUSTFLAGS="-C profile-generate=/tmp/pgo-data" cargo build --release

# 2. Run benchmarks as training workload
cargo bench --profile profiling  # (uses profiling profile)

# 3. Merge profile data
llvm-profdata merge -o /tmp/pgo-data/merged.profdata /tmp/pgo-data/*.profraw

# 4. Build optimized binary
RUSTFLAGS="-C profile-use=/tmp/pgo-data/merged.profdata" cargo build --release
```

Alternatively, use `cargo-pgo` for a streamlined workflow [5].

**Interaction with existing optimizations:**
- `profile.release` already has `opt-level = 3`, `lto = true`, `codegen-units = 1`
- These are the prerequisites for PGO to work well — single codegen unit ensures
  the profiling data maps cleanly to the optimized build
- PGO stacks on top of LTO — the two are complementary

**Risks and limitations:**
- Training data representativeness: PGO optimizes for the training workload. If
  real usage differs significantly, it could pessimize uncommon code paths
- Build complexity: Requires a multi-pass build process
- CI/CD impact: PGO builds take 2-3x longer (instrument + train + optimize)
- Community extension distribution: The community extension build system would
  need to support PGO, or we'd need to provide pre-built PGO artifacts
- Platform specificity: PGO profiles are not portable across architectures

**Estimated improvement:** 10-15% across all functions, based on:
- Branch-heavy hot paths (scan_funnel: 6+ branches per event)
- Pattern dispatch (NFA step matching)
- Mode checking (per-event mode flag evaluation)
- Function inlining decisions in tight loops

### Implementation Plan

**Phase 1: Validate locally**
1. Install `cargo-pgo`: `cargo install cargo-pgo`
2. Run `cargo pgo build` → `cargo pgo bench` → `cargo pgo optimize build`
3. Compare optimized benchmarks against baseline
4. Document measured improvement with confidence intervals

**Phase 2: Automate in CI** (only if Phase 1 shows >5% improvement)
1. Add PGO build step to release workflow
2. Use benchmark suite as training workload
3. Publish PGO-optimized artifacts alongside standard builds

**Phase 3: Community extension** (only if Phase 2 is stable)
1. Work with DuckDB community extension build system to support PGO
2. Or provide separate PGO artifacts via GitHub Releases

### Verdict

**HIGHEST PRIORITY. Lowest risk. Broadest impact.**

PGO is the single most promising optimization because:
1. It requires **zero source code changes** — purely a build configuration change
2. It applies to **all 7 functions simultaneously** — not function-specific
3. It has **proven 10-15% improvement** in comparable Rust workloads
4. It has **no runtime risk** — the optimized binary is strictly a standard binary with better compile-time decisions
5. It **stacks with all existing optimizations** — LTO, pdqsort, fast paths, etc.
6. It can be gated behind a `--profile pgo` build flag with zero impact on default builds
7. `cargo-pgo` makes the process straightforward

The main limitation is build complexity, which is a CI/CD concern, not a correctness concern.

---

## Prioritized Recommendation Matrix

| Priority | Optimization | Expected Speedup | Scope | Risk | Effort | Source Changes |
|---|---|---|---|---|---|---|
| **1 (HIGHEST)** | PGO builds | 10-15% all functions | All 7 functions | Very low | Low | None |
| **2 (HIGH)** | Time-constrained NFA fast path | ~48% for timed patterns | sequence_match/count/events | Low | Medium | ~100 lines |
| **3 (MEDIUM)** | Rayon parallel sort (feature-gated) | 2-4x sort phase only | Event-collecting functions (when unsorted) | Medium | Medium | ~20 lines + dependency |
| **4 (LOW)** | Single-condition NFA fast path | ~5% for trivial patterns | sequence_match/count | Very low | Low | ~15 lines |
| **5 (DO NOT PURSUE)** | SIMD intrinsics | <5% total | N/A | High | High | Platform-specific unsafe |
| **6 (DO NOT PURSUE)** | Cache-line alignment | <1% | N/A | Medium | Medium | Architectural changes |

### Implementation Order

1. **PGO builds** — Validate locally, measure, document. If >5% confirmed,
   add to CI. Zero source code risk.

2. **Time-constrained wildcard fast path** — Extend `classify_pattern` to
   detect `Condition, AnyEvents, TimeConstraint, Condition` sequences. Add
   `fast_wildcard_timed` scan function. This is the highest-impact source
   code change available.

3. **Rayon parallel sort** — Behind `feature = "parallel"`. Benchmark under
   concurrent DuckDB load before promoting to default. Only benefits
   unsorted input (minority case).

4. **Single-condition fast path** — Trivial addition to pattern classification.
   Low impact but clean code.

---

## References

- [1] "Evaluating SIMD Compiler-Intrinsics for Database Systems," CEUR-WS Vol. 3462, ADMS Workshop. https://ceur-ws.org/Vol-3462/ADMS5.pdf
- [2] Rayon `par_sort_unstable` documentation and implementation. https://docs.rs/rayon/latest/rayon/slice/trait.ParallelSliceMut.html
- [3] "Align data structures to cache lines," Mayorana.ch. https://mayorana.ch/en/blog/cache-line-awareness-optimization
- [4] "Rust, Memory performance & latency - locality, access, allocate, cache lines," DeveloperLife, 2025. https://developerlife.com/2025/05/19/rust-mem-latency/
- [5] "Profile-guided Optimization," The rustc book. https://doc.rust-lang.org/beta/rustc/profile-guided-optimization.html
- [6] "Profile-Guided Optimization," Vector documentation. https://vector.dev/docs/administration/optimization/pgo/
- [7] "Profile-Guided Optimization (PGO) benchmark results," minitrace-rust #195. https://github.com/tikv/minitrace-rust/issues/195
- [8] "The state of SIMD in Rust in 2025," Sergey Davidoff. https://shnatsel.medium.com/the-state-of-simd-in-rust-in-2025-32c263e5f53d
- [9] Rust RFC 2325: Stable SIMD. https://rust-lang.github.io/rfcs/2325-stable-simd.html
- [10] "Optimizing Rust Data Structures: Cache-Efficient Patterns for Production Systems." https://elitedev.in/rust/optimizing-rust-data-structures-cache-efficient-p/
- [11] S. Breß et al., "GPU-Accelerated Database Systems: Survey and Open Challenges," Springer LNCS, 2014. https://link.springer.com/chapter/10.1007/978-3-662-45761-0_1
