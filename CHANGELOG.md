# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.8.0] - 2026-06-12

### Added

- **`window_funnel_events` function** — `LIST(TIMESTAMP)` of the best funnel
  chain: one timestamp per matched step, in match order (list length always
  equals `window_funnel`'s step count for the same arguments). Companion to
  `window_funnel` for funnel debugging and step-to-step latency analysis;
  extension beyond ClickHouse. Shares `WindowFunnelState` and the
  update/combine FFI callbacks; the greedy scan is generic over a zero-sized
  step recorder so `window_funnel`'s hot path is unchanged.
- **`behavioral_version()` scalar** — returns the loaded extension version
  (the first scalar function; registered via quack-rs `ScalarFunctionBuilder`).
- **Strict configuration validation** — invalid configuration now aborts the
  query with a descriptive SQL error (via
  `duckdb_aggregate_function_set_error`) instead of silently producing wrong
  results or NULL: unknown `window_funnel` mode strings (message lists all
  valid modes), month-based or negative INTERVAL windows/gaps, malformed
  sequence patterns (the parser's position-annotated message), and unknown
  `sequence_next_node` direction/base values. NULL configuration parameters
  remain lenient.
- **`wasm-check` CI job** — the crate compiles for `wasm32-unknown-emscripten`
  (DuckDB-WASM) since quack-rs 0.14.0; CI now enforces it. The community
  descriptor still excludes wasm platforms pending a verified emscripten
  link + load.

### Fixed

- **`sequence_next_node` now matches ClickHouse exactly** (verified against
  `AggregateFunctionSequenceNextNode.cpp`). Three divergences fixed:
  `head`/`tail` anchor at the literal first/last event (which must satisfy
  the base condition) rather than scanning for one; the event chain must
  match **consecutive** events (interleaved non-matching events break it, and
  a failed chain is not retried at other anchors); events sort by
  `(timestamp, value)` so same-timestamp results are deterministic.
- **Timestamp gap arithmetic saturates instead of wrapping** — with DuckDB's
  `±infinity` timestamps (`±i64::MAX` internally), the gap computations in
  `sessionize` (update + combine), `window_funnel`'s window check, and the
  pattern executor's `(?t…)` elapsed-time evaluation previously overflowed
  and wrapped in release builds, silently treating infinitely distant events
  as adjacent. All sites now use `saturating_sub`.
- **Pattern time constraints reject numbers above `i64::MAX`** — values in
  `(i64::MAX, u64::MAX]` previously wrapped negative through an unchecked
  cast, inverting the constraint (e.g. `(?t>=18446744073709551615)` became
  `(?t>=-1)`). Now a position-annotated pattern error.
- **NFA exploration budget no longer causes silent false negatives** — the
  fixed 10,000-iteration cap could make complex patterns (time constraints,
  `.`) report "no match" on groups with more than a few thousand
  non-matching events even when a match existed. The budget now scales with
  input size (`8 × events × steps`), consecutive `.*` wildcards are collapsed
  at parse time (they are semantically one wildcard but each copy multiplied
  the NFA branching factor), and if a truly adversarial pattern still
  exhausts the budget the query fails loudly with a descriptive error
  instead of silently returning a wrong result.

### Changed

- **Deterministic results under parallel aggregation** — shared event sorting
  now uses the full `(timestamp, conditions)` key (with a matching presorted
  check), so `window_funnel`, `window_funnel_events`, `sequence_match`,
  `sequence_count`, and `sequence_match_events` produce identical results
  regardless of thread count or physical row order. Verified by an
  integration probe that hashes results across `threads=1/4` and reversed
  insertion order. (ClickHouse's `windowFunnel` achieves the same via
  stable-sorted `(timestamp, event_index)` pairs.)
- **`sessionize` FFI migrated to quack-rs `AggregateFunctionBuilder`** — the
  last hand-rolled raw `libduckdb-sys` module is gone; every function now
  registers through the `Registrar` trait. Behavior-preserving (identical
  C-API call sequence, default NULL handling); adds the combine-harness tests
  sessionize never had.
- **Dev/test builds link a prebuilt libduckdb** (quack-rs
  `bundled-test-prebuilt` + `DUCKDB_DOWNLOAD_LIB=1`) instead of compiling
  DuckDB's C++ tree from source — cold test setup drops from ~25 minutes to
  the Rust wrapper crates only. Release builds are untouched (verified: no
  libduckdb `NEEDED`/`RUNPATH` in the shipped `.so`).
- **BREAKING (0.x minor)**: `ffi::sessionize::register_sessionize` now takes
  `&impl Registrar` instead of a raw `duckdb_connection`.

### Tests

- 482 unit tests (was 453) and 15 in-process integration tests (was 7),
  including: adversarial probes for `±infinity` timestamps and oversized
  time constraints, a ClickHouse-semantics matrix for all eight
  `sequence_next_node` direction/base combinations, parallel-combine
  determinism hashing, windowed `window_funnel` usage, and SQL error-path
  assertions for all validation errors. 8 E2E SQL files with 76 queries.


## [0.7.0] - 2026-06-07

### Changed

- **quack-rs v0.14.0** — upgraded from v0.13.0. Every public API this crate uses
  (`AggregateFunctionSetBuilder`, `FfiState`, `VectorReader`/`VectorWriter`,
  `ListVector`, `LogicalType::list`, `returns_logical`, `AggregateTestHarness`,
  `Connection`/`Registrar`, `entry_point_v2!`) is unchanged: the extension
  compiles against the new SDK with **zero changes to existing source**. v0.14.0
  is purely additive — it adds `wasm32-unknown-emscripten` (DuckDB-WASM) support,
  a `bundled-test-prebuilt` test feature, and `InMemoryDb::open_unsigned()`. MSRV
  stays 1.87; DuckDB stays v1.5.3 (`libduckdb-sys`/`duckdb` `1.10503.1`, already
  the latest).

### Added

- **In-process extension-load integration test** (`tests/extension_load.rs`, 7
  tests). Builds the real release `cdylib`, appends the DuckDB metadata footer
  (a Rust port of `append_extension_metadata.py`), and `LOAD`s it into an
  in-memory DuckDB via quack-rs 0.14.0's
  `testing::InMemoryDb::open_unsigned()`, then exercises **all seven functions**
  through live SQL. This runs inside `cargo test` with no external `duckdb` CLI
  and closes the long-standing gap where unit tests could pass while the FFI
  registration path was broken (the segfault-on-load / silent-non-registration /
  wrong-result class of bugs). Adds the `quack-rs` `bundled-test` dev-feature,
  which unifies with the existing `duckdb/bundled` dev-dependency — no extra
  DuckDB build.

### Security

- **Dependency refresh via `cargo update`** — notably `tar` 0.4.45 → 0.4.46
  (resolves **GHSA-3pv8-6f4r-ffg2**, "PAX header desynchronization"; `tar` is a
  build-time dependency of `libduckdb-sys`), plus `cc` 1.2.61 → 1.2.63 and
  `shlex` 1.3.0 → 2.0.1, and routine bumps across the dev/transitive tree
  (`arrow` 58.1 → 58.3, `tokio`, `wasm-bindgen`, `zerocopy`, …). All direct
  dependencies are at their latest versions; the sole held-back transitive crate
  is `comfy-table` (constrained by the `duckdb` dev-dependency). All bumped
  crates declare an MSRV ≤ 1.85, so the project MSRV stays 1.87.
- **`deny.toml`** — added a tightly-scoped `CC0-1.0` license exception for
  `tiny-keccak`, a phantom `Cargo.lock` entry behind `ahash`'s optional,
  never-enabled `compile-time-rng` feature (absent from every buildable graph and
  from the shipped `.so`). This lets current `cargo-deny` releases pass the
  license gate, future-proofing CI against the pinned action updating.

### Documentation

- Updated every version reference to the `v0.7.0` / quack-rs `v0.14.0` baseline
  (DuckDB `v1.5.3` and MSRV `1.87` unchanged) across `README.md`, `CLAUDE.md`,
  `SECURITY.md`, `description.yml`, `scripts/setup.sh`, `docs/src/**.md`, the
  issue/PR templates, and the community-submission workflow, and documented the
  new in-process integration-test layer.

## [0.6.0] - 2026-05-24

### Changed

- **DuckDB v1.5.3 support** — upgraded `libduckdb-sys` from `1.10502.0` to
  `1.10503.1` and `duckdb` (dev) from `1.10502.0` to `1.10503.1`. Restores
  community-extension compatibility with the current DuckDB release line (the
  registry had de-listed the extension for targeting an older version)
- **quack-rs v0.13.0** — upgraded from v0.12.0. The public APIs used by this
  crate (`AggregateFunctionSetBuilder`, `FfiState`, `VectorReader`/`VectorWriter`,
  `ListVector`, `LogicalType::list`, `AggregateTestHarness`,
  `Connection`/`Registrar` trait, `entry_point_v2!`) are unchanged: the crate
  compiles against the new SDK with **zero source changes**. New upstream
  capabilities in this range — the `error_data`, `expression`, `file_system`,
  `appender`, `selection_vector`, and `instance_cache` modules, plus
  `TypeId::Variant` / `TypeId::Geometry` behind the `duckdb-1-5-3` feature —
  are available but not consumed: the extension's aggregate-only surface area
  doesn't exercise them
- **MSRV bumped from 1.86 to 1.87** — quack-rs v0.13.0 corrected its declared
  MSRV to 1.87.0 to match `libduckdb-sys`. CI's `cargo check --all-targets`
  MSRV gate now pins 1.87
- E2E tests now run against DuckDB v1.5.3 CLI (previously v1.5.2)

### Fixed

- **Version drift in `scripts/setup.sh`** — the DuckDB CLI download URLs
  hardcoded `v1.5.2` in four places instead of deriving from the
  `DUCKDB_RELEASE_VERSION` constant. Refactored to build every URL from the
  single constant, removing a recurring source of stale references

### Documentation

Audited and updated all version/MSRV references across `README.md`,
`CLAUDE.md`, `CONTRIBUTING.md`, `SECURITY.md`, all of `docs/src/**.md`,
all `.github/workflows/*.yml`, `Makefile`, `description.yml`, and the
issue templates so every cross-reference is consistent with the
`v0.6.0` / DuckDB `v1.5.3` / quack-rs `v0.13.0` / MSRV `1.87` baseline.

A full accuracy pass over every doc against the source also corrected
several pre-existing defects unrelated to the version bump:

- **Broken metadata-append examples** in `docs/src/getting-started.md` and
  `docs/src/contributing.md` stamped `-dv v1.2.0 ... --abi-type C_STRUCT`,
  which produces an extension DuckDB 1.5.x rejects on load. Corrected to the
  real `-dv v1.5.3 ... --abi-type C_STRUCT_UNSTABLE` convention.
- **CI job lists** in `README.md` and `docs/src/engineering.md` claimed "13
  CI jobs" but enumerated only 12 (omitting `ci-gate`); both now list all 13.
- **`sequence_next_node` bases** in `docs/src/quick-reference.md` wrongly
  described `head`/`tail` as aliases of `first_match`/`last_match`; they are
  four distinct strategies (corrected to match the function reference).
- **Spurious `rand` dev-dependency** removed from the `docs/src/operations/security.md`
  dependency table — `rand` is only transitive; benchmarks generate data
  deterministically.
- **`attest-build-provenance@v3`** in `docs/src/operations/security.md`
  corrected to `@v4` (matching `release.yml`), and a stale `v0.2.0`
  verification example bumped to `v0.6.0`.
- Cleared stale "DuckDB 1.5.1" and "C API version v1.2.0" references in
  `README.md`, `docs/src/index.md`, `docs/src/engineering.md`,
  `docs/src/faq.md`, and `docs/src/getting-started.md`.
- Aligned the `AggregateTestHarness` config-propagation claim in `CLAUDE.md`
  to "all 6 aggregate functions" (across 5 FFI test modules).

## [0.5.0] - 2026-05-01

### Changed

- **DuckDB v1.5.2 support** — upgraded `libduckdb-sys` from `1.10501.0` to
  `1.10502.0` and `duckdb` (dev) from `1.10501.0` to `1.10502.0`
- **quack-rs v0.12.0** — upgraded from v0.7.1. Public APIs used by this crate
  (`AggregateFunctionSetBuilder`, `FfiState`, `VectorReader`/`VectorWriter`,
  `ListVector`, `LogicalType::list`, `AggregateTestHarness`,
  `Connection`/`Registrar` trait, `entry_point_v2!`) are unchanged. New
  capabilities introduced upstream during this range — `StructReader`/
  `StructWriter`, `ChunkWriter`, `MapVector`, `Value` RAII wrapper,
  `scalar_callback!` / `table_scan_callback!` panic-safe macros, expanded
  `LogicalType` introspection, `tls`/`warning`/`secrets` modules — are
  available but not consumed: the extension's aggregate-only surface area
  doesn't exercise them
- **MSRV bumped from 1.84.1 to 1.86** — required by the new `libduckdb-sys`
  / `duckdb` (1.85.1) and `criterion` 0.8.2 (1.86) crate metadata. CI's
  `cargo check --all-targets` MSRV gate enforces 1.86
- **`extension-ci-tools` submodule** updated to latest upstream
- **CI dependency updates** — `EmbarkStudios/cargo-deny-action` v2.0.15→v2.0.17,
  `taiki-e/install-action` →v2.75.27, `actions/deploy-pages` v4.0.5→v5.0.0,
  `actions/upload-pages-artifact` v4.0.0→v5.0.0,
  `actions/upload-artifact` v7.0.0→v7.0.1, `github/codeql-action` v4.33.0→v4.35.3,
  `codecov/codecov-action` v5.5.2→v6.0.0,
  `softprops/action-gh-release` v2.6.1→v3.0.0
- E2E tests now run against DuckDB v1.5.2 CLI (previously v1.5.1)
- **Transitive crate refresh** via `cargo update` — notably `rand` 0.8.5→0.8.6,
  `rustls-webpki` 0.103.10→0.103.13. `cargo deny check` reports clean
  advisories, bans, licenses, and sources

### Fixed

- **`scripts/setup.sh` was using a wrong `-dv` value for DuckDB metadata**
  — the `C_API_VERSION="v1.2.0"` constant produced an extension that
  DuckDB v1.5.x rejects with "file was built specifically for DuckDB
  version 'v1.2.0'". Replaced with `DUCKDB_RELEASE_VERSION="v1.5.2"`
  matching the Makefile `TARGET_DUCKDB_VERSION` convention, and aligned
  `--abi-type` to `C_STRUCT_UNSTABLE` (matches `make release`)
- **Stale `EXT_VERSION` in `scripts/setup.sh`** — was pinned at `v0.1.0`,
  now tracks the current release version (`v0.5.0`)
- **Stale DuckDB CLI install URL in `scripts/setup.sh`** — was pinned at
  `v1.5.0`, now installs `v1.5.2` to match `TARGET_DUCKDB_VERSION`
- **Dead `as idx_t` round-trip cast** in `src/ffi/sequence_next_node.rs`
  finalize callback (the `idx` was cast to `idx_t` then immediately back
  to `usize` four times)
- **`deny.toml` license allowances** that no longer matched any picked
  license in the dep graph (`BSD-2-Clause`, `CC0-1.0`); `cargo deny check`
  now reports zero `license-not-encountered` warnings

### Documentation

Audited and updated all version/MSRV references across `README.md`,
`CLAUDE.md`, `CONTRIBUTING.md`, `SECURITY.md`, all of `docs/src/**.md`,
all `.github/workflows/*.yml`, `Makefile`, `description.yml`, and the
issue templates so every cross-reference is consistent with the
`v0.5.0` / DuckDB `v1.5.2` / quack-rs `v0.12.0` / MSRV `1.86` baseline.

### Subsumes

This release closes the following dependabot pull requests, which were
all bumping deps to versions at or below those landed here: #58, #61,
#62, #64, #65, #66, #67.

## [0.4.0] - 2026-03-28

### Changed

- **DuckDB v1.5.1 support** — upgraded `libduckdb-sys` from `1.10500.0` to
  `1.10501.0` and `duckdb` (dev) from `1.10500.0` to `1.10501.0`. This
  restores community extension compatibility (previously only supported v1.4.4)
- **quack-rs v0.7.1** — upgraded from v0.6.0. Includes ARM64/aarch64 build fix,
  five new `duckdb-1-5` feature modules (catalog, client_context, config_option,
  copy_function, table_description), new `TypeId` variants, and
  `ScalarFunctionBuilder` enhancements (varargs, volatile, bind, init)
- **`entry_point_v2!` migration** — entry point now uses `quack_rs::entry_point_v2!`
  with `&Connection` / `Registrar` trait instead of raw `duckdb_connection`. All 6
  aggregate functions register via `con.register_aggregate_set(builder)` with proper
  `Result` error propagation. `sessionize` uses `con.as_raw_connection()` for window
  function FFI
- **CI dependency updates** — `Swatinem/rust-cache` v2.8.2→v2.9.1,
  `github/codeql-action` v4.32.6→v4.33.0, `actions/download-artifact`
  v8.0.0→v8.0.1, `taiki-e/install-action` updated,
  `softprops/action-gh-release` v2.5.0→v2.6.1
- **Transitive dependency updates** — `tar` 0.4.44→0.4.45
- E2E tests now run against DuckDB v1.5.1 CLI (previously v1.5.0)

---

## [0.3.0] - 2026-03-28

### Added

- **SQL Cookbook** documentation with 25+ practical recipes organized by function
  category (funnels, sessions, retention, patterns, user flows, combined analysis)
- **Quick Reference** one-page cheat sheet covering all functions, pattern syntax,
  NULL handling, limits, and common translations
- **6 standalone example SQL scripts** in `examples/` directory — self-contained,
  runnable demonstrations of each function category
- **Developer quality check script** (`scripts/check.sh`) — runs all quality checks
  (fmt, clippy, test, doc, bench) with colored output and `--quick` mode
- **Question issue template** (`.github/ISSUE_TEMPLATE/question.md`) for usage
  questions with documentation checklist
- Enhanced mdBook CSS: admonition/callout styling, heading highlight animation,
  smooth scrolling, better link styling, keyboard shortcut styling, horizontal
  rule polish
- 18 new `AggregateTestHarness` unit tests for combine config-propagation
  across all 5 aggregate functions (435 → 453 tests)

### Changed

- **README overhaul**: Added documentation badge, nav bar, "Choosing the Right
  Function" guide, expanded examples section (8 patterns), integrations section
  (Python, Node.js, dbt, Parquet), and verification one-liners in Quick Start
- **CONTRIBUTING.md rewrite**: Added project architecture diagram, quality check
  table, PR checklist, documentation files reference, and `scripts/check.sh`
  instructions
- **mdBook SUMMARY.md**: Added SQL Cookbook and Quick Reference to Getting Started
  section
- Migrated FFI layer to `quack-rs` v0.6.0 SDK for safe state management, vector
  I/O, and function set registration
- Entry point (`src/lib.rs`) now uses `quack_rs::entry_point!` macro instead
  of ~80 lines of hand-rolled unsafe code
- MSRV bumped from 1.80 to 1.84.1 (required by quack-rs)
- FFI modules use `FfiState<T>`, `VectorReader`, `VectorWriter` from quack-rs
- LIST output uses `ListVector` + `VectorWriter` instead of raw pointer arithmetic
- VARCHAR output uses `VectorWriter::write_varchar()` instead of `CString` + raw FFI
- LIST type construction uses `LogicalType::list()` instead of raw `duckdb_create_list_type`
- `retention` registration: ~45 lines of raw loop → 15-line builder chain
- `sequence_match_events` registration: ~55 lines of raw loop → 15-line builder chain
- `sessionize` FFI remains hand-rolled (window function limitation in quack-rs)

### Removed

- Hand-rolled `read_varchar()` helper in `sequence_next_node` (replaced by
  `VectorReader::read_str()`)
- Raw `duckdb_list_vector_*` pointer arithmetic in retention and
  sequence_match_events (replaced by `ListVector` wrappers)
- Raw `duckdb_create_aggregate_function_set` loops in retention and
  sequence_match_events (replaced by `AggregateFunctionSetBuilder`)
- `CString` sanitization in sequence_next_node (replaced by `write_varchar`)

---

## [0.2.0] - 2026-02-15

### Added

- **`sequence_next_node` function** with full direction (forward/backward) and
  base (head/tail/first_match/last_match) support
- **`sequence_match_events` function** returning matched condition timestamps
  as `LIST(TIMESTAMP)`
- **32-condition support** for all variadic functions, matching ClickHouse's
  limit (expanded from 8 to 32)
- **6 combinable `window_funnel` modes**: `strict`, `strict_order`,
  `strict_deduplication`, `strict_increase`, `strict_once`, `allow_reentry`
- **NFA fast-path classification** dispatching common pattern shapes to
  specialized O(n) linear scans (39--61% improvement for `sequence_count`)
- **Presorted detection** skipping O(n log n) sort for timestamp-ordered input
- **27 end-to-end SQL tests** against real DuckDB v1.4.4 CLI
- **26 property-based tests** (proptest) verifying algebraic properties
- GitHub Pages documentation site (mdBook) with function reference, use cases,
  FAQ, architecture, and performance documentation
- CI/CD pipeline with structured test output (`cargo-nextest`), job summaries,
  E2E workflow, SemVer release validation, 4-platform release builds with
  provenance attestation
- Community extension infrastructure (`description.yml`, `Makefile`,
  `extension-ci-tools` submodule)
- Listed in [DuckDB Community Extensions](https://github.com/duckdb/community-extensions/tree/main/extensions/behavioral)
  repository ([PR #1306](https://github.com/duckdb/community-extensions/pull/1306),
  merged 2026-02-15). Install with `INSTALL behavioral FROM community; LOAD behavioral;`
- `'timestamp_dedup'` mode string for the extension-only timestamp-based
  deduplication mode in `window_funnel`
- ClickHouse parity scope table and known semantic differences documentation

### Changed

- **`'strict_deduplication'` mode mapping**: Now correctly maps to `STRICT`
  (0x01), matching ClickHouse where `'strict'` and `'strict_deduplication'`
  are aliases. The timestamp-based dedup behavior is now available under
  `'timestamp_dedup'`.
- **BREAKING**: All public state structs marked `#[non_exhaustive]` to allow
  future field additions without semver-breaking changes. Affected structs:
  `SessionizeState`, `SessionizeBoundaryState`, `RetentionState`,
  `WindowFunnelState`, `SequenceState`, `SequenceNextNodeState`,
  `NextNodeEvent`, `Event`, `CompiledPattern`, `PatternError`, `MatchResult`.
  Use the provided `new()` constructors instead of struct literal syntax.
- **Custom C entry point** (`behavioral_init_c_api`) replaces fragile
  `duckdb::Connection` extraction, using `duckdb_connect` directly from
  `duckdb_extension_access`
- **`Arc<str>` for `Send+Sync` safety** in `sequence_next_node` (replaced
  `Rc<str>`)
- **Defensive FFI boolean reading**: `*const bool` replaced with `*const u8`
  across all FFI modules for ABI safety
- **No-panic FFI entry point**: removed `.unwrap()` on DuckDB function pointers
  in `lib.rs`
- Runtime dependency reduced from 3 crates to 1 (`libduckdb-sys` only)

### Fixed

- SEGFAULT on extension load (incorrect pointer arithmetic in connection
  extraction)
- 6 of 7 functions silently failing to register (missing
  `duckdb_aggregate_function_set_name` call)
- `window_funnel` returning incorrect results (combine not propagating
  `window_size_us` and `mode`)
- `sequence_next_node` NULL output producing `\0\0\0\0` instead of NULL
  (missing `duckdb_vector_ensure_validity_writable` call)
- Interior null byte handling in `sequence_next_node` FFI output
- `retention` finalize calling `ensure_validity_writable` inside loop
  (hoisted outside)

## [0.1.0] - 2026-02-14

### Added

- Initial release with 7 behavioral analytics functions
- `sessionize` -- window function assigning session IDs based on inactivity gaps
- `retention` -- aggregate function for cohort retention analysis
- `window_funnel` -- conversion funnel step tracking within a time window
- `sequence_match` -- NFA-based pattern matching over event sequences
- `sequence_count` -- count non-overlapping pattern occurrences
- `sequence_match_events` -- return timestamps of matched pattern steps
- `sequence_next_node` -- find next/previous event value after pattern match
- Complete ClickHouse behavioral analytics function parity
- 453 unit tests + 1 doc-test
- 27 E2E SQL integration tests
- Criterion.rs benchmarks for all 7 functions (up to 1 billion elements)
- 88.4% mutation testing kill rate (cargo-mutants)
- MIT license

[Unreleased]: https://github.com/tomtom215/duckdb-behavioral/compare/v0.8.0...HEAD
[0.8.0]: https://github.com/tomtom215/duckdb-behavioral/compare/v0.6.0...v0.8.0
[0.7.0]: https://github.com/tomtom215/duckdb-behavioral/commit/f50cb24
[0.6.0]: https://github.com/tomtom215/duckdb-behavioral/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/tomtom215/duckdb-behavioral/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/tomtom215/duckdb-behavioral/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/tomtom215/duckdb-behavioral/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/tomtom215/duckdb-behavioral/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/tomtom215/duckdb-behavioral/releases/tag/v0.1.0
