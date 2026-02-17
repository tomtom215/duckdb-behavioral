# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Listed in [DuckDB Community Extensions](https://github.com/duckdb/community-extensions/tree/main/extensions/behavioral)
  repository ([PR #1306](https://github.com/duckdb/community-extensions/pull/1306),
  merged 2026-02-15). Install with `INSTALL behavioral FROM community; LOAD behavioral;`
- Updated all documentation to reflect community extension availability as the
  recommended installation method
- `'timestamp_dedup'` mode string for the extension-only timestamp-based
  deduplication mode in `window_funnel`
- Mandatory session protocol and anti-pattern list in CLAUDE.md
- ClickHouse parity scope table and known semantic differences documentation

### Changed

- **`'strict_deduplication'` mode mapping**: Now correctly maps to `STRICT`
  (0x01), matching ClickHouse where `'strict'` and `'strict_deduplication'`
  are aliases. The timestamp-based dedup behavior is now available under
  `'timestamp_dedup'`.

### Fixed

- GitHub Pages theme switcher: replaced fragile programmatic click on hidden
  mdBook theme list with direct localStorage and class manipulation, preventing
  breakage across mdBook versions
- iOS Safari theme toggle: added touch-action:manipulation for fast taps and
  300ms debounce to prevent double-fire from touch + click events
- Tables cut off on mobile: added JS table wrappers (display:table inside
  scrollable div), visible WebKit scrollbar styling, changed parent overflow
  from hidden to clip so child scroll containers work properly
- Mermaid diagrams squished on narrow viewports: removed max-width:100% on
  SVGs below 768px so diagrams scroll horizontally instead of scaling down
  until text becomes unreadable and subgraph titles get clipped
- Print button: overridden to call window.print() on current page instead of
  navigating to print.html, preventing print dialog from re-triggering on
  page refresh
- Added @media print styles for proper table/diagram rendering when printing

## [0.2.0] - 2026-02-14

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

### Changed

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
- 434 unit tests + 1 doc-test
- 27 E2E SQL integration tests
- Criterion.rs benchmarks for all 7 functions (up to 1 billion elements)
- 88.4% mutation testing kill rate (cargo-mutants)
- MIT license

[Unreleased]: https://github.com/tomtom215/duckdb-behavioral/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/tomtom215/duckdb-behavioral/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/tomtom215/duckdb-behavioral/releases/tag/v0.1.0
