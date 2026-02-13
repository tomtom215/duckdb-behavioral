# CLAUDE.md — DuckDB Behavioral Analytics Extension

This file provides context for AI assistants working on this codebase.

## Table of Contents

- [Project Overview](#project-overview)
- [Architecture](#architecture)
  - [Key Design Decisions](#key-design-decisions)
- [Build & Test](#build--test)
- [Functions](#functions)
- [Dependencies](#dependencies)
- [Code Quality Standards](#code-quality-standards)
- [Performance](#performance)
  - [Session 1: Event Bitmask Optimization](#session-1-event-bitmask-optimization)
  - [Session 2: In-Place Combine + Sort Optimization](#session-2-in-place-combine--sort-optimization)
  - [Session 3: NFA Lazy Matching + 10M Benchmarks](#session-3-nfa-lazy-matching--10m-benchmarks)
  - [Session 4: Billion-Row Benchmarks + Test Hardening](#session-4-billion-row-benchmarks--test-hardening)
  - [Session 5: ClickHouse Feature Parity + sequenceMatchEvents](#session-5-clickhouse-feature-parity--sequencematchevents)
  - [Session 6: Mode FFI + 32-Condition Support + Presorted Detection](#session-6-mode-ffi--32-condition-support--presorted-detection)
  - [Session 7: Negative Results + Benchmark Validation + Testing](#session-7-negative-results--benchmark-validation--testing)
  - [Session 8: sequenceNextNode + Complete ClickHouse Parity](#session-8-sequencenextnode--complete-clickhouse-parity)
  - [Session 9: Rc\<str\> Optimization + Community Extension](#session-9-rcstr-optimization--community-extension)
  - [Session 10: E2E Validation + Custom C Entry Point](#session-10-e2e-validation--custom-c-entry-point)
  - [Session 11: NFA Fast Paths + Git Mining Demo](#session-11-nfa-fast-paths--git-mining-demo)
- [ClickHouse Parity Status](#clickhouse-parity-status)
- [Testing](#testing)
- [CI/CD](#cicd)
- [Common Tasks](#common-tasks)
  - [Adding a new function](#adding-a-new-function)
  - [Updating DuckDB version](#updating-duckdb-version)
  - [Performance optimization session](#performance-optimization-session)
- [Lessons Learned](#lessons-learned)

## Project Overview

`duckdb-behavioral` is a DuckDB loadable extension written in Rust that provides
behavioral analytics functions inspired by ClickHouse. It compiles to a `.so`/`.dylib`
that DuckDB loads at runtime via `LOAD 'behavioral'`.

## Architecture

```
src/
├── lib.rs                  # Custom C entry point (behavioral_init_c_api)
├── common/
│   ├── mod.rs
│   ├── event.rs            # Event type (u32 bitmask conditions, Copy) shared by window_funnel, sequence_*
│   └── timestamp.rs        # Interval-to-microseconds conversion
├── pattern/
│   ├── mod.rs
│   ├── parser.rs           # Recursive descent parser for sequence patterns
│   └── executor.rs         # NFA-based pattern matcher
├── sessionize.rs           # Sessionize state (boundary-tracking for segment trees)
├── retention.rs            # Retention state (bitmask-based)
├── window_funnel.rs        # Window funnel state (greedy forward scan, bitflag modes)
├── sequence.rs             # Sequence match/count/events state (wraps pattern engine)
├── sequence_next_node.rs   # Sequence next node state (sequential matching, Rc<str> values)
└── ffi/
    ├── mod.rs              # register_all_raw() — dispatches to all FFI modules
    ├── sessionize.rs       # FFI callbacks for sessionize
    ├── retention.rs        # FFI callbacks for retention
    ├── window_funnel.rs    # FFI callbacks for window_funnel
    ├── sequence.rs         # FFI callbacks for sequence_match + sequence_count
    ├── sequence_match_events.rs  # FFI callbacks for sequence_match_events
    └── sequence_next_node.rs     # FFI callbacks for sequence_next_node
```

### Key Design Decisions

1. **Pure Rust core + FFI bridge**: Business logic lives in top-level modules
   (`sessionize.rs`, `retention.rs`, etc.) with zero FFI dependencies. The `ffi/`
   submodules handle DuckDB C API registration only.

2. **Aggregate functions via raw C API**: DuckDB's Rust crate does not yet provide
   high-level aggregate function registration. We use `libduckdb-sys` directly with
   the five required callbacks: `state_size`, `init`, `update`, `combine`, `finalize`.

3. **Function sets for variadic signatures**: Since `duckdb_aggregate_function_set_varargs`
   doesn't exist, we register function sets with 31 overloads (2-32 boolean parameters)
   for retention, window_funnel, sequence_match, sequence_count,
   sequence_match_events, and sequence_next_node.

6. **Combinable `FunnelMode` bitflags**: `window_funnel` modes are represented as a
   `u8` bitflag struct (`FunnelMode(u8)`) rather than a mutually exclusive enum. This
   enables ClickHouse-compatible mode combinations (e.g., `strict | strict_increase`).
   Six modes are defined: `STRICT`, `STRICT_ORDER`, `STRICT_DEDUPLICATION`,
   `STRICT_INCREASE`, `STRICT_ONCE`, `ALLOW_REENTRY`.

4. **O(1) combine for sessionize**: The `SessionizeBoundaryState` tracks `first_ts`,
   `last_ts`, and `boundaries` count, enabling O(1) combine for DuckDB's segment
   tree windowing machinery.

5. **Custom C entry point**: `behavioral_init_c_api` bypasses `duckdb::Connection`
   entirely. It obtains `duckdb_connection` directly via `duckdb_connect` from the
   `duckdb_extension_access` struct, eliminating fragile struct layout assumptions.
   Only `libduckdb-sys` (with `loadable-extension` feature) is needed at runtime.

## Build & Test

```bash
# Build (dev)
cargo build

# Build (release, produces loadable .so/.dylib)
cargo build --release

# Run all unit tests (375 tests + 1 doc-test)
cargo test

# Run clippy (must produce zero warnings)
cargo clippy --all-targets

# Format check
cargo fmt -- --check

# Run benchmarks
cargo bench

# Build docs
cargo doc --no-deps

# E2E test against real DuckDB (requires duckdb CLI)
# 1. Build release
cargo build --release
# 2. Copy and append metadata
cp target/release/libduckdb_behavioral.so /tmp/behavioral.duckdb_extension
python3 extension-ci-tools/scripts/append_extension_metadata.py \
  -l /tmp/behavioral.duckdb_extension -n behavioral \
  -p linux_amd64 -dv v1.2.0 -ev v0.1.0 --abi-type C_STRUCT \
  -o /tmp/behavioral.duckdb_extension
# 3. Load and test
duckdb -unsigned -c "LOAD '/tmp/behavioral.duckdb_extension'; SELECT ..."
```

## Functions

| Function | Signature | Returns | Description |
|---|---|---|---|
| `sessionize` | `(TIMESTAMP, INTERVAL)` | `BIGINT` | Window function assigning session IDs |
| `retention` | `(BOOLEAN, BOOLEAN, ...)` | `BOOLEAN[]` | Cohort retention analysis |
| `window_funnel` | `(INTERVAL[, VARCHAR], TIMESTAMP, BOOLEAN, ...)` | `INTEGER` | Conversion funnel step tracking |
| `sequence_match` | `(VARCHAR, TIMESTAMP, BOOLEAN, ...)` | `BOOLEAN` | Pattern matching over events |
| `sequence_count` | `(VARCHAR, TIMESTAMP, BOOLEAN, ...)` | `BIGINT` | Count non-overlapping pattern matches |
| `sequence_match_events` | `(VARCHAR, TIMESTAMP, BOOLEAN, ...)` | `LIST(TIMESTAMP)` | Return matched condition timestamps |
| `sequence_next_node` | `(VARCHAR, VARCHAR, TIMESTAMP, VARCHAR, BOOLEAN, BOOLEAN, ...)` | `VARCHAR` | Next event value after pattern match |

## Dependencies

**Runtime** (linked into the `.so`/`.dylib`):
- `libduckdb-sys = "=1.4.4"` with `loadable-extension` feature — C API bindings
  for aggregate function registration. The `loadable-extension` feature provides
  runtime function pointer stubs via global atomic statics initialized by
  `duckdb_rs_extension_api_init`.

**Dev-only** (unit tests):
- `duckdb = "=1.4.4"` with `bundled` feature — Used in `#[cfg(test)]` modules
  for `Connection::open_in_memory()`. Not linked into the release extension.

## Code Quality Standards

- **Zero clippy warnings** with pedantic, nursery, and cargo lint groups enabled
- **403 unit tests** covering all functions, edge cases, combine associativity,
  property-based testing (proptest), mutation-testing-guided coverage, and
  ClickHouse mode combinations
- **1 doc-test** for the pattern parser
- **11 E2E tests** against real DuckDB v1.4.4 CLI covering all 7 functions
  with multiple scenarios (basic, timeout, modes, GROUP BY, no-match)
- **6 Criterion benchmark files** (sessionize, retention, window_funnel, sequence, sort,
  sequence_next_node) with combine benchmarks, realistic cardinality benchmarks, and
  throughput reporting up to 1B elements
- **Mutation testing**: 88.4% kill rate via cargo-mutants (130 caught / 17 missed)
- **MSRV 1.80** verified in CI
- All public items have documentation
- Release profile: LTO, single codegen unit, abort on panic, stripped symbols

## Performance

Performance engineering is documented in [`PERF.md`](PERF.md), which contains:

- **Benchmark methodology**: Criterion.rs configuration, reproduction instructions
- **Algorithmic complexity**: Formal O() analysis for all functions
- **Optimization history**: Every optimization with hypothesis, technique, measured
  before/after data with confidence intervals, and scaling analysis
- **Current baseline**: Post-optimization benchmark numbers that serve as the
  floor for future sessions
- **Session improvement protocol**: The exact workflow for ensuring measurable,
  auditable, session-over-session improvement

### Session 1: Event Bitmask Optimization

Event conditions are stored as a bitmask rather than `Vec<bool>`. This
eliminates per-event heap allocation and enables `Copy` semantics.
(Originally `u8`, expanded to `u32` in Session 6 for 32-condition support.)

- **Event struct**: 16 bytes (`i64` + `u32` + padding) with `Copy`, down from 32+ bytes
  + heap allocation
- **Condition check**: single bitwise AND (`O(1)`) instead of `Vec` indexing
- **Merge**: `memcpy` via `Copy` instead of per-element `clone()`

| Function | Input | Before | After | Speedup |
|---|---|---|---|---|
| `window_funnel` | 1k events | 18.6 µs | 1.65 µs | 11.3x |
| `window_funnel` | 100k events | 2.35 ms | 188 µs | 12.5x |
| `sequence_match` | 10k events | 260 µs | 28.0 µs | 9.3x |
| `sequence_count` | 10k events | 254 µs | 48.9 µs | 5.2x |

### Session 2: In-Place Combine + Sort Optimization

Replaced O(N²) merge-allocate combine with O(N) in-place extend. Added
`combine_in_place(&mut self)` that extends the existing Vec using amortized
O(1) appends. Also switched to unstable sort (pdqsort) for event sorting.

| Function | Input | Before | After | Speedup |
|---|---|---|---|---|
| `window_funnel_combine` | 100 states | 10.9 µs | 466 ns | 23x |
| `window_funnel_combine` | 1k states | 781 µs | 3.03 µs | 258x |
| `window_funnel_combine` | 10k states | 75.3 ms | 30.9 µs | **2,436x** |
| `sequence_combine` | 100 states | 12.1 µs | 1.31 µs | 9.2x |
| `sequence_combine` | 1k states | 756 µs | 11.0 µs | 68.8x |

Full optimization history with confidence intervals: [`PERF.md`](PERF.md)

### Session 3: NFA Lazy Matching + 10M Benchmarks

Fixed NFA `.*` exploration order in `src/pattern/executor.rs`: swapped LIFO stack
push order so the NFA tries advancing the pattern before consuming events (lazy
matching). This eliminated catastrophic O(n * MAX_NFA_STATES * starts) behavior.

| Function | Input | Before | After | Speedup |
|---|---|---|---|---|
| `sequence_match` | 100 events | 750 ns | 454 ns | 1.76x |
| `sequence_match` | 1M events | 4.43 s | 2.31 ms | **1,961x** |

Also expanded all benchmarks to 10M elements, added 22 mutation-killing boundary
tests (204 → 226), and added isolated sort benchmarks. No new bottlenecks found
at 10M scale — all functions scale as documented.

### Session 4: Billion-Row Benchmarks + Test Hardening

Expanded benchmarks to 100M for all event-collecting functions and 1B for O(1)-state
functions (sessionize, retention). Added 12 mutation-killing tests targeting NFA
executor gaps identified through systematic mutation analysis.

**Headline numbers (Criterion-validated, 3 runs, 95% CI):**

| Function | Scale | Wall Clock | Throughput |
|---|---|---|---|
| `sessionize_update` | **1 billion** | **1.21 s** | **824 Melem/s** |
| `retention_combine` | **1 billion** | **3.06 s** | **327 Melem/s** |
| `window_funnel` | 100 million | 898 ms | 111 Melem/s |
| `sequence_match` | 100 million | 1.08 s | 92.7 Melem/s |
| `sequence_count` | 100 million | 1.57 s | 63.5 Melem/s |

Test count increased from 226 to 238 (+12 targeted mutation-killing tests).

### Session 5: ClickHouse Feature Parity + `sequenceMatchEvents`

Architectural refactoring session focused on closing ClickHouse feature parity gaps.
No performance optimization targets — this session prioritized correctness and
feature completeness.

**Changes:**

1. **`FunnelMode` bitflags refactor**: Replaced mutually exclusive `enum FunnelMode`
   with `struct FunnelMode(u8)` bitflags, enabling ClickHouse-compatible mode
   combinations. Six independent modes: `STRICT` (0x01), `STRICT_ORDER` (0x02),
   `STRICT_DEDUPLICATION` (0x04), `STRICT_INCREASE` (0x08), `STRICT_ONCE` (0x10),
   `ALLOW_REENTRY` (0x20).

2. **Three new `window_funnel` modes**:
   - `strict_increase`: Requires strictly increasing timestamps between steps
   - `strict_once`: Each event can advance the funnel by at most one step
   - `allow_reentry`: Resets the funnel chain when condition 0 fires again

3. **Multi-step advancement per event**: Changed condition checking from `if` to
   `while` loop, allowing a single event to advance multiple funnel steps when it
   satisfies consecutive conditions. This matches ClickHouse default behavior.
   `STRICT_ONCE` breaks after one advancement per event.

4. **`sequence_match_events` function**: New aggregate function returning
   `LIST(TIMESTAMP)` — the timestamps of each matched `(?N)` step in the pattern.
   Empty list on no match. Implemented via `NfaStateWithTimestamps` in the NFA
   executor that collects timestamps during pattern matching.

5. **51 new tests** (238 → 289): 30 for `window_funnel` mode combinations, 7 for
   NFA executor event collection, 14 for `SequenceState::finalize_events`.

Test count increased from 238 to 289 (+51 tests).

### Session 6: Mode FFI + 32-Condition Support + Presorted Detection

Session focused on closing ClickHouse parity gaps and adding sort optimization.

**Changes:**

1. **`window_funnel` mode FFI exposure** (PARITY-1): Exposed all six `FunnelMode`
   flags via the FFI layer. Added VARCHAR parameter overloads accepting
   comma-separated mode strings (e.g., `'strict_increase, strict_once'`).
   Added `FunnelMode::parse_modes()` method. Backward-compatible: existing
   SQL without mode string continues to work.

2. **32-condition support** (PARITY-2): Expanded conditions bitmask from `u8`
   (8 conditions) to `u32` (32 conditions), matching ClickHouse's limit. Event
   struct stays at 16 bytes due to alignment padding. FFI registrations expanded
   from 7 overloads (2-8) to 31 overloads (2-32) per function.

3. **Presorted detection** (PERF-2): Added O(n) is_sorted check to `sort_events()`
   that skips O(n log n) pdqsort when events are already in timestamp order.
   Common case optimization for ORDER BY queries.

4. **20 new tests** (289 → 309): 10 for `parse_modes`, 5 for presorted detection,
   5 for 32-condition support.

Test count increased from 289 to 309 (+20 tests).

### Session 7: Negative Results + Benchmark Validation + Testing

Session focused on benchmark validation, performance experimentation, and test
hardening. Three performance optimization hypotheses were tested and all
produced negative results — documented honestly in PERF.md.

**Changes:**

1. **Benchmark baseline validation**: Ran `cargo bench` 3 times to establish
   Session 7 baseline. Retroactively validated Session 6's presorted detection
   claim (modest ~3-5% improvement, CIs overlap on some runs).

2. **PERF-1: LSD Radix Sort (NEGATIVE)**: Implemented 8-bit LSD radix sort
   for Event sorting. Measured 4.3x slower at 100M elements due to poor cache
   locality in scatter pattern for 16-byte elements. Reverted.

3. **PERF-3: Branchless Sessionize (NEGATIVE)**: Replaced branches with
   `i64::from(bool)`, `max`, `min`. Measured ~5-10% regression — branch predictor
   handles the 90/10 pattern efficiently. Reverted.

4. **PERF-4: Retention Update (NOT ATTEMPTED)**: Analysis showed benchmark
   bottleneck is `Vec<bool>` allocation, not the update loop.

5. **PARITY-3: `sequenceNextNode` Investigation**: Researched ClickHouse's
   `sequenceNextNode` function. Key finding: return type is always
   `Nullable(String)` (not polymorphic), which simplifies FFI. Documented
   design analysis below.

6. **15 new tests** (309 → 324): 10 proptest-based tests for 32-condition paths
   (retention: 4, window_funnel: 3, sequence: 3), 5 mutation-killing tests for
   event.rs (sort presorted check, from_bools, condition bit extraction, negative
   timestamps).

Test count increased from 309 to 324 (+15 tests).

### Session 8: sequenceNextNode + Complete ClickHouse Parity

Session focused on implementing the last remaining ClickHouse behavioral analytics
function, achieving COMPLETE feature parity.

**Changes:**

1. **`sequence_next_node` function** (PARITY-3): Implemented `sequenceNextNode`
   with full direction (forward/backward) and base (head/tail/first_match/last_match)
   support. Uses a dedicated `NextNodeEvent` struct with per-event `String` storage
   (separate from the `Copy` `Event` struct). Simple sequential matching algorithm.

2. **New FFI module** (`src/ffi/sequence_next_node.rs`): 32 function set overloads
   (1-32 event conditions) with VARCHAR return type. Shared `read_varchar` helper
   for DuckDB string extraction.

3. **New benchmark** (`benches/sequence_next_node_bench.rs`): Update + finalize
   throughput and combine benchmarks at multiple scales.

4. **42 new tests** (324 → 366): 39 unit tests covering direction/base parsing,
   all 8 direction×base combinations, multi-step patterns, combine correctness,
   NULL values, gap events, unsorted input, edge cases. 3 proptest-based tests
   for algebraic properties.

5. **Documentation**: New function page (`docs/src/functions/sequence-next-node.md`),
   updated ClickHouse compatibility matrix (all functions now "Complete").

Test count increased from 324 to 366 (+42 tests).

### Session 9: Rc\<str\> Optimization + Community Extension

Session focused on performance optimization of `sequence_next_node` and preparing
for DuckDB community extension submission.

**Changes:**

1. **Rc\<str\> optimization** (PERF-2): Replaced `Option<String>` with `Option<Rc<str>>`
   in `NextNodeEvent`, reducing struct size from 40 to 32 bytes and enabling O(1)
   clone via reference counting. Delivered 2.1-5.8x improvement across all scales,
   with combine operations improving 2.8-6.4x. Updated FFI layer to convert
   `read_varchar` results to `Rc<str>`.

2. **String pool attempt** (PERF-1, NEGATIVE): Tested `PooledEvent` (24 bytes, Copy)
   with separate `Vec<String>` pool. Measured 10-55% regression at most scales due to
   dual-vector overhead. Only 1M showed improvement (22%). Reverted and documented.

3. **Realistic cardinality benchmark**: Added `sequence_next_node_realistic` benchmark
   using a pool of 100 distinct `Rc<str>` values, matching typical behavioral analytics
   cardinality. Shows 35% improvement over unique strings at 1M events.

4. **Community extension infrastructure**: Added `Makefile` with configure/debug/release
   targets, `extension-ci-tools` git submodule, `description.yml` for community
   extension submission, and SQL integration tests in `test/sql/` covering all 7
   functions.

5. **9 new tests** (366 → 375): `Rc<str>` sharing verification, struct size assertion,
   combine reference sharing, empty/unicode/long string values, 32-condition
   interaction, three-state combine chain, mixed null/Rc values in combine.

Test count increased from 366 to 375 (+9 tests).

### Session 10: E2E Validation + Custom C Entry Point

Session focused on end-to-end validation against a real DuckDB instance. Discovered
and fixed three critical bugs that 375 passing unit tests completely missed.

**Bugs discovered and fixed:**

1. **SEGFAULT on extension load**: `extract_raw_connection` used incorrect pointer
   arithmetic through `Rc<RefCell<InnerConnection>>`. Replaced with a custom C entry
   point (`behavioral_init_c_api`) that obtains `duckdb_connection` via
   `duckdb_connect` directly from `duckdb_extension_access`.

2. **6 of 7 functions silently failed to register**: `duckdb_aggregate_function_set_name`
   must be called on EACH function added to a function set. Without this,
   `duckdb_register_aggregate_function_set` returns `DuckDBError` with no diagnostic.
   Only `sessionize` (registered as a single function, not a set) worked.

3. **`window_funnel` returned wrong results**: `combine_in_place` did not propagate
   `window_size_us` or `mode`. DuckDB's segment tree creates fresh zero-initialized
   target states and combines source states into them. At finalize time,
   `window_size_us=0` caused all events outside the entry timestamp to be excluded.

**Changes:**

1. **Custom C entry point** (`src/lib.rs`): Hand-written `behavioral_init_c_api` that
   calls `duckdb_rs_extension_api_init`, `duckdb_connect`, `register_all_raw`,
   `duckdb_disconnect`. Removes `duckdb` and `duckdb-loadable-macros` as runtime
   dependencies.

2. **Function set name fix** (`src/ffi/*.rs`): Added `duckdb_aggregate_function_set_name`
   call inside every function set registration loop across 5 FFI modules (retention,
   window_funnel, sequence, sequence_match_events, sequence_next_node).

3. **Combine propagation fix** (`src/window_funnel.rs`): `combine_in_place` now
   propagates `window_size_us` and `mode` from source states when the target has
   default values.

4. **Dependency simplification** (`Cargo.toml`): Runtime dependency reduced from 3
   crates (`duckdb`, `duckdb-loadable-macros`, `libduckdb-sys`) to 1
   (`libduckdb-sys` with `loadable-extension` feature).

5. **11 E2E tests**: All 7 functions validated with real SQL against DuckDB v1.4.4 CLI.

### Session 11: NFA Fast Paths + Git Mining Demo

Session focused on performance optimization of the NFA pattern executor and
developer experience (demo, SQL examples, combine test coverage).

**Performance optimizations (2 Criterion-validated):**

1. **NFA reusable stack** (`src/pattern/executor.rs`): Pre-allocate the NFA state
   Vec once in `execute_pattern` and reuse across all starting positions via
   `clear()`. Eliminates O(N) alloc/free pairs in `sequence_count`.
   `sequence_count` improved 33-47% across all scales (p=0.00).

2. **Fast-path linear scan** (`src/pattern/executor.rs`): Pattern classification
   dispatches common shapes to specialized O(n) scans:
   - `AdjacentConditions` (`(?1)(?2)`): sliding window scan
   - `WildcardSeparated` (`(?1).*(?2)`): single-pass step counter
   - `Complex`: falls back to NFA
   `sequence_count` improved 39-40% on top of the NFA reusable stack (p=0.00).
   Combined improvement: 56-61% at 100-1M scale.

3. **First-condition pre-check (NEGATIVE)**: Adding an early-exit branch for
   non-matching first conditions increased overhead. Reverted.

**Other changes:**

4. **21 combine propagation tests**: Systematic tests verifying DuckDB's
   zero-initialized target combine pattern works correctly for all 5 functions.
   Tests: sessionize (4), window_funnel (5), sequence (4),
   sequence_next_node (5), retention (3).

5. **7 fast-path tests**: Edge cases for adjacent skip correctness, three-step
   patterns, wildcard counting, insufficient events, and NFA fallback.

6. **Git mining SQL examples** (`test/sql/git_mining.test`): 7 SQL integration
   tests demonstrating all functions for git repository mining, each grounded
   in published academic research with citations.

7. **Interactive HTML demo** (`demo/index.html`): Tabbed interface showcasing
   all 7 functions with SQL examples and pre-computed results.

Test count increased from 375 to 403 (+28 tests).

## ClickHouse Parity Status

**COMPLETE** — All ClickHouse behavioral analytics functions are implemented
as of Session 8. There are no remaining feature gaps.

## Testing

Tests are organized as `#[cfg(test)] mod tests` within each module. Key test
categories:

- **State tests**: Empty state, single update, multiple updates
- **Edge cases**: Threshold boundaries, NULL handling, empty inputs
- **Combine correctness**: Empty combine, boundary detection, associativity
- **Property-based tests**: 26 proptest-based tests verifying algebraic properties
  (combine associativity, commutativity, identity element, idempotency, monotonicity)
  including 10 tests exercising 32-condition paths (conditions 9-32)
- **Mutation-testing-guided tests**: 51 tests added from cargo-mutants analysis to
  kill surviving mutants (combine_in_place, OR vs XOR, strict mode edge cases,
  boundary conditions, condition index limits, timestamp extremes, NFA executor
  time constraint propagation, lazy matching verification, sort presorted check,
  from_bools accumulation, condition bit extraction)
- **Pattern parser**: All operators, error positions, whitespace tolerance
- **NFA executor**: Match/no-match, wildcards, time constraints, counting, event
  collection (`execute_pattern_events`)
- **`FunnelMode` tests**: Bitflag operations, mode parsing, display, all six modes
  individually, mode combinations (strict+strict_increase, strict_order+strict_increase,
  strict_dedup+strict_increase, strict_once+strict_increase, allow_reentry+strict_order,
  all modes combined)
- **`sequence_match_events` tests**: Basic matching, multi-step, gap events, no-match,
  single-event-match, complex patterns, interaction with `finalize_events`
- **`sequence_next_node` tests**: Direction/base parsing, forward/backward matching,
  head/tail/first_match/last_match combinations, multi-step patterns, combine
  correctness, NULL value handling, gap events, presorted detection, proptest,
  Rc\<str\> sharing verification, struct size assertions, unicode/long strings

Run with `cargo test`. All 403 tests + 1 doc-test run in <1 second.

**E2E tests** (manual, against real DuckDB CLI):
- 11 test cases covering all 7 functions
- Tests: basic usage, timeouts, modes, GROUP BY, no-match, per-user aggregation
- Requires: `cargo build --release`, metadata append, `duckdb -unsigned`
- Validates the complete chain: extension load → function registration → SQL execution → correct results

## CI/CD

GitHub Actions workflows in `.github/workflows/`:

- **ci.yml**: check, test, clippy, fmt, doc, MSRV, bench-compile, deny, coverage,
  cross-platform (Linux + macOS)
- **release.yml**: Builds release artifacts for x86_64 and aarch64 on Linux/macOS,
  creates GitHub release on tag push

## Common Tasks

### Adding a new function

1. Create `src/new_function.rs` with state struct, `new()`, `update()`, `combine()`, `finalize()`
2. Create `src/ffi/new_function.rs` with FFI callbacks
   - **CRITICAL**: If using function sets, call `duckdb_aggregate_function_set_name` on EACH function
   - **CRITICAL**: `combine_in_place` must propagate ALL configuration fields (not just events)
3. Register in `src/ffi/mod.rs` `register_all_raw()`
4. Add `pub mod new_function;` to `src/lib.rs`
5. Add benchmark in `benches/`
6. Write unit tests in the module
7. **E2E test**: Build release, load in DuckDB CLI, verify SQL produces correct results

### Updating DuckDB version

The custom C entry point uses `libduckdb-sys` directly. When updating:

1. Update `libduckdb-sys` version in `[dependencies]` and `duckdb` in `[dev-dependencies]`
2. Check if `duckdb_rs_extension_api_init` minimum version string needs updating
   (currently `"v1.2.0"` — find the new C API version from `duckdb-loadable-macros`)
3. Update `append_extension_metadata.py -dv` to match the new C API version
4. Run full unit test suite (`cargo test`)
5. **MANDATORY**: E2E test — build release, load in DuckDB CLI, verify all 7 functions

### Performance optimization session

Follow the Session Improvement Protocol in [`PERF.md`](PERF.md). Summary:

1. Run `cargo bench` 3 times. Record baseline. Compare against PERF.md "Current Baseline".
2. One optimization per commit. Measure before/after with confidence intervals.
3. Accept only when CIs do not overlap. Revert and document if not significant.
4. Update PERF.md: add optimization history entry, update current baseline.
5. Update test count in this file if tests were added or removed.

## Lessons Learned

Accumulated from past sessions. Consult before beginning any work.

1. **Retention finalize overflow**: `1u32 << i` panics when `i >= 32`. Always test
   at type boundaries (`u8::MAX`, `u32::MAX`, `i32::MIN`, etc.).

2. **cargo-deny config format**: Pin tool versions. Use `deny.toml` v2 format with
   `[graph]` section.

3. **Transitive dependency licenses**: Run `cargo deny check licenses` locally after
   any dependency change. `CC0-1.0`, `CDLA-Permissive-2.0` from transitive deps
   must be in the allow list.

4. **Unsafe code confinement**: All unsafe lives in `src/ffi/`. Business logic is
   100% safe Rust. Every unsafe block has `// SAFETY:` documentation.

5. **Raw connection extraction was fatally fragile (REMOVED in Session 10)**:
   `extract_raw_connection` depended on `duckdb` internal struct layout and
   caused SEGFAULTs. Replaced with custom C entry point using `duckdb_connect`
   directly. See lesson 52.

6. **Benchmark stability**: Run Criterion benchmarks 3+ times before comparing.
   Single-run comparisons are invalid. Report confidence intervals.

7. **Function set overloads**: 31 overloads (2-32 boolean parameters) per function
   because `duckdb_aggregate_function_set_varargs` does not exist.

8. **Test organization**: Group by category — state tests, edge cases, combine
   correctness, overflow/boundary. Makes gaps visible.

9. **Combine is the dominant cost**: In DuckDB's segment tree windowing, combine
   is called O(n log n) times. The combine implementation IS the bottleneck for
   event-collecting functions. Replacing O(n+m) merge-alloc with O(m) in-place
   extend yielded 2,436x improvement at scale. Always profile combine first.

10. **Deferred sorting**: If finalize re-sorts events anyway, maintaining sorted
    order in combine is wasted work. Append-only combine + sort-in-finalize is
    strictly better than merge-in-combine + sort-in-finalize.

11. **In-place vs returning-new**: `combine(&self, other: &Self) -> Self` creates
    a new Vec every time. `combine_in_place(&mut self, other: &Self)` reuses the
    existing Vec with amortized O(1) append. For left-fold chains, the difference
    is O(N²) vs O(N) total copies. Always provide an in-place variant.

12. **Unstable sort for Copy types**: When element ordering among equal keys has no
    defined semantics, prefer `sort_unstable_by_key` (pdqsort). Eliminates O(n)
    auxiliary memory for Copy types and provides better constant factors.

13. **Mutation testing reveals test gaps**: cargo-mutants found 17 surviving mutants
    including combine_in_place (untested), OR vs XOR in retention combine (both pass
    when conditions don't overlap), and first_ts/last_ts tracking comparisons.
    Run mutation testing after adding new public methods.

14. **Property-based testing catches algebraic violations**: proptest verified
    combine associativity, identity, and commutativity across randomized inputs.
    Essential for aggregate functions used in DuckDB's segment trees where these
    properties are required for correctness.

15. **Session startup protocol**: Read PERF.md baseline first. Run benchmarks 3x
    to establish session baseline BEFORE any changes. Compare against documented
    CIs. This catches environment-specific differences early (different machines
    produce different absolute numbers but same relative improvements).

16. **Optimization priority by measured data**: The combine operations were orders
    of magnitude more expensive than any other operation (75ms vs 200µs). Always
    target the largest measured cost first. Per-element analysis (cost/N) reveals
    algorithmic complexity in practice.

17. **NFA exploration order matters catastrophically**: In a LIFO-based NFA
    backtracking engine, the push order for `AnyEvents` (.*) determines lazy vs
    greedy semantics. Greedy (consume event first) causes O(n²) at scale because
    the NFA consumes all events before trying to advance the pattern. Lazy (advance
    step first) is O(n * pattern_len). This produced a 1,961x speedup at 1M events.
    Always verify NFA semantics match the specification (ClickHouse uses lazy).

18. **10M benchmarks expose DRAM bandwidth limits**: At 10M events (160MB working
    set), throughput drops from ~800 Melem/s to ~42-96 Melem/s as the working set
    exceeds L3 cache. Per-element cost analysis at this scale reveals the true
    constant factors hidden by cache effects at smaller sizes.

19. **Negative results are results**: Document optimizations with overlapping CIs
    honestly. The compiled pattern preservation showed ~2.5% improvement but CIs
    overlapped — committed as code quality but NOT counted as performance gain.
    This prevents accumulation of unjustified performance claims.

20. **Large-scale benchmarks find hidden bugs**: The 1M benchmark expansion directly
    exposed the NFA O(n²) bug that was invisible at 10K events (30µs vs expected
    ~17µs — easy to dismiss as noise). At 1M events, the 4.4 second runtime was
    unmistakable. Always benchmark at scales that exceed cache hierarchies.

21. **O(1)-state functions scale to 1B without memory constraints**: Functions like
    sessionize (O(1) state) and retention (O(1) combine, 4 bytes/state) can be
    benchmarked at 1B elements because they don't store event data. Event-collecting
    functions (window_funnel, sequence) are limited by available RAM since they store
    16 bytes per event. On a 21GB system, 100M events (1.6GB) is the practical maximum
    for Criterion benchmarks that clone the data per iteration.

22. **Systematic mutation analysis reveals NFA executor gaps**: Automated search for
    mutation opportunities (operator swaps, branch removal, return value changes)
    found that NFA executor time constraint propagation through `.` (OneEvent) and
    `.*` (AnyEvents) steps lacked coverage. Tests for individual features existed
    but not for their composition. Always test feature interactions, not just
    individual features.

23. **Throughput stabilizes at DRAM bandwidth for O(1) operations**: Sessionize
    maintains ~824 Melem/s from 100K to 1B elements (1.21 ns/element). This is
    consistent with the operation being compute-bound on a tight compare + branch
    + increment loop that fits entirely in registers, with no data-dependent memory
    access pattern. The ~3 cycles per element matches the expected cost of a
    conditional branch + integer add on modern x86.

24. **Bitflags beat enums for combinable modes**: ClickHouse's `window_funnel` modes
    are independently composable (e.g., `strict | strict_increase`). An enum forces
    mutually exclusive variants, requiring N! combinations as separate variants.
    A bitflag struct `FunnelMode(u8)` with `has()`, `with()` methods enables O(1)
    mode checks via bitwise AND while supporting arbitrary combinations. The
    `parse_mode_str()` method handles comma-separated string input from SQL.

25. **Multi-step advancement per event**: ClickHouse's `window_funnel` allows a
    single event to advance multiple funnel steps when it satisfies consecutive
    conditions. Using `if event.condition(step)` limits to one step per event;
    `while event.condition(step)` enables multi-step. The `strict_once` mode then
    naturally constrains this by breaking after one advancement. Always verify
    ClickHouse semantics with the source code, not just the documentation.

26. **Event filtering interacts with test design**: The `update()` method filters
    events where `has_any_condition()` is false to avoid storing irrelevant data.
    Test "gap" events (intended to test non-matching intermediate events) must have
    at least one true condition to pass the filter, or they will be silently dropped.
    Design tests with awareness of upstream filters.

27. **NFA timestamp collection requires separate execution path**: Extracting
    matched condition timestamps during NFA execution requires tracking per-state
    `collected: Vec<i64>` alongside the standard `event_idx`/`step_idx`. This
    cannot be bolted onto the existing `execute_pattern`/`count_pattern` without
    breaking their performance characteristics. A separate `execute_pattern_events`
    function with `NfaStateWithTimestamps` keeps the hot paths clean.

28. **Presorted detection saves O(n log n) for ordered input**: DuckDB often
    provides events in timestamp order via ORDER BY. An explicit `is_sorted`
    check with `Iterator::windows(2)` and early termination on first violation
    is cheaper than pdqsort's adaptive path for the fully-sorted case. The O(n)
    overhead for unsorted input is negligible before O(n log n) sort.

29. **u8 → u32 bitmask is free due to alignment**: Expanding the Event conditions
    bitmask from `u8` to `u32` does not change the struct size: `i64` (8 bytes)
    requires 8-byte alignment, so the struct is padded to 16 bytes regardless of
    whether the second field is 1 byte or 4 bytes. This means 32-condition support
    is a zero-cost upgrade from 8-condition support.

30. **Comma-separated mode parsing needs careful whitespace handling**: When
    parsing mode strings like `"strict_increase, strict_once"`, each token must
    be trimmed independently. Empty tokens (from trailing commas or double commas)
    should be silently skipped rather than causing errors. The `parse_modes()`
    method handles all edge cases: empty string, whitespace-only, trailing comma,
    duplicate modes (idempotent via OR).

31. **LSD radix sort is catastrophically slow for embedded-key structs**: For
    sorting 16-byte Event structs by their embedded i64 key, LSD radix sort
    (8-bit radix, 8 passes) was 4.3x slower than pdqsort at 100M elements.
    The scatter pattern (random writes to 256 buckets) has terrible cache/TLB
    locality for large elements. Radix sort wins only when sorting small keys
    (4-8 bytes) separate from large payloads. For embedded keys, comparison-based
    sorts with good spatial locality (pdqsort, introsort) are superior.

32. **Branchless is not always better**: Replacing well-predicted branches
    (90/10 or better split) with branchless operations (`i64::from(bool)`, `max`,
    `min`) can ADD overhead. The CMOV instruction has fixed latency and always
    executes both paths, while a correctly predicted branch has zero-cost
    continuation. Only attempt branchless optimization when branch misprediction
    rate exceeds ~5% (random data, 50/50 splits). For periodic or monotonic
    patterns, the branch predictor achieves near-perfect accuracy.

33. **Negative results are essential for future sessions**: Session 7 tested 3
    performance hypotheses and all were negative. This is valuable because it
    establishes that the current implementation is already well-optimized: pdqsort
    is the right choice for Event sorting, branches are correct for Sessionize,
    and the retention bottleneck is allocation not computation. Future sessions
    should not re-attempt these optimizations.

34. **`sequenceNextNode` return type is NOT polymorphic**: ClickHouse's
    `sequenceNextNode` always returns `Nullable(String)`, contrary to the original
    assumption that it would require polymorphic return types. The `event_column`
    parameter only accepts `String` or `Nullable(String)`. This simplifies FFI
    registration to a single VARCHAR return type. Always verify ClickHouse
    semantics from the source/docs, not from assumptions.

35. **Separate event struct for String-carrying functions**: `sequence_next_node`
    requires storing a `String` value per event, which cannot be added to the
    existing `Copy` `Event` struct without breaking all other functions'
    performance characteristics. A dedicated `NextNodeEvent` struct keeps the
    hot paths clean for the six existing functions while supporting the per-event
    String storage needed by `sequence_next_node`. Always use separate types
    when new requirements break existing performance invariants.

36. **Store all events for next-node functions**: Unlike `sequence_match` which
    filters events with `has_any_condition()`, `sequence_next_node` must store
    ALL events regardless of conditions because any event could be the "next
    node" whose value is returned. Different filtering requirements demand
    different `update()` implementations.

37. **String allocation dominates sequence_next_node cost**: At 10K events,
    `sequence_next_node` is 31x slower per element than `sequence_match` (53
    ns/element vs 1.7 ns/element). The dominant costs are: per-event heap
    allocation for `Option<String>`, larger struct size (~80+ bytes vs 16 bytes)
    reducing cache utilization, and deep String cloning in combine operations.
    Future optimization should target string interning, arena allocation, or
    SmallString optimization for short event values (<24 bytes).

38. **Non-monotonic throughput at large scales**: `sequence_next_node` shows
    6.58 Melem/s at 1M but 8.12 Melem/s at 10M — a counterintuitive improvement.
    This is measurement noise: at 10M with only 10 Criterion samples, the CIs are
    wide ([1.15s, 1.30s]). The underlying trend is decreasing throughput with
    scale as expected from cache hierarchy effects. Wide CIs at the largest scale
    should be flagged but not over-interpreted.

39. **Community extension path for Rust**: DuckDB community extensions support
    two paths for Rust: (a) CMake + Corrosion + C++ glue (`build: cmake`,
    `requires_toolchains: rust`) — well-tested, 5+ examples; (b) Pure Rust via
    `extension-template-rs` (`build: cargo`) — experimental, only `rusty_quack`
    example. Our extension's `duckdb_entrypoint_c_api` macro aligns with path (b).
    Key requirement: add a `Makefile` with configure/debug/release targets and a
    metadata appending script. Platform exclusions needed for wasm and some Windows
    targets.

40. **Community extension `description.yml` is the only submission artifact**:
    Extension source stays in our repo. The PR to `duckdb/community-extensions`
    adds only `extensions/behavioral/description.yml`. The CI builds for all
    platforms automatically. No code review of extension source — only build
    success and metadata correctness. The `ref` field pins to a specific commit
    hash for reproducibility.

41. **DuckDB version coupling risk eliminated by custom entry point**: The
    Session 10 custom C entry point (`behavioral_init_c_api`) removed the
    `duckdb = "=1.4.4"` pin dependency for connection extraction. The only
    remaining version coupling is `libduckdb-sys = "=1.4.4"` for C API struct
    definitions, which is stable across DuckDB releases. The community extension
    system supports `ref` (stable) and `ref_next` (development) fields to manage
    version-specific builds.

42. **Benchmark data generation cost matters**: The `sequence_next_node` benchmark
    uses `format!("page_{i}")` to generate per-event String values. At 10M events,
    this generates 10M unique strings of ~12-18 characters each. In production,
    event values have much lower cardinality (few hundred distinct page names).
    The benchmark measures worst-case allocation behavior, not typical usage.
    Session 9 added a "realistic cardinality" benchmark variant that reuses a
    pool of ~100 distinct `Rc<str>` values, showing 35% improvement at 1M scale.

43. **Rc\<str\> beats String for shared immutable event values**: Replacing
    `Option<String>` with `Option<Rc<str>>` in `NextNodeEvent` delivered 2.1-5.8x
    improvement with zero functionality change. The key insight: event values in
    behavioral analytics are read-only after creation, making reference counting
    ideal. `Rc::clone` is a single atomic increment (~1ns) vs `String::clone`
    which copies len bytes (~20-80ns). The improvement compounds in combine
    operations where every element is cloned. Always prefer `Rc<str>` over
    `String` for immutable shared data in aggregate state.

44. **String pool with separate index vectors is a pessimization**: Separating
    string storage into `Vec<String>` pool + `u32` indices in a `Copy` struct
    sounds optimal (24-byte Copy events!) but measured 10-55% slower at most
    scales. The dual-vector overhead (two separate allocations, two resize
    paths, pool cloning in combine) outweighs the per-element size reduction.
    Only the 1M scale showed improvement (22%) where cache line utilization
    dominated. Lesson: measure before committing to "obviously better" data
    structures. Simple reference counting (Rc) beat the clever pool design.

45. **Struct size reduction has diminishing returns below cache line**: Reducing
    `NextNodeEvent` from 40 bytes (String) to 32 bytes (Rc\<str\>) improved
    throughput by 2.1-5.8x, but most of the gain came from O(1) clone, not
    size reduction. The further reduction to 24 bytes (PooledEvent with u32
    index) was a net negative. Below one cache line (64 bytes), the access
    pattern and clone cost matter more than the absolute struct size.

46. **Realistic cardinality benchmarks reveal Rc sharing benefits**: With 100
    distinct `Rc<str>` values (typical for page names), `Rc::clone` from the
    pool avoids even the initial `Rc::from()` allocation. At 1M events this
    provides 35% additional improvement over unique-string benchmarks. Always
    include a realistic-cardinality benchmark alongside worst-case (unique
    values) to understand production performance characteristics.

47. **Community extension submodule is the critical dependency**: The
    `extension-ci-tools` git submodule provides the Makefile includes, Python
    metadata appending script, and test runner. Without it, the Makefile
    targets (`configure`, `debug`, `release`, `test`) cannot function. The
    submodule must be initialized before any `make` invocation. Pin to a
    known-working commit for reproducibility.

48. **SQL integration tests use SQLLogicTest format**: DuckDB's test runner
    expects `.test` files in `test/sql/` using SQLLogicTest format: `statement ok`,
    `query <type_codes>`, `----` result separator, `NULL` for null values.
    Column type codes: `I` = integer, `T` = text, `R` = real. The `require
    behavioral` directive loads the extension before test execution.

49. **E2E testing against real DuckDB is non-negotiable**: Unit tests alone missed
    3 critical bugs: SEGFAULT on load, 6 functions failing to register, and
    `window_funnel` returning wrong results. All 403 unit tests passed while the
    extension was completely broken in production. Unit tests validate business
    logic in isolation; E2E tests validate the FFI boundary, DuckDB's data chunk
    format, state lifecycle management, and the extension loading mechanism. Both
    test levels are mandatory — never ship without E2E validation.

50. **`duckdb_aggregate_function_set_name` must be called on EACH function in a
    set**: DuckDB's `duckdb_register_aggregate_function_set` silently fails if
    individual functions in the set don't have their name set via
    `duckdb_aggregate_function_set_name`. The name must be set on both the set
    (via `duckdb_create_aggregate_function_set`) AND each member function. This is
    not documented in the C API — discovered by reading DuckDB's own test code
    (`test/api/capi/test_capi_aggregate_functions.cpp`). When debugging function
    registration, test with a single function first (via
    `duckdb_register_aggregate_function`), then progressively add function set
    complexity.

51. **DuckDB combine creates fresh zero-initialized target states**: DuckDB's
    segment tree calls `state_init` to create new target states, then combines
    source states into them. This means `combine_in_place` must propagate ALL
    configuration fields — not just events/data. Fields that default to zero or
    empty will remain zero at finalize time unless explicitly copied. Affected
    fields: `window_size_us`, `mode` (window_funnel), `pattern_str` (sequence),
    `direction`, `base`, `num_steps` (sequence_next_node). The `sessionize`
    function avoids this because its `combine()` returns a new `Self` and
    handles the empty case via `(None, _) => other.clone()`.

52. **Custom C entry point eliminates struct layout fragility**: The hand-written
    `behavioral_init_c_api` uses `duckdb_connect` directly from
    `duckdb_extension_access.get_database`, bypassing `duckdb::Connection`
    entirely. This eliminates `extract_raw_connection` which depended on internal
    `Rc<RefCell<InnerConnection>>` layout — a version-pinned, architecture-specific
    hack that caused SEGFAULTs. The custom entry point requires only `libduckdb-sys`
    (not `duckdb` or `duckdb-loadable-macros`) at runtime, reducing the dependency
    surface and eliminating the DuckDB version coupling risk documented in lesson 41.

53. **Extension metadata version is C API version, not DuckDB version**: DuckDB
    v1.4.4 uses C API version `v1.2.0`. The `append_extension_metadata.py -dv`
    flag must use the C API version. Using the DuckDB release version (`-dv v1.4.4`)
    causes: "file was built for DuckDB C API version 'v1.4.4' but we can only load
    extensions built for DuckDB C API 'v1.2.0'". The C API version can be found in
    `duckdb-loadable-macros` source (`DEFAULT_DUCKDB_VERSION = "v1.2.0"`) or by
    checking what version string DuckDB reports in the error message.

54. **`libduckdb-sys` `loadable-extension` feature is sufficient for runtime**:
    This feature provides runtime function pointer stubs via global atomic statics,
    initialized by `duckdb_rs_extension_api_init`. Neither `duckdb` nor
    `duckdb-loadable-macros` are needed as runtime dependencies. `duckdb` remains
    as a dev-dependency with `bundled` feature for unit tests that use
    `Connection::open_in_memory`. This reduces the dependency tree by ~6 crates
    (darling, darling_core, darling_macro, ident_case, strsim, duckdb-loadable-macros).

55. **Function set registration failure is silent**: `duckdb_register_aggregate_function_set`
    returns `DuckDBError` but produces no diagnostic output explaining WHY it failed.
    The extension's own `eprintln` was the only indication. Debug strategy: (1) test
    with `duckdb_register_aggregate_function` for a single function first, (2) test
    with a function set containing one overload, (3) check DuckDB's test code for
    correct API usage patterns. The `duckdb_functions()` system table is invaluable
    for verifying which functions actually registered.

56. **Unit tests and E2E tests serve fundamentally different purposes**: 375 unit
    tests validated business logic: combine associativity, mode parsing, NFA pattern
    matching, edge cases. But they operate on Rust structs in isolation — they never
    exercise the FFI boundary, DuckDB's data chunk format, DuckDB's aggregate state
    lifecycle (init→update→combine→finalize→destroy), or extension loading. All 3
    bugs found in Session 10 were in the FFI/integration layer. The test pyramid
    must include both levels.

57. **Debug with `eprintln` in FFI callbacks, then remove**: When debugging E2E
    failures, adding `eprintln!("behavioral: DEBUG ...")` in FFI callbacks (update,
    combine, finalize) is the fastest way to understand what DuckDB is actually
    passing to the extension. DuckDB's stderr output is visible in the CLI. Always
    remove debug output before committing — it causes measurable performance
    degradation at scale and pollutes user-visible output.

58. **Reuse heap allocations across loop iterations**: When a function is called
    repeatedly in a loop (e.g., `try_match_from` called from every starting
    position in `execute_pattern`), pre-allocate any internal Vecs outside the
    loop and pass them by `&mut` reference. Use `clear()` to reset without
    freeing. This converts O(N) alloc/free pairs to O(1) total allocations.
    For `sequence_count`, this single change delivered 33-47% improvement.

59. **Pattern-directed specialization eliminates NFA overhead for common cases**:
    Most behavioral analytics patterns are simple chains of conditions with
    optional `.*` wildcards (e.g., `(?1).*(?2).*(?3)`). These can be executed
    with a single-pass O(n) linear scan instead of the full NFA backtracking
    engine. Classifying patterns at execution time and dispatching to specialized
    code paths delivered 39-40% additional improvement for `sequence_count`.
    The key insight: the NFA is a general solution, but the common case doesn't
    need generality.

60. **First-condition pre-checks add overhead in tight loops**: Adding an
    `if let Some(idx) = first_cond` check before each NFA invocation to skip
    non-matching starting positions caused 0-8% regression. The NFA already
    handles non-matching starts efficiently (push + pop + condition check =
    ~5ns). The extra branch in the hot loop path costs more than it saves.
    When the operation being skipped is already very cheap, pre-checking is
    a pessimization.

61. **Adjacent-condition fast paths must not skip-ahead on intermediate
    failures**: For pattern `(?1)(?2)`, when checking events[i..i+k], if
    events[i+j] fails (j > 0), advancing to i+j+1 is WRONG because positions
    i+1 through i+j could be valid starting positions. The NFA handles this
    correctly by always advancing by 1 on failure. The fast path must do
    the same. This bug was caught by adding a targeted regression test.
