# CLAUDE.md — DuckDB Behavioral Analytics Extension

Engineering reference for the `duckdb-behavioral` codebase.

## Table of Contents

- [Project Overview](#project-overview)
- [Architecture](#architecture)
- [Build & Test](#build--test)
- [Functions](#functions)
- [Dependencies](#dependencies)
- [Code Quality Standards](#code-quality-standards)
- [Performance](#performance)
- [ClickHouse Parity Status](#clickhouse-parity-status)
- [Testing](#testing)
- [CI/CD](#cicd)
- [Common Tasks](#common-tasks)

## Project Overview

`duckdb-behavioral` is a DuckDB loadable extension written in Rust that provides
behavioral analytics functions inspired by ClickHouse. It is listed in the
[DuckDB Community Extensions](https://github.com/duckdb/community-extensions/tree/main/extensions/behavioral)
repository and can be installed with `INSTALL behavioral FROM community; LOAD behavioral;`.
It can also be built from source, producing a `.so`/`.dylib` that DuckDB loads at runtime.

## Architecture

```
src/
├── lib.rs                  # Entry point via quack_rs::entry_point_v2! macro
├── common/
│   ├── mod.rs
│   ├── config.rs           # Error messages for per-group constant arguments and out-of-memory
│   ├── event.rs            # Event type (u32 bitmask conditions, Copy) shared by window_funnel, sequence_*
│   └── timestamp.rs        # Interval-to-microseconds conversion
├── pattern/
│   ├── mod.rs
│   ├── parser.rs           # Recursive descent parser for sequence patterns
│   ├── executor.rs         # Pattern matcher: fast paths + feasibility-then-greedy matcher
│   └── reference_nfa.rs    # Test-only: the original backtracking search, kept as a differential oracle
├── sessionize.rs           # Sessionize state (boundary-tracking for segment trees)
├── retention.rs            # Retention state (bitmask-based)
├── window_funnel.rs        # Window funnel state (port of ClickHouse windowFunnel, bitflag modes)
├── sequence.rs             # Sequence match/count/events state (wraps pattern engine)
├── sequence_next_node.rs   # Sequence next node state (sequential matching, one byte buffer of values per state)
├── oracle_tests.rs         # Test-only: window_funnel vs an exhaustive reference; order/combine invariance for every state
└── ffi/
    ├── mod.rs              # register_all() — dispatches to all FFI modules via Registrar trait
    ├── view.rs             # Safe views over DuckDB's raw handles (Chunk/columns, States<T>, Output<K>): the only unsafe constructors
    ├── sessionize.rs       # FFI via quack-rs AggregateFunctionBuilder (single fixed signature)
    ├── version.rs          # behavioral_version() scalar via quack-rs ScalarFunctionBuilder
    ├── retention.rs        # FFI via quack-rs builder + returns_logical(LIST(BOOLEAN)) + ListVector
    ├── window_funnel.rs    # FFI via quack-rs builder + FfiState + VectorReader/VectorWriter
    ├── window_funnel_events.rs   # FFI sharing window_funnel update/combine + LIST(TIMESTAMP) finalize
    ├── sequence.rs         # FFI via quack-rs builder for sequence_match + sequence_count
    ├── sequence_match_events.rs  # FFI via quack-rs builder + returns_logical(LIST(TIMESTAMP)) + ListVector
    └── sequence_next_node.rs     # FFI via quack-rs builder + Output<VarcharKind>
```

### Key Design Decisions

1. **Pure Rust core + FFI bridge**: Business logic lives in top-level modules
   (`sessionize.rs`, `retention.rs`, etc.) with zero FFI dependencies. The `ffi/`
   submodules handle DuckDB C API registration only. `unsafe_code = "deny"` in
   `Cargo.toml` makes that mechanical: only `mod ffi` is allowed `unsafe`, and
   inside it `clippy::undocumented_unsafe_blocks` and
   `clippy::multiple_unsafe_ops_per_block` are denied, so every `unsafe` block
   is one operation with a `// SAFETY:` note. The callbacks are safe Rust over
   `ffi/view.rs`, whose `unsafe` constructors state what DuckDB guarantees for
   each handle (flat inputs, declared argument types, initialised states) and
   whose methods bounds-check every row index; `States::pair` panics rather
   than hand out aliasing references. To add a new accessor, add it to
   `view.rs` with its constructor's contract, not an `unsafe` block in a callback.

2. **Aggregate functions via quack-rs SDK**: DuckDB's Rust crate does not yet
   provide high-level aggregate function registration. We use `quack-rs` v0.19.0
   ([crates.io](https://crates.io/crates/quack-rs)) which wraps the raw C API with safe builders
   (`AggregateFunctionSetBuilder`), state management (`FfiState<T>`, installed
   with `ffi_state::<T>()`), panic-guarded callbacks
   (`aggregate_{update,combine,finalize}_callback!`), vector I/O
   (`VectorReader`/`VectorWriter` including `write_varchar`), complex type helpers
   (`ListVector`, `LogicalType::list()`), parameterized return type support
   (`returns_logical(LogicalType)`), and `AggregateTestHarness` for combine
   testing. All 8 aggregate functions use quack-rs builders for registration --
   the seven variadic functions via `AggregateFunctionSetBuilder` (including
   `retention` and `sequence_match_events`, which use
   `returns_logical(LogicalType::list(...))` for their `LIST(T)` return types),
   and `sessionize` via `AggregateFunctionBuilder` (single fixed signature;
   DuckDB windows any aggregate, so no window-specific registration exists).
   The `bundled-test-prebuilt` dev-feature additionally provides `testing::InMemoryDb`,
   used by the in-process extension-load integration test (see [Testing](#testing)).

3. **Function sets for variadic signatures**: Since `duckdb_aggregate_function_set_varargs`
   doesn't exist, we register function sets via
   `AggregateFunctionSetBuilder::overloads(range, ...)`, which automatically calls
   `duckdb_aggregate_function_set_name` on each overload. Every timestamp
   argument is registered for both `TIMESTAMP` and `TIMESTAMPTZ`
   (`ffi::TIMESTAMP_TYPES`; same int64 microseconds; the `*_events`
   functions return `LIST(TIMESTAMPTZ)` for `TIMESTAMPTZ` input), so:
   `retention` 31 overloads (2..=32 conditions, no timestamp), `sequence_*`
   62, `window_funnel` / `window_funnel_events` 128 (1..=32 conditions, with
   and without a mode), `sequence_next_node` 64, `sessionize` 2 (540 with
   `behavioral_version`, checked with `duckdb_functions()`).

4. **Combinable `FunnelMode` bitflags**: `window_funnel` modes are represented as a
   `u8` bitflag struct (`FunnelMode(u8)`) rather than a mutually exclusive enum. This
   enables ClickHouse-compatible mode combinations (e.g., `strict | strict_increase`).
   Five ClickHouse modes are defined: `STRICT` (SQL `'strict_deduplication'`;
   `'strict'` is a backward-compatible alias that ClickHouse 26.9 rejects),
   `STRICT_ORDER`, `STRICT_INCREASE`, `STRICT_ONCE`, `ALLOW_REENTRY` (requires
   `STRICT_ORDER`, an error otherwise). `STRICT_DEDUPLICATION` (SQL:
   `'timestamp_dedup'`) is an extension alias of `STRICT_INCREASE`.

   The funnel engine is a port of ClickHouse's `windowFunnel`
   (`AggregateFunctionWindowFunnel.cpp`): one entry per true condition,
   visited in `(timestamp, condition)` order, latest-entry chain kept per
   level. Two ClickHouse defects are deliberately fixed (see
   [ClickHouse Parity Status](#clickhouse-parity-status)); `strict_once` uses
   bipartite matching instead of chain enumeration.

5. **O(1) combine for sessionize**: The `SessionizeBoundaryState` tracks `first_ts`,
   `last_ts`, and `boundaries` count, enabling O(1) combine for DuckDB's segment
   tree windowing machinery.

6. **Entry point via `quack_rs::entry_point_v2!` macro**: The `behavioral_init_c_api`
   symbol is generated by the macro, which handles API initialization, connection
   management via `duckdb_connect`/`duckdb_disconnect`, and error reporting. The
   closure receives a `&Connection` implementing the `Registrar` trait, providing
   a version-agnostic API for registering extension components. The variadic
   functions use `con.register_aggregate_set(builder)` and `sessionize` uses
   `con.register_aggregate(builder)` — nothing touches the raw connection handle.

7. **Strict configuration validation**: Invalid configuration aborts the query
   with a descriptive SQL error (via `AggregateFunctionInfo::set_error` →
   `duckdb_aggregate_function_set_error`) instead of silently producing wrong
   results or NULL: unknown `window_funnel` mode strings, month-based or
   negative INTERVAL windows/gaps, malformed sequence patterns (the parser's
   position-annotated message), and unknown `sequence_next_node`
   direction/base values. NULL configuration parameters remain lenient (row
   skipped / NULL result), matching SQL aggregate conventions.

   Configuration arguments (pattern, window, mode, direction, base) must be
   the same for every row of a group: every update and combine compares them
   and raises `... the <arg> argument must be the same for every row of a
   group` on a difference, so the error does not depend on row order or
   thread scheduling. (Taking the first value seen, as before 0.10.0, made
   results order-dependent.) `sessionize`'s gap is the exception: each row's
   gap legitimately judges that row. Per chunk, the raw 16-byte string slot
   (`Varchars::slot` in `ffi/view.rs`) and last parse are cached, so an
   unchanged value costs one comparison. Allocation failures (event buffers,
   the value buffer of `sequence_next_node`, the sequence matcher) are SQL
   errors too, never aborts.

## Build & Test

```sql
-- Install from DuckDB Community Extensions (no build required)
INSTALL behavioral FROM community;
LOAD behavioral;
```

```bash
# Build from source (dev)
cargo build

# Build from source (release, produces loadable .so/.dylib)
cargo build --release

# Everything CI runs that works offline, in one command (tools that are not
# installed are skipped): ./scripts/check.sh   (--quick skips docs and benches)

# Run all tests (541 unit + 29 integration + 1 doc-test).
# DUCKDB_DOWNLOAD_LIB=1 makes libduckdb-sys link a prebuilt libduckdb
# (downloaded once, cached in target/duckdb-download/) instead of compiling
# DuckDB's C++ tree from source. Offline alternative: DUCKDB_LIB_DIR=<dir>
# (plus LD_LIBRARY_PATH=<dir> on Linux). Never set these for release builds.
DUCKDB_DOWNLOAD_LIB=1 cargo test

# Run clippy (must produce zero warnings; CI adds --locked -- -D warnings)
cargo clippy --all-targets

# Format check
cargo fmt -- --check

# Run benchmarks
cargo bench

# Build docs (CI: RUSTDOCFLAGS=-Dwarnings, --document-private-items)
cargo doc --no-deps

# E2E: `make configure release test_release` builds the stamped extension and
# runs test/sql/*.test; then assert results through any DuckDB CLI:
#   scripts/compat-check.sh "$(command -v duckdb)" build/release/behavioral.duckdb_extension
# By hand against a real DuckDB (requires duckdb CLI):
# 1. Build release
cargo build --release
# 2. Copy and append metadata. Stable C API: ABI type C_STRUCT (the default),
#    -dv is the C API version (v1.2.0), not a DuckDB release. Never pass
#    --abi-type C_STRUCT_UNSTABLE: it pins the binary to one DuckDB release.
cp target/release/libbehavioral.so /tmp/behavioral.duckdb_extension
python3 extension-ci-tools/scripts/append_extension_metadata.py \
  -l /tmp/behavioral.duckdb_extension -n behavioral \
  -p linux_amd64 -dv v1.2.0 -ev v0.10.1 \
  -o /tmp/behavioral.duckdb_extension
# 3. Load and test
duckdb -unsigned -c "LOAD '/tmp/behavioral.duckdb_extension'; SELECT ..."
# Or the community path, which stamps the same footer:
make configure release test_release
```

## Functions

| Function | Signature | Returns | Description |
|---|---|---|---|
| `sessionize` | `(TIMESTAMP, INTERVAL)` | `BIGINT` | Window function assigning session IDs |
| `retention` | `(BOOLEAN, BOOLEAN, ...)` | `BOOLEAN[]` | Cohort retention analysis |
| `window_funnel` | `(INTERVAL[, VARCHAR], TIMESTAMP, BOOLEAN, ...)` | `INTEGER` | Conversion funnel step tracking |
| `window_funnel_events` | `(INTERVAL[, VARCHAR], TIMESTAMP, BOOLEAN, ...)` | `LIST(TIMESTAMP)` | Timestamps of the best funnel chain |
| `sequence_match` | `(VARCHAR, TIMESTAMP, BOOLEAN, ...)` | `BOOLEAN` | Pattern matching over events |
| `sequence_count` | `(VARCHAR, TIMESTAMP, BOOLEAN, ...)` | `BIGINT` | Count non-overlapping pattern matches |
| `sequence_match_events` | `(VARCHAR, TIMESTAMP, BOOLEAN, ...)` | `LIST(TIMESTAMP)` | Return matched condition timestamps |
| `sequence_next_node` | `(VARCHAR, VARCHAR, TIMESTAMP, VARCHAR, BOOLEAN, BOOLEAN, ...)` | `VARCHAR` | Next event value after pattern match |
| `behavioral_version` | `()` | `VARCHAR` | Loaded extension version (diagnostic scalar) |

Every `TIMESTAMP` argument also accepts `TIMESTAMPTZ` (the `*_events`
functions then return `TIMESTAMPTZ[]`).

## Dependencies

**Runtime** (linked into the `.so`/`.dylib`):
- `quack-rs` v0.19.0 ([crates.io](https://crates.io/crates/quack-rs)) — Rust SDK
  for DuckDB loadable extensions. Provides `entry_point_v2!` macro,
  `Connection`/`Registrar` trait for version-agnostic registration,
  `AggregateFunctionSetBuilder` (with `returns_logical(LogicalType)` for `LIST(T)` returns),
  `FfiState<T>`, `VectorReader`/`VectorWriter` (including `write_varchar`),
  `ListVector` for LIST output, `LogicalType::list()` for parameterized types,
  and `AggregateTestHarness` for combine testing. Re-exports `libduckdb-sys`
  with `loadable-extension` feature.
- `smallvec = "1"` — Inline storage for the first two events of a group
  (`SmallVec<[Event; 2]>` in `window_funnel` and `sequence`), so millions of
  tiny groups make no heap allocation. A caret requirement, locked by
  `Cargo.lock`; 2.x is not adopted.
- `libduckdb-sys = "=1.20000.1"` with `loadable-extension` feature — Re-exported
  by quack-rs but pinned explicitly: the FFI modules use its raw types
  (`duckdb_function_info`, `duckdb_data_chunk`, `duckdb_aggregate_state`, …)
  in the `unsafe extern "C"` callback signatures.
  Note: crate versioning uses `1.MAJOR_MINOR_PATCH.x` scheme (DuckDB v1.5.6 →
  crate v1.10506.x; v2.0.0 → v1.20000.x). The pin sets the headers the bindings are generated
  from, not the DuckDB the extension runs on: the binary uses only the
  stable C API and loads into DuckDB 1.3.2+ (see "Stable C API" below).

**Dev-only** (unit tests and benchmarks):
- `duckdb = "=1.20000.1"` (no `bundled` feature) — Used in `#[cfg(test)]` modules
  for `Connection::open_in_memory()`. Not linked into the release extension.
  Dev/test builds link a **prebuilt libduckdb** downloaded by `libduckdb-sys`
  (`DUCKDB_DOWNLOAD_LIB=1`, cached in `target/duckdb-download/`) instead of
  compiling DuckDB's C++ tree from source. `ci.yml` sets the env var workflow-wide;
  locally run `DUCKDB_DOWNLOAD_LIB=1 cargo test`, or set
  `DUCKDB_LIB_DIR=/path/to/libduckdb` (plus `LD_LIBRARY_PATH` on Linux) to use
  an already-downloaded library. Never set either variable for release builds.
  Gotcha: with the unified `loadable-extension` feature set, libduckdb-sys only
  emits `rerun-if-env-changed` for `DUCKDB_DOWNLOAD_LIB` — setting
  `DUCKDB_LIB_DIR` *after* a previous build does not invalidate the cached
  build-script output; clear `target/*/build/libduckdb-sys-*` (or toggle
  `DUCKDB_DOWNLOAD_LIB`) to force a re-run.
  Note: crate versioning uses `1.MAJOR_MINOR_PATCH.x` scheme (DuckDB v1.5.6 →
  crate v1.10506.x; v2.0.0 → v1.20000.x).
  Gotcha: rust-cache cannot keep `target/duckdb-download`: before saving it
  prunes `target/`, treating that directory as a build profile and keeping
  only `build/`, `.fingerprint/` and `deps/`, so `cache-directories` saves an
  empty directory. A warm cache then restores libduckdb-sys's build-script
  output without the library (`unable to find library -lduckdb`). The
  `setup-rust` composite action (`.github/actions/setup-rust`) runs
  "Re-fetch prebuilt libduckdb if the cache dropped it" after rust-cache for
  every job that sets `DUCKDB_DOWNLOAD_LIB=1`; bumping the cache key only
  defers this. The step must reach every build directory: cargo-llvm-cov
  builds into `target/llvm-cov-target/debug`, one level deeper than
  `target/debug`, and a `target/*/build/...` glob missed it (the Coverage job
  failed on the first pull request after a cache existed: `quack-rs`'s build
  script panicked for lack of `DEP_DUCKDB_INCLUDE`).
  Gotcha: libduckdb-sys's downloader (ureq/rustls with bundled roots) ignores
  a proxy CA. Behind a TLS-intercepting proxy, fetch
  `libduckdb-linux-amd64.zip` with curl into
  `target/duckdb-download/<target-triple>/<duckdb-version>/`; the build script
  skips the download when the archive exists.
- `quack-rs` v0.19.0 with `bundled-test-prebuilt` feature — Provides
  `testing::InMemoryDb` (incl. `open_unsigned()`) for the in-process
  extension-load integration test (`tests/extension_load.rs`). Links the same
  prebuilt libduckdb as the `duckdb` dev-dependency above (no DuckDB C++
  build); dev-only, never in the release `.so`.
- `criterion = "0.8"` with `html_reports` feature — Statistical benchmarking
- `proptest = "1"` — Property-based testing

## Code Quality Standards

### Mandatory Requirements

Every change MUST meet these requirements:

1. **`cargo test`** — ALL tests pass. Zero failures. Zero ignored.
2. **`cargo clippy --all-targets`** — Zero warnings (CI: `--locked -- -D warnings`),
   including the `unsafe_code`, `undocumented_unsafe_blocks` and
   `multiple_unsafe_ops_per_block` denials.
3. **`cargo fmt -- --check`** — Zero formatting violations.
4. **`cargo doc --no-deps`** — Builds without errors or warnings (CI adds
   `--document-private-items` with `RUSTDOCFLAGS=-Dwarnings`).
5. **Test count verified** — Actual test count matches documented count.
6. **Documentation accurate** — All claims in CLAUDE.md, PERF.md, docs/ are
   verifiable and match current code state.

### Current Metrics

- **Zero clippy warnings** with pedantic, nursery, and cargo lint groups enabled
- **541 unit tests** covering all functions, edge cases, combine associativity,
  property-based testing (proptest), mutation-testing-guided coverage,
  ClickHouse mode combinations, and `AggregateTestHarness` combine
  config-propagation tests for all 8 aggregate functions (across 7 FFI test
  modules -- `sequence_match` and `sequence_count` share one state type;
  `window_funnel_events` shares `WindowFunnelState`). `src/oracle_tests.rs`
  checks `window_funnel` against an exhaustive reference and every state for
  order/split/combine-tree invariance (below).
- **1 doc-test** for the pattern parser
- **29 in-process integration tests** (`tests/extension_load.rs`): build the real
  release `cdylib`, append the DuckDB metadata footer, `LOAD` it into an
  in-memory DuckDB via `quack_rs::testing::InMemoryDb::open_unsigned()`, and
  exercise all 8 aggregate functions plus the `behavioral_version()` scalar
  through live SQL — the registration/FFI path unit
  tests cannot reach, now covered inside `cargo test` (no external CLI)
- **E2E tests** against real DuckDB CLIs: 8 SQL logic test files with 78
  directives (44 result-checked queries) covering all 8 functions with multiple
  scenarios (basic, timeout, modes, GROUP BY, no-match, NULL inputs, empty
  tables, all funnel modes, 5+ conditions, all 8 direction/base combinations),
  and `scripts/compat-check.sh`: 21 asserted queries (all functions,
  `TIMESTAMPTZ`, NULLs, error messages, the 540-overload count, a 1- vs
  4-thread determinism check), run on Linux and macOS in DuckDB 2.0.0 and on
  Linux in 1.3.2, 1.4.4, 1.5.0 and 1.5.6
- **7 Criterion benchmark files** (sessionize, retention, window_funnel, sequence, sort,
  sequence_next_node, sequence_match_events) with combine benchmarks, realistic
  cardinality benchmarks, and throughput reporting up to 1B elements
- **Mutation testing** (cargo-mutants 27.1.0, `-- --lib`; not re-run since the
  0.10.1 test changes: duplicate and length-only tests were removed, the oracle
  tests added): 88.4% (130/147)
  on v0.4.x. The 0.10.0 audit of the 9 non-FFI modules detected 570 of 607
  viable mutants (547 caught + 23 timeouts, 93.9%; 37 missed). After the
  `mutation_tests` modules and two `Matcher` tests, `window_funnel.rs`,
  `sequence_next_node.rs` and `pattern/parser.rs` were re-run: 320 of 342
  viable detected (315 caught + 5 timeouts, 93.6%). The 22 survivors were
  reviewed one by one and judged equivalent: the funnel's hash function,
  `NoPath::steps` (never called), cache-key and shortcut paths in the
  `strict_once` matcher that only change sharing, and guards whose
  alternative branch returns the same value. The other six modules have
  only the audit figures.
- **MSRV 1.88**: `cargo +1.88.0 check --all-targets --locked` passes with
  quack-rs 0.19.0 (which declares 1.86.0) and libduckdb-sys / duckdb 1.20000.1
  (1.88, the crates.io `rust_version`). Edition 2024 needs 1.85 and let chains
  1.88, so the MSRV is not lowerable. `rust-toolchain.toml` pins the toolchain
  CI and releases use (1.97.0); the `setup-rust` action exports
  `RUSTUP_TOOLCHAIN`, which outranks the file, so the `msrv` job really runs 1.88.
  Bump the pin when `scheduled.yml`'s stable and beta jobs pass.
- All public items have documentation
- Release profile: LTO, single codegen unit, `panic = "unwind"` (required —
  see "Panic containment" below), stripped symbols

## Performance

Performance engineering is documented in [`PERF.md`](PERF.md), which contains
optimization history with before/after measurements, algorithmic complexity
analysis, and the benchmark improvement protocol.

**Last recorded baseline (Criterion 0.8.2, 95% CI of the mean, PERF.md
Session 21: shared 4-vCPU Xeon @ 2.80 GHz, rustc 1.97.0, `bench` profile).
These are the Rust state functions without DuckDB; the Session 15 numbers they
replace came from another machine and, for `window_funnel`, an engine replaced
in 0.10.0:**

| Function | Scale | Time | Throughput |
|---|---|---|---|
| `sessionize_update` | **1 billion** | **1.68 s** | **595 Melem/s** |
| `retention_combine` | 100 million | 251 ms | 398 Melem/s |
| `window_funnel_finalize` (8 conditions) | 100 million | 2.12 s | 47 Melem/s |
| `window_funnel_events_finalize` | 100 million | 1.49 s | 67 Melem/s |
| `sequence_match` | 100 million | 1.49 s | 67 Melem/s |
| `sequence_count` | 100 million | 1.50 s | 67 Melem/s |
| `sequence_match_events` | 100 million | 1.30 s | 77 Melem/s |
| `sequence_next_node` | 10 million | 587 ms | 17 Melem/s |

The 100M rows have 10 samples and wide intervals; PERF.md lists them. SQL
timings come from `scripts/bench-sql.sh` (10M rows, 100,000 groups, 4 threads):
1.5 to 1.9 s per aggregate query, `sequence_next_node` 3.3 s, `retention`
0.46 s. Session 21 compared v0.10.1 and this branch on the same machine,
alternating runs: no change beyond the run-to-run spread except
`sequence_next_node`, about twice as fast. Compare against numbers measured on
the same machine in the same session, never against the table above from a
different machine.

Key optimizations: u32 bitmask conditions (eliminates per-event heap alloc),
in-place O(N) combine (replaces O(N^2) merge-allocate), a
feasibility-then-greedy sequence matcher (replaced a backtracking search that
was quadratic per group for patterns with skipping time constraints),
fast-path linear scans for common pattern shapes, presorted detection, and, for
`sequence_next_node`, one byte buffer of values per state with 24-byte records
(replaced an `Arc<str>` per event).

## ClickHouse Parity Status

All six ClickHouse behavioral parametric functions are implemented and were
differentially tested against ClickHouse 26.10.2.103 (`clickhouse local`;
`windowFunnel` also on 26.9.8.3, with identical results) on random event
groups (ties, multi-condition events, every mode combination). The counts are
in `docs/src/internals/clickhouse-compatibility.md`. Every remaining
difference is listed below with its cause. For `sequence_match`, `sequence_count`
and `sequence_match_events`, `scripts/clickhouse-sequence-diff.py` checks that a
model of each engine predicts every result (8 case sets x 600,000 cells x 3
functions x 2 engines, on each of two seeds, zero exceptions); the ClickHouse rules were found by
running it, not from its source. Treat any other difference as a bug. To
re-check, download the ClickHouse static binary
(`clickhouse-common-static-<ver>-amd64.tgz` from the ClickHouse GitHub
release); for the `sequence_*` functions run
`scripts/clickhouse-sequence-diff.py --clickhouse <bin> --duckdb <cli>
--extension <ext>` (about 10 minutes for the default eight cases) after any
change to the matcher, and compare per group for the other functions; for
`window_funnel`, an exhaustive reference (ClickHouse's algorithm keeping every
chain, ties ordered canonically) must equal the extension in every case.

### Scope

The ClickHouse parametric functions page documents 10 functions. Six are
behavioral (all implemented). Four are out of scope:

| Category | Functions | Status |
|---|---|---|
| Behavioral | `retention`, `windowFunnel`, `sequenceMatch`, `sequenceCount`, `sequenceMatchEvents`, `sequenceNextNode` | All implemented |
| Statistical | `histogram` | Out of scope (not behavioral) |
| Cardinality | `uniqUpTo` | Out of scope (not behavioral) |
| Map aggregation | `sumMapFiltered`, `sumMapFilteredWithOverflow` | Out of scope (not behavioral) |

Full justification: [`docs/src/internals/clickhouse-compatibility.md`](docs/src/internals/clickhouse-compatibility.md)

### `windowFunnel` Mode Mapping

`'strict_deduplication'` maps to `STRICT` (0x01); `'strict'` is accepted as an
alias (ClickHouse 26.9 rejects it). `'timestamp_dedup'` (0x04) is an alias of
`strict_increase`. `'allow_reentry'` requires `'strict_order'`.

### Extensions Beyond ClickHouse

- `sessionize`: Window function (no ClickHouse equivalent)
- `window_funnel_events`: Returns the best funnel chain's step timestamps as
  `LIST(TIMESTAMP)` (ClickHouse's `windowFunnel` has no timestamp-returning
  companion)
- `'timestamp_dedup'` mode: alias of `strict_increase` in `window_funnel`
- `(?t!=N)` time constraint: Not-equal operator in sequence patterns
  (`.` and `.*` and the other five time operators match ClickHouse)
- No settings required (ClickHouse's `sequenceNextNode` requires
  `SET enable_funnel_functions = 1`; the former name
  `allow_experimental_funnel_functions` is still accepted there)

### Known Semantic Differences

Deliberate (ClickHouse is defective or order-dependent):

1. **`windowFunnel` `strict_increase` without `strict_once`**: ClickHouse keeps
   one chain per level and loses a valid one (`c1@0, c1@1, c2@1` → 1); we keep
   the best chain ending before the current timestamp (→ 2). With
   `strict_deduplication` this can also make our answer lower.
2. **Arrival-order ties**: ClickHouse's `windowFunnel` + `strict_once` (with
   `strict_deduplication` its answer changes with row order),
   `sequenceNextNode` ties on `(timestamp, value)`, and `sequenceMatch` ties on
   timestamp all depend on arrival order. We sort by total keys:
   `(timestamp, conditions)` for `Event`, `(timestamp, value, base_condition,
   conditions)` for `sequence_next_node`.
3. **Time-constraint anchor**: we measure `(?t op N)` from the event consumed
   by the last `(?N)` or `.`; after `.*` ClickHouse measures from the event
   where `.*` stopped consuming (so `(?1).*(?t>0)(?2)` never matches there),
   and neither `.*` nor a skipping constraint can absorb the last event (a
   trailing `.*(?t<0)` matches only if earlier steps consumed it). A constraint
   before any `(?N)`/`.` is a parse error here.
4. **`sequenceMatchEvents`** reports the first chain that reached the longest
   length, even if that attempt then failed a later constraint and a later
   attempt completed the pattern (also when it shares its first step with the
   match); we report the chain that matched. Pinned by
   `test_events_report_the_completing_chain_not_an_abandoned_attempt`.
5. **ClickHouse UB**: its trailing-skip loop for patterns ending in `.*` /
   `(?t<…)` / `(?t<=…)` / `(?t>=0)` may read past the action list. On 26.9.8.3
   identical input gave different answers between runs (308 `sequenceMatch` and
   144 `sequenceCount` of 1.8M results in 4 runs); on 26.10.2.103 none of
   1.8M varied.

Other:

6. **NULLs**: ClickHouse skips rows with any NULL argument; we treat NULL
   conditions as false, skip NULL timestamps, keep NULL `sequence_next_node`
   values.
7. **Units**: `INTERVAL` windows (months rejected); microsecond timestamps,
   `(?t)` thresholds in seconds with elapsed time floored (`(?t==N)` means
   `[N, N+1)`); gaps touching `±infinity` computed exactly in `u64`.
8. **Accepted input**: we accept pattern whitespace, `'strict'`, and two
   consecutive time constraints after an event; ClickHouse accepts an empty
   pattern and `u64` thresholds.
9. **Empty input**: `retention` → `[]`, `sequence_match`/`count` → NULL
   (ClickHouse: zeros) over zero rows. Rows with no true condition and a
   `.*`-only pattern: `sequence_match` true, `sequence_count` 0, as in
   ClickHouse.
10. **`sequenceNextNode` limit**: ClickHouse 64 conditions, ours 32.

## Testing

Tests are organized as `#[cfg(test)] mod tests` within each module.

**Test categories:**

- **State tests**: Empty state, single update, multiple updates
- **Edge cases**: Threshold boundaries, NULL handling, empty inputs
- **Combine correctness**: Empty combine, boundary detection, associativity,
  config propagation via `AggregateTestHarness`
- **Property-based tests**: 37 proptest tests verifying algebraic properties
  (associativity, commutativity, identity, idempotency, monotonicity),
  including tests at the 32-condition limit, a brute-force check of the
  `strict_once` matching, and 2 differential tests checking the pattern
  matcher against the original backtracking search
  (`src/pattern/reference_nfa.rs`, test-only) result for result
- **Oracle and invariance tests** (`src/oracle_tests.rs`): `window_funnel` is
  compared with an exhaustive reference over every mode combination, and every
  state (`window_funnel`, `sequence_*`, `retention`, `sequence_next_node`,
  `sessionize`) must give the same answer under input permutation, any split
  into partial states, any combine tree, and combine into a fresh target
- **Parser never panics**: `pattern::parser::fuzz_tests` feeds random byte
  strings and pattern-like strings to the parser
- **Mutation-testing-guided tests**: 51 tests from the v0.4.x cargo-mutants
  analysis, plus 16 from the 0.10.0 runs (the `mutation_tests` modules and
  two `Matcher` tests)
- **Pattern parser**: All operators, error positions, whitespace tolerance
- **Pattern executor**: Match/no-match, wildcards, time constraints, counting,
  event collection, fast-path classification, linear scaling on the shapes
  that were quadratic under the old backtracking search
- **`FunnelMode` tests**: Bitflag operations, parsing, all six modes,
  mode combinations
- **`sequence_match_events` tests**: Multi-step, gap events, no-match,
  complex patterns
- **`window_funnel_events` tests**: Complete/partial/empty chains, multi-step
  events, best-chain selection, reentry resets, length == `finalize()` parity
  across modes
- **`sequence_next_node` tests**: All 8 direction/base combinations,
  multi-step patterns, combine, NULL versus empty-string values, value sharing
  and range rebasing in the byte arena

Run with `DUCKDB_DOWNLOAD_LIB=1 cargo test`. The 541 unit tests take a few
seconds (the doc-test about 1 s). The 29 in-process integration tests add
20–35 s when the release `cdylib` must be rebuilt (they build and `LOAD` it)
and about 5 s once it is cached. CI runs the same tests with
`cargo nextest run --all-targets` plus `cargo test --doc`.

**In-process integration tests** (`tests/extension_load.rs`, run by `cargo test`):
- Build the release `cdylib`, append the DuckDB metadata footer (a Rust port of
  `append_extension_metadata.py`), and `LOAD` it via
  `quack_rs::testing::InMemoryDb::open_unsigned()` (quack-rs 0.19.0), with
  `allow_extensions_metadata_mismatch` left off so a wrong stamp fails
- Assert all 8 functions register and return correct results through live SQL —
  catching the load/registration/wrong-result class of FFI bugs that unit tests
  cannot, without needing the external `duckdb` CLI

**E2E tests** (against real DuckDB CLI):
- `e2e.yml` (reusable; `ci.yml` calls it): per platform (Linux + macOS) the
  community Makefile build, the SQL logic tests, and `scripts/compat-check.sh`;
  a `compat` job then loads the Linux binary into older DuckDB CLIs
- 8 SQL logic test files with 78 directives (44 result-checked queries) in `test/sql/`
- `scripts/crash-shapes.sh` runs the three query shapes of duckdb/duckdb#26109
  against a CLI and reports which crash (not part of CI: the outcome is a
  property of the DuckDB release, see the matrix under DuckDB FFI Gotchas)
- Covers all 8 functions with basic usage, timeouts, all 6 modes, GROUP BY,
  no-match, NULL inputs, empty tables, 5+ conditions, all direction x base combinations
- Requires: `cargo build --release`, metadata append, `duckdb -unsigned`

## CI/CD

GitHub Actions workflows in `.github/workflows/` (details:
`docs/src/operations/ci-cd.md`). Every `uses:` is pinned to a commit SHA, every
`cargo` call is `--locked`, and the toolchain, cache and libduckdb handling is
the composite action `.github/actions/setup-rust`.

- **ci.yml**: fmt, clippy, test (Linux + macOS, nextest + doc-test), doc, msrv,
  wasm-check (compile for `wasm32-unknown-emscripten`), bench-compile, deny,
  hygiene (machete, typos, actionlint, zizmor, shellcheck, SPDX headers),
  coverage (informational), e2e (calls `e2e.yml`), and `ci-gate`, the only
  required status check
- **e2e.yml**: reusable; builds the stamped extension on Linux and macOS, runs
  `test/sql` and `scripts/compat-check.sh`, then a `compat` job loads the same
  binary into DuckDB v1.3.2, v1.4.4, v1.5.0 and v1.5.6 and asserts the results
- **scheduled.yml**: weekly canaries (RustSec advisories, stable and beta
  toolchains, `cargo update`, DuckDB nightly); red means look, not blocked
- **codeql.yml**: CodeQL static analysis for Rust (push, PR, weekly schedule)
- **release.yml**: validates the tag against `Cargo.toml`, `description.yml` and
  `CHANGELOG.md`, builds four platforms without a cache, attaches
  `SHA256SUMS.txt` and build-provenance attestations; steps in `RELEASING.md`
- **pages.yml**: builds the mdBook on pull requests, deploys on `main`

## Common Tasks

### Adding a new function

1. Create `src/new_function.rs` with state struct implementing `Default + Send + 'static`,
   plus `update()`, `combine_in_place()`, `finalize()` methods
2. Add `impl quack_rs::aggregate::AggregateState for NewFunctionState {}` in the FFI module
3. Create `src/ffi/new_function.rs` using quack-rs builders:
   - Use `AggregateFunctionSetBuilder::new("name").overloads(...)` for registration
   - Use `FfiState::<NewFunctionState>::size_callback`/`init_callback`/`destroy_callback`
   - Use `VectorReader` for input (read_bool, read_str, read_i64, read_interval)
   - Use `VectorWriter` for output (write_i32, write_bool, write_varchar, set_null)
   - For `LIST(T)` output, use `.returns_logical(LogicalType::list(TypeId::...))` on the
     builder, and `ListVector` + `VectorWriter` for child data in finalize
   - **CRITICAL**: `combine_in_place` must propagate ALL configuration fields (not just events)
4. Register in `src/ffi/mod.rs` `register_all()` via `Registrar` trait
5. Add `pub mod new_function;` to `src/lib.rs`
6. Add benchmark in `benches/`
7. Write unit tests + `AggregateTestHarness` combine config-propagation tests
8. **E2E test**: Build release, load in DuckDB CLI, verify SQL produces correct results

### Updating DuckDB version

The entry point uses `quack-rs` which depends on `libduckdb-sys`. When updating:

A new DuckDB release does not require a new build to load the extension (it is
stamped for the stable C API, v1.2.0). To build against the new headers:

1. Update `libduckdb-sys` version in `[dependencies]` and `duckdb` in `[dev-dependencies]`
2. Update `DUCKDB_TEST_VERSION` in `Makefile`. Do **not** change
   `TARGET_DUCKDB_VERSION`: it is the C API version (`v1.2.0`), and raising it
   raises the minimum DuckDB that can load the binary
3. Update `DUCKDB_VERSION` and the `compat` matrix in `.github/workflows/e2e.yml`.
   DuckDB 2.0.0 renamed its CLI assets (`duckdb-cli-linux-amd64.tar.gz`,
   `duckdb-cli-osx-universal.tar.gz`; older releases keep
   `duckdb_cli-linux-amd64.zip`); `scripts/install-duckdb-cli.sh` handles both
   names, so only the version strings change
4. Re-run the crash-shape matrix (`scripts/crash-shapes.sh <extension> <duckdb>...`) on the new
   release and update the matrix under DuckDB FFI Gotchas, the README "Known
   limitations" and the FAQ
4a. Run `scripts/check-stable-api.py` (Stable C API note below) in case a
   quack-rs upgrade started using a newer C API function; CI also runs it
5. Update DuckDB-version references in docs (`CLAUDE.md`, `README.md`,
   `docs/src/**/*.md`, `CONTRIBUTING.md`, `SECURITY.md`)
6. Check whether the new `libduckdb-sys`/`duckdb`/`criterion` MSRVs raise
   the project MSRV; if so, update `rust-version` in `Cargo.toml`,
   `MSRV (X.Y)` in `.github/workflows/ci.yml`, and badges/text in docs
7. Run full unit test suite (`cargo test`)
8. **MANDATORY**: E2E test -- `make configure release test_release`, then load
   `build/release/behavioral.duckdb_extension` (stamped `C_STRUCT`, `-dv
   v1.2.0`) into the new DuckDB CLI and at least one older one, and verify all
   8 functions

### Performance optimization session

Follow the protocol in [`PERF.md`](PERF.md):

1. Run `cargo bench` 3 times. Record baseline. Compare against PERF.md "Current Baseline".
2. One optimization per commit. Measure before/after with confidence intervals.
3. Accept only when CIs do not overlap. Revert and document if not significant.
4. Update PERF.md: add optimization history entry, update current baseline.
5. Update test count in this file if tests were added or removed.

## Critical Implementation Notes

Hard-won knowledge from developing this extension. Consult before making changes.

### DuckDB FFI Gotchas

- **Combine creates zero-initialized targets**: DuckDB's segment tree calls
  `state_init` on fresh target states, then combines sources into them. All
  config fields (`window_size_us`, `mode`, `pattern_str`, `direction`, `base`,
  `num_steps`) default to zero/empty and must be explicitly propagated in
  `combine_in_place`. Failure produces silently wrong results at finalize.

- **Function set name must be set per overload**: `duckdb_register_aggregate_function_set`
  silently fails if individual functions don't have `duckdb_aggregate_function_set_name`
  called. The quack-rs builder handles this automatically; raw registration must not forget it.

- **`duckdb_vector_ensure_validity_writable` required before setting NULL**: Without
  this call, `duckdb_vector_get_validity` returns an uninitialized pointer. Writing
  to it produces garbage instead of NULL. `VectorWriter::set_null()` handles this
  automatically.

- **Stable C API (`C_STRUCT`, `-dv v1.2.0`)**: the extension dereferences only
  stable-prefix slots of `duckdb_ext_api_v1` (76 of 548 slots, highest 306;
  the stable prefix is 357 slots). `scripts/check-stable-api.py` measures it
  by disassembling an unstripped release build (`cargo build --profile
  profiling --lib`) and collecting the `___DUCKDB_*` statics that are *loaded*
  (every one is stored at init); CI runs it on Linux (`e2e.yml`) and
  `scripts/check.sh` runs it too. The FFI-view refactor left the figures
  unchanged (76, 306). A slot at or above
  357 cannot be called from a `C_STRUCT -dv v1.2.0` binary. Slots 357-403 are
  APIs deprecated since v1.0.0; slots 404-547 became stable only in v1.5.6, so
  using one means stamping `-dv v1.5.6`, which refuses 1.3.2 to 1.5.5.
  `USE_UNSTABLE_C_API=1` is worse: it pins the binary to one DuckDB release
  and is what dropped v0.9.1 from the community channel when DuckDB v1.5.6
  shipped; on DuckDB 2.0.0 `C_STRUCT_UNSTABLE` always means the V2 C API, so
  a V1-entry-point binary cannot load there at all (VERIFIED: stamped with
  `-dv v2.0.0` it fails with `did not contain function
  "behavioral_init_c_api_v2"`; `extension_load.cpp` in DuckDB 2.0.0). With the stable stamp, `-dv` is the C API version
  (`quack_rs::DUCKDB_API_VERSION`), set by `TARGET_DUCKDB_VERSION` in the
  `Makefile`. CI's `compat` job (`e2e.yml`) loads one binary into DuckDB
  v1.3.2, v1.4.4, v1.5.0, v1.5.6 and v2.0.0. Residual risk: libduckdb-sys's init
  copies all 548 slot pointers (`if let Some(fun) = (*p_api).<slot>` per
  slot), and an older DuckDB's struct is shorter, so the copy reads past it;
  the extra values are never called. VALIDATED benign in practice: under
  valgrind (release build, 1.3.2 and 1.4.4 CLIs, a query per function) there
  were 0 invalid reads or writes, and every reported "conditional jump
  depends on uninitialised value" (59 on 1.3.2, 52 on 1.4.4) was inside
  `behavioral_init_c_api` during `LOAD`; none during queries, and the same
  CLIs without the extension, and DuckDB 2.0.0 with it, report 0. The
  over-read itself is upstream (libduckdb-sys's init) and is not fixable
  here.

- **No interrupt check is possible**: DuckDB 1.5.6's C API has
  `duckdb_interrupt(connection)` but nothing an aggregate callback can poll,
  so a query cannot be cancelled mid-finalize. Keep finalize bounded (no
  superlinear paths); never add a long-running loop to a callback.

- **C++ exceptions must not unwind through Rust frames**:
  `duckdb_list_vector_reserve` throws `OutOfRangeException` above
  `MAX_VECTOR_SIZE`; unwinding that through a Rust callback is UB. The FFI
  audit found no input that reaches it (a list output grows by at most 32
  entries per row, or one per `(?N)` step, at most 1024), but do not add C
  API calls that can throw on user-sized input.

- **Panic containment**: every aggregate callback is generated by
  `quack_rs::aggregate_{update,combine,finalize}_callback!`, which runs the
  body under `catch_unwind` and reports a panic as a SQL error through
  `duckdb_aggregate_function_set_error`. That only works with
  `panic = "unwind"`; under `"abort"` (this crate's setting until 0.9.1) a
  panic kills the user's DuckDB process. `release_profile_keeps_panic_guards_effective`
  enforces the profile. Never write a raw `unsafe extern "C" fn` callback.

- **C API aggregates crash in some query shapes on DuckDB before 2.0.0
  (duckdb/duckdb#26109)**: DuckDB passes a one-element state vector with
  `count > 1`, because the C API cannot set `simple_update`, and
  `CAPIAggregateUpdate` does not flatten it. The extension cannot detect it
  (reading `states[1]` is the out-of-bounds read) or refuse it (no aggregate
  bind hook in the stable C API). Valgrind shows the invalid read; the process
  usually exits 139. Fixed upstream by duckdb/duckdb#26455 (`CAPIAggregateUpdate`
  now flattens the state vector, `aggregate_function-c.cpp` in DuckDB 2.0.0),
  which landed on the 2.0 line only: the heads of the `v1.4-andium` and
  `v1.5-variegata` branches still lack it as of 2026-10-29, and the community
  channel builds v1.4.5, v1.5.6 and 2.0.0. VALIDATED per release with
  `scripts/crash-shapes.sh` (`window_funnel`, 10 rows); on 2.0.0 the shapes are
  also asserted by `issue_26109_query_shapes_work_on_duckdb_2`
  (`tests/extension_load.rs`, all 8 functions, equal to `GROUP BY`),
  `scripts/compat-check.sh` and `scripts/nightly-smoke.py`:

  | Query shape | 1.3.2 | 1.4.4 | 1.5.0 | 1.5.6 | 2.0.0 |
  |---|---|---|---|---|---|
  | `f(.. ORDER BY ts)`, `OVER ()`, `UNBOUNDED..UNBOUNDED FOLLOWING` | crash | crash | crash | crash | ok |
  | `OVER (PARTITION BY g)`, `OVER (PARTITION BY 1)` (no `ORDER BY`) | crash | crash | ok | ok | ok |
  | `PARTITION BY g ORDER BY ts` with a bounded or running frame | ok | ok | ok | ok | ok |

  On 2.0.0 none crashes for any of the 8 functions, and the 7 event functions
  match the plain aggregate. Documented in README "Known limitations" and the
  FAQ. A related upstream defect: states of groups never reached (a
  `GROUP BY .. LIMIT`, an interrupt) are never destroyed, so `sequence_next_node`
  leaks the bytes it held; the README records the measured sizes.

- **DuckDB 2.0's C API v2 is not used**: it adds an aggregate bind callback,
  function docs/examples, variadic and named parameters, and order/distinct
  flags (`duckdb_v2_*`), but all 620 of its functions are `unstable: v2.0.0` and
  DuckDB 2.0.0 loads a V2 extension only on the exact release it was built for
  (`extension.cpp`: "no V2 function is stable yet"). quack-rs 0.19.0 does not
  generate the `<name>_init_c_api_v2` entry point either. The bind callback
  would not have refused the crash shapes anyway: it sees argument types and
  constants, not `ORDER BY` or `OVER`. V2 has no "stable prefix"; stability is
  a per-function lifecycle tag, so check for `stable` tags in a new
  `duckdb_v2.h` before reconsidering. The V1 C API has no setter for function
  descriptions, examples, categories or parameter names, so
  `duckdb_functions()` shows `col0, col1, ...` and blank descriptions
  (duckdb-behavioral#94); `COMMENT ON FUNCTION` fails for native functions.

- **Window usage needs no special registration**: `sessionize` is registered as
  a plain aggregate (`AggregateFunctionBuilder`); DuckDB drives any aggregate
  through update/combine/finalize when it appears in an `OVER` clause. The
  window-specific care lives in the state, not the registration: O(1) combine
  for segment trees, and `current_row_null` propagating from the right-hand
  segment so NULL-timestamp rows emit NULL.

### Performance Gotchas

- **Combine is the dominant cost**: DuckDB's segment tree calls combine O(n log n)
  times. Use `combine_in_place` with `Vec::extend_from_slice` (O(N) amortized) rather
  than `combine` returning a new Vec (O(N^2) total copies for left-fold chains).

- **Backtracking sequence search is quadratic; don't reintroduce it**: the old
  lazy depth-first search re-scanned the rest of the group from every start
  whenever a `.*` or a skipping time gate (`>=`, `>`, `!=`, `==`) could not
  complete (8.3-9.1 s at 32,000 events in one group; 32-35 s for
  `sequence_match_events`). `executor.rs` now computes, per consuming step, the
  positions from which the rest of the pattern can complete (one backward
  pass, O(s · n log n)) and walks forward greedily. The greedy walk equals the
  lazy search's first match because each gap step reaches a superset of
  positions from an earlier position — except the end-of-events position `n`
  for vacuous `<`/`<=` gates, which `tail_end` tracks separately. Any change
  must keep the differential proptests against `reference_nfa.rs` green.
  Its working memory is one bit per event per consuming step plus 8 bytes
  per event, outside DuckDB's `memory_limit`; the parser caps patterns at
  1024 steps and every large allocation goes through `try_reserve`, so
  exhaustion is a SQL error, not an abort.

- **`strict_once` matching must stay off SipHash**: the bipartite-matching
  cache in `window_funnel.rs` is hit once per (step, event); with
  `std`'s default hasher it was ~58% of `strict_once` time (perf). Keys are
  packed `u64`s with `PackedKeyHasher`; cheap answers (empty range, more
  steps than events, one-step range) skip the cache entirely.

- **Running window frames are quadratic for event-collecting states**:
  DuckDB's segment tree combines a full copy of the frame's events into each
  row's state and finalizes ~2,048 such states at once (40,000-row running
  frame: 11.1 s, 1.25 GB). The C API has no window callback, so this cannot
  be fixed in the extension; it is documented in the README and FAQ.

- **Presorted detection before sort**: DuckDB often provides timestamp-ordered data.
  An O(n) `windows(2).all()` check before `sort_unstable_by_key` avoids O(n log n)
  for the common case.

- **`sequence_next_node` keeps values in one byte buffer per state**: each event
  is a 24-byte `Copy` record with a `(start, len)` range into the buffer, and a
  value equal to the previous event's is stored once. This replaced an
  `Arc<str>` per event (0.10.1), whose allocation was infallible (an abort
  under memory pressure instead of a SQL error) and was lost wholesale when
  DuckDB abandons a grouped scan. Measured on DuckDB 2.0.0, 10M rows in 100k
  groups, 4 threads: CPU time median 5.08 s to 2.66 s. Growth goes through
  `try_push` / `try_combine_in_place`, which return `StoreError` that the FFI
  reports as a SQL error; do not reintroduce a per-event allocation.

- **Radix sort is wrong for embedded-key structs**: LSD radix sort (8-bit, 8 passes)
  on 16-byte `Event` structs was 4.3x slower than pdqsort at 100M elements due to
  cache-hostile scatter patterns. Comparison sorts win for embedded keys.

- **Branchless is wrong for well-predicted branches**: `CMOV` always evaluates both
  paths. For 90/10 splits (like session boundary checks), the branch predictor
  achieves near-perfect accuracy and branching is faster.

### ClickHouse Semantics

- **`windowFunnel` semantics are subtle; port, don't reinterpret**: one entry
  per true condition (an event can fill several steps, the entry included),
  `strict_order` is broken by condition-less events and by a step arriving
  before its predecessor, `strict_deduplication` stops on any repeat of a
  reached level (even an expired one), `allow_reentry` only skips early steps.
  The pre-0.10 greedy scan got six of these wrong while its docs claimed
  parity; differential testing found them.

- **`sequenceNextNode` always returns `Nullable(String)`**: Not polymorphic.
  Simplifies FFI to a single VARCHAR return type.

- **`sequence_next_node` follows ClickHouse** (differentially tested): a single
  anchor per `base` (`head`/`tail` = the literal first/last event, which must
  satisfy `base_condition`; `first_match`/`last_match` = first/last event
  satisfying base AND `event1`), chains match **consecutive** sorted events
  only, failed chains are not retried, `forward`+`tail` / `backward`+`head` are
  errors. Ties sort by `(timestamp, value, base_condition, conditions)`.

- **Sequence fast paths must preserve adjacency**: `fast_wildcard` is only
  valid when no two `(?N)` steps are adjacent; `(?1)(?2).*(?3)` must go to the
  general matcher. An empty match in `sequence_count` must still advance one event.

- **Multi-step funnel advancement**: an event satisfying several conditions
  fills several steps, including the entry step; `strict_once` and
  `strict_increase` limit it to one.

### Build & Community Extension

- **Library `[lib] name` must match extension name**: The community Makefile expects
  `lib{name}.so`. The crate name `duckdb-behavioral` produces `libduckdb_behavioral.so`.
  Fix: `[lib] name = "behavioral"` in Cargo.toml.

- **`extension-ci-tools` submodule is required**: Initialize before any `make` invocation.
  Provides Makefile includes, metadata script, and test runner.

- **Local `interval_to_micros` rejects months intentionally**: quack-rs converts
  months to 30 days. The local version returns `None` for month-based intervals
  because 28-31 day ambiguity is unacceptable for behavioral analytics.

- **WASM compiles but is not yet shipped**: since quack-rs 0.14.0 the crate
  compiles for `wasm32-unknown-emscripten` (DuckDB-WASM), enforced by the
  `wasm-check` CI job. `description.yml` still excludes the wasm platforms:
  flipping them on requires verifying the emscripten link + DuckDB-WASM load,
  which the community-extensions CI can serve as the test bed for in a
  dedicated submission.

### Testing Notes

- **E2E testing is non-negotiable**: Unit tests once passed at 375/375 while the
  extension was completely broken: SEGFAULT on load, 6 of 7 functions silently
  failing to register, and `window_funnel` returning wrong results. All three bugs
  were in the FFI layer that unit tests don't exercise.

- **SQL test values must be validated against actual DuckDB output**: Session IDs
  are 1-indexed (not 0-indexed), LIST(TIMESTAMP) uses single-quoted format,
  `sequence_next_node` requires base_condition AND event1 conjunction.

- **Event filtering in tests**: `update()` filters events where `has_any_condition()`
  is false. Test "gap" events must have at least one true condition or they're silently
  dropped.
