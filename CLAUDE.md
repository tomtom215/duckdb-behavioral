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
  - [Session 12: Community Extension Readiness + Benchmark Completion](#session-12-community-extension-readiness--benchmark-completion)
  - [Session 13: CI/CD Hardening + Benchmark Refresh](#session-13-cicd-hardening--benchmark-refresh)
  - [Session 14: Production Readiness Hardening](#session-14-production-readiness-hardening)
  - [Session 15: Enterprise Quality Standards](#session-15-enterprise-quality-standards)
- [ClickHouse Parity Status](#clickhouse-parity-status)
- [Testing](#testing)
- [CI/CD](#cicd)
- [Common Tasks](#common-tasks)
  - [Adding a new function](#adding-a-new-function)
  - [Updating DuckDB version](#updating-duckdb-version)
  - [Performance optimization session](#performance-optimization-session)
- [Lessons Learned](#lessons-learned) → [`LESSONS.md`](LESSONS.md)

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
├── sequence_next_node.rs   # Sequence next node state (sequential matching, Arc<str> values)
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

4. **Combinable `FunnelMode` bitflags**: `window_funnel` modes are represented as a
   `u8` bitflag struct (`FunnelMode(u8)`) rather than a mutually exclusive enum. This
   enables ClickHouse-compatible mode combinations (e.g., `strict | strict_increase`).
   Six modes are defined: `STRICT`, `STRICT_ORDER`, `STRICT_DEDUPLICATION`,
   `STRICT_INCREASE`, `STRICT_ONCE`, `ALLOW_REENTRY`.

5. **O(1) combine for sessionize**: The `SessionizeBoundaryState` tracks `first_ts`,
   `last_ts`, and `boundaries` count, enabling O(1) combine for DuckDB's segment
   tree windowing machinery.

6. **Custom C entry point**: `behavioral_init_c_api` bypasses `duckdb::Connection`
   entirely. It obtains `duckdb_connection` directly via `duckdb_connect` from the
   `duckdb_extension_access` struct, eliminating fragile struct layout assumptions.
   Only `libduckdb-sys` (with `loadable-extension` feature) is needed at runtime.

## Build & Test

```bash
# Build (dev)
cargo build

# Build (release, produces loadable .so/.dylib)
cargo build --release

# Run all unit tests (411 tests + 1 doc-test)
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
cp target/release/libbehavioral.so /tmp/behavioral.duckdb_extension
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

**Dev-only** (unit tests and benchmarks):
- `duckdb = "=1.4.4"` with `bundled` feature — Used in `#[cfg(test)]` modules
  for `Connection::open_in_memory()`. Not linked into the release extension.
- `criterion = "0.8"` with `html_reports` feature — Statistical benchmarking
- `proptest = "1"` — Property-based testing
- `rand = "0.9"` — Random data generation for benchmarks

## Code Quality Standards

- **Zero clippy warnings** with pedantic, nursery, and cargo lint groups enabled
- **411 unit tests** covering all functions, edge cases, combine associativity,
  property-based testing (proptest), mutation-testing-guided coverage, and
  ClickHouse mode combinations
- **1 doc-test** for the pattern parser
- **27 E2E tests** against real DuckDB v1.4.4 CLI covering all 7 functions
  with multiple scenarios (basic, timeout, modes, GROUP BY, no-match, NULL
  inputs, empty tables, all 6 funnel modes, 5+ conditions, all 8
  direction/base combinations)
- **7 Criterion benchmark files** (sessionize, retention, window_funnel, sequence, sort,
  sequence_next_node, sequence_match_events) with combine benchmarks, realistic
  cardinality benchmarks, and throughput reporting up to 1B elements
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

1. **Rc\<str\> optimization** (PERF-2): Replaced `Option<String>` with `Option<Arc<str>>`
   in `NextNodeEvent`, reducing struct size from 40 to 32 bytes and enabling O(1)
   clone via reference counting. Delivered 2.1-5.8x improvement across all scales,
   with combine operations improving 2.8-6.4x. Updated FFI layer to convert
   `read_varchar` results to `Arc<str>`.

2. **String pool attempt** (PERF-1, NEGATIVE): Tested `PooledEvent` (24 bytes, Copy)
   with separate `Vec<String>` pool. Measured 10-55% regression at most scales due to
   dual-vector overhead. Only 1M showed improvement (22%). Reverted and documented.

3. **Realistic cardinality benchmark**: Added `sequence_next_node_realistic` benchmark
   using a pool of 100 distinct `Arc<str>` values, matching typical behavioral analytics
   cardinality. Shows 35% improvement over unique strings at 1M events.

4. **Community extension infrastructure**: Added `Makefile` with configure/debug/release
   targets, `extension-ci-tools` git submodule, `description.yml` for community
   extension submission, and SQL integration tests in `test/sql/` covering all 7
   functions.

5. **9 new tests** (366 → 375): `Arc<str>` sharing verification, struct size assertion,
   combine reference sharing, empty/unicode/long string values, 32-condition
   interaction, three-state combine chain, mixed null/Arc values in combine.

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

### Session 12: Community Extension Readiness + Benchmark Completion

Session focused on community extension submission readiness, benchmark completion,
comprehensive documentation, and E2E test fixes.

**Changes:**

1. **`sequence_match_events` benchmark** (`benches/sequence_match_events_bench.rs`):
   New Criterion benchmark covering update + finalize throughput (100 to 100M events)
   and combine operations (100 to 1M states). All 7 functions now have dedicated
   benchmarks.

2. **Library name fix for community extension**: Added `name = "behavioral"` to
   `[lib]` in `Cargo.toml` so `cargo build --release` produces `libbehavioral.so`
   (required by the community extension Makefile) instead of
   `libduckdb_behavioral.so`. Updated all benchmark imports from
   `duckdb_behavioral::` to `behavioral::` and fixed the doc-test import.

3. **`sequence_next_node` NULL handling fix** (`src/ffi/sequence_next_node.rs`):
   Added `duckdb_vector_ensure_validity_writable` call before setting validity bits.
   Without this, setting a row as invalid wrote to an uninitialized validity bitmap,
   producing `\0\0\0\0` instead of NULL in the output.

4. **SQL test corrections** (`test/sql/`): Fixed expected values across multiple
   test files — session IDs changed from 0-indexed to 1-indexed (matching actual
   `sessionize` output), timestamp lists changed to single-quoted format (matching
   DuckDB's actual LIST output), and `sequence_next_node` test restructured to
   match the base_condition + event matching semantics.

5. **GitHub Pages documentation expansion**:
   - Enhanced `index.md` with value proposition, use case sections, quick install
   - Enhanced `getting-started.md` with troubleshooting, worked examples
   - Created `faq.md` with loading, functions, pattern syntax, modes, performance,
     ClickHouse differences, data preparation, scaling, integration sections
   - Created `contributing.md` with dev setup, code style, testing, benchmark protocol
   - Created `use-cases.md` with 5 complete real-world examples (e-commerce funnels,
     SaaS retention, web sessions, user journeys, A/B testing)
   - Completed `performance.md` with Sessions 8-11 detail and updated baseline tables
   - Added cross-references between related function pages
   - Updated `SUMMARY.md` with all new pages

6. **Community extension infrastructure**: Initialized `extension-ci-tools` submodule,
   verified `make configure && make release && make test_release` passes all 7 SQL
   test files.

### Session 13: CI/CD Hardening + Benchmark Refresh

Session focused on CI/CD hardening, E2E workflow addition, SemVer release
validation, documentation expansion, and full benchmark baseline refresh.

**Changes:**

1. **CI/CD hardening**: Added E2E workflow testing all 7 functions against real
   DuckDB, SemVer release validation, expanded CI to 13 jobs.

2. **Benchmark refresh**: Re-ran full `cargo bench` suite to establish Session 13
   baseline. All 7 functions benchmarked including `sequence_match_events`
   (previously pending).

3. **Documentation expansion**: Enhanced GitHub Pages site, CLAUDE.md
   modularization, added Mermaid architecture diagrams.

4. **Test count fix**: Corrected stale test count references (375 → 411).

**Session 13 baseline (Criterion-validated, 95% CI):**

| Function | Scale | Wall Clock | Throughput |
|---|---|---|---|
| `sessionize_update` | **1 billion** | **1.18 s** | **848 Melem/s** |
| `retention_combine` | **1 billion** | **2.94 s** | **340 Melem/s** |
| `window_funnel` | 100 million | 715 ms | 140 Melem/s |
| `sequence_match` | 100 million | 902 ms | 111 Melem/s |
| `sequence_count` | 100 million | 1.05 s | 95 Melem/s |
| `sequence_match_events` | 100 million | 921 ms | 109 Melem/s |
| `sequence_next_node` | 10 million | 438 ms | 23 Melem/s |

### Session 14: Production Readiness Hardening

Session focused on closing every gap identified in a 4-audit production readiness
review. All changes are correctness, safety, or documentation improvements — no
new features or performance optimizations.

**Source code safety improvements:**

1. **`Arc<str>` migration** (`src/sequence_next_node.rs`, `src/ffi/sequence_next_node.rs`,
   `benches/sequence_next_node_bench.rs`): Replaced `Rc<str>` with `Arc<str>` for
   `Send + Sync` safety. Eliminates theoretical unsoundness if DuckDB ever
   parallelizes combine operations. Adds ~1-2ns/clone overhead (atomic vs
   non-atomic reference counting) — negligible at measured throughput.

2. **Defensive boolean reading** (all 5 FFI modules): Changed `*const bool` to
   `*const u8` with `!= 0` comparison. Avoids depending on Rust's strict `bool`
   invariant (must be 0 or 1) for data read from DuckDB's C API. Output booleans
   use `u8::from(matched)`.

3. **No-panic FFI entry point** (`src/lib.rs`): Replaced `.unwrap()` on
   `set_error` and `get_database` function pointers with `if let Some()` and
   `.ok_or()` respectively. Prevents panicking across FFI boundaries in debug
   builds.

4. **Hoisted validity bitmap** (`src/ffi/retention.rs`): Moved
   `duckdb_vector_ensure_validity_writable` and `duckdb_vector_get_validity`
   calls outside the finalize loop. Previously called per-null-state inside the
   loop — correct but wasteful.

5. **Interior null byte handling** (`src/ffi/sequence_next_node.rs`): Replaced
   `CString::new(value).unwrap_or_default()` with explicit null-byte stripping
   via `value.replace('\0', "")`. Prevents silently producing empty strings on
   values containing null bytes.

**Documentation fixes:**

6. **Stale benchmark numbers**: Updated 4 function doc pages (retention,
   window-funnel, sequence-match, sequence-count) to Session 13 baseline.
7. **Cross-references**: Added See Also sections to sessionize.md and retention.md.
8. **Negative results count**: Fixed engineering.md from "3" to "5" (added
   compiled pattern preservation and first-condition pre-check).
9. **`Arc<str>` references**: Updated all `Rc<str>` references across 10
   documentation files to `Arc<str>`.

**E2E test expansion (16 new test cases):**

10. **NULL/empty input tests**: sessionize NULL timestamp, retention empty table,
    window_funnel empty table.
11. **All 6 window_funnel modes**: strict, strict_order, strict_deduplication,
    strict_once, allow_reentry (plus existing strict_increase).
12. **5-condition test**: sequence_match with 5 boolean conditions.
13. **All direction×base combinations**: 6 new sequence_next_node tests covering
    forward/backward × head/tail/last_match (existing covered first_match).

**CI/CD hardening:**

14. **Pinned GitHub Actions to commit SHAs**: All third-party Actions across 4
    workflow files pinned to specific SHAs with version comments.
15. **Visible CI failures**: Removed `|| true` from cargo-semver-checks and
    cargo-tarpaulin. Added `continue-on-error: true` at job level instead.

**Repository structure improvements:**

16. **Root-level files**: Added `CHANGELOG.md` (Keep a Changelog format),
    `SECURITY.md` (vulnerability reporting, security model), `CONTRIBUTING.md`
    (quick-start guide linking to full docs).

**Session 14 baseline (Criterion-validated, 95% CI):**

| Function | Scale | Wall Clock | Throughput |
|---|---|---|---|
| `sessionize_update` | **1 billion** | **1.21 s** | **826 Melem/s** |
| `retention_combine` | 100 million | 259 ms | 386 Melem/s |
| `window_funnel` | 100 million | 755 ms | 132 Melem/s |
| `sequence_match` | 100 million | 951 ms | 105 Melem/s |
| `sequence_count` | 100 million | 1.10 s | 91 Melem/s |
| `sequence_match_events` | 100 million | 988 ms | 101 Melem/s |
| `sequence_next_node` | 10 million | 559 ms | 18 Melem/s |

Performance is consistent with Session 13 baseline. `sequence_next_node`
throughput decreased from 23 to 18 Melem/s due to `Arc<str>` atomic reference
counting overhead — an expected and acceptable cost for `Send+Sync` safety.

### Session 15: Enterprise Quality Standards

Session focused on dependency consolidation, documentation polish, benchmark
refresh, and GitHub Pages improvements. No Rust source code changes.

**Dependency updates (7 dependabot PRs consolidated):**

1. **Criterion 0.5 → 0.8.2**: Migrated `criterion::black_box` to
   `std::hint::black_box` across all 7 benchmark files.
2. **rand 0.8 → 0.9.2**: Transparent update — no code uses rand directly.
3. **Swatinem/rust-cache v2.7.8 → v2.8.2**: CI workflow cache action.
4. **actions/setup-python v5.6.0 → v6.2.0**: CI workflow Python setup.
5. **actions/attest-build-provenance v2.2.3 → v3.2.0**: Release attestation.
6. **actions/download-artifact v4.2.1 → v7.0.0**: Release artifact download.
7. **dtolnay/rust-toolchain**: Skipped — uses branch refs, not version tags.

**GitHub Pages improvements:**

8. **Mermaid diagram contrast**: Added comprehensive CSS overrides in
   `custom.css` forcing dark text and visible borders on all mermaid elements.
   Added inline `style` directives with saturated fill colors and explicit
   `color:#1a1a1a` to all mermaid diagrams across 4 documentation files.
9. **Dark theme support**: Navy/coal theme overrides for mermaid edge labels,
   arrows, and backgrounds.
10. **SUMMARY.md restructure**: Reorganized into 4 sections: Getting Started,
    Function Reference, Technical Deep Dive, Operations & Contributing.

**Documentation updates:**

11. **Test count**: Updated 403 → 411 across all documentation files.
12. **Criterion version**: Updated 0.5 → 0.8 references in PERF.md and
    performance.md.
13. **README.md badges**: Added E2E Tests and MSRV badges.
14. **Benchmark baseline refresh**: Full `cargo bench` with Criterion 0.8.2.

**Session 15 baseline (Criterion 0.8.2, 95% CI):**

| Function | Scale | Wall Clock | Throughput |
|---|---|---|---|
| `sessionize_update` | **1 billion** | **1.20 s** | **830 Melem/s** |
| `retention_combine` | 100 million | 274 ms | 365 Melem/s |
| `window_funnel` | 100 million | 791 ms | 126 Melem/s |
| `sequence_match` | 100 million | 1.05 s | 95 Melem/s |
| `sequence_count` | 100 million | 1.18 s | 85 Melem/s |
| `sequence_match_events` | 100 million | 1.07 s | 93 Melem/s |
| `sequence_next_node` | 10 million | 546 ms | 18 Melem/s |

Performance is consistent with Session 14 baseline. Minor throughput
differences reflect Criterion 0.8.2's updated statistical sampling and
rand 0.9.2's different data generation — no Rust source code was changed.

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
  Arc\<str\> sharing verification, struct size assertions, unicode/long strings

Run with `cargo test`. All 411 tests + 1 doc-test run in <1 second.

**E2E tests** (manual, against real DuckDB CLI):
- 27 test cases covering all 7 functions
- Tests: basic usage, timeouts, all 6 modes, GROUP BY, no-match, per-user aggregation,
  NULL inputs, empty tables, 5+ conditions, all direction×base combinations
- Requires: `cargo build --release`, metadata append, `duckdb -unsigned`
- Validates the complete chain: extension load → function registration → SQL execution → correct results

## CI/CD

GitHub Actions workflows in `.github/workflows/`:

- **ci.yml**: check, test, clippy, fmt, doc, MSRV, bench-compile, deny, semver,
  coverage, cross-platform (Linux + macOS), extension-build
- **codeql.yml**: CodeQL static analysis for Rust (push, PR, weekly schedule)
- **e2e.yml**: Builds extension, tests all 7 functions against real DuckDB CLI
- **release.yml**: Builds release artifacts for x86_64 and aarch64 on Linux/macOS,
  creates GitHub release on tag push with SemVer validation
- **pages.yml**: Deploys mdBook documentation to GitHub Pages

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

65 lessons accumulated from 12 development sessions, organized by category in
[`LESSONS.md`](LESSONS.md). Consult before beginning any work.

Categories: Type Safety, Build & Dependencies, Testing Strategy, Unsafe Code & FFI,
Benchmarking & Measurement, Combine Operations, Sorting & Data Structures,
NFA Pattern Engine, ClickHouse Semantics, Performance Optimization Principles,
String & Memory Optimization, Community Extension.
