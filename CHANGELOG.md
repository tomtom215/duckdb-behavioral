# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Migrated FFI layer to `quack-rs` v0.5.0 (pre-release, git dependency) SDK
  for safe state management, vector I/O, and function set registration
- All 6 aggregate functions now use `AggregateFunctionSetBuilder` for registration
- `retention` and `sequence_match_events` use `returns_logical(LogicalType::list(...))`
  — eliminating the last raw function set registration code
- 18 new `AggregateTestHarness` unit tests for combine config-propagation
  across all 5 aggregate functions (435 → 453 tests)

### Changed

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

[Unreleased]: https://github.com/tomtom215/duckdb-behavioral/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/tomtom215/duckdb-behavioral/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/tomtom215/duckdb-behavioral/releases/tag/v0.1.0
