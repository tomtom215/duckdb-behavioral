<p align="center">
  <img src="assets/banner.svg" alt="duckdb-behavioral" width="600">
</p>

<p align="center">
  <strong>Behavioral analytics functions for DuckDB, inspired by ClickHouse.</strong>
</p>

<p align="center">
  <a href="https://github.com/tomtom215/duckdb-behavioral/actions/workflows/ci.yml"><img src="https://github.com/tomtom215/duckdb-behavioral/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/tomtom215/duckdb-behavioral/actions/workflows/e2e.yml"><img src="https://github.com/tomtom215/duckdb-behavioral/actions/workflows/e2e.yml/badge.svg" alt="E2E Tests"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-MIT-blue.svg" alt="License: MIT"></a>
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/MSRV-1.80-blue.svg" alt="MSRV: 1.80"></a>
</p>

Provides `sessionize`, `retention`, `window_funnel`, `sequence_match`,
`sequence_count`, `sequence_match_events`, and `sequence_next_node` as a loadable
[DuckDB](https://duckdb.org/) extension written in Rust. **Complete
[ClickHouse](https://clickhouse.com/docs/en/sql-reference/aggregate-functions/parametric-functions)
behavioral analytics parity.**

> **Personal Project Disclaimer**: This is a personal project developed on my own
> time. It is not affiliated with, endorsed by, or related to my employer or
> professional role in any way.

---

### AI-Assisted Development Disclosure

This project was developed with assistance from Anthropic's Claude (AI-assisted
programming). In the interest of full transparency and academic rigor, the
following safeguards ensure that AI assistance does not compromise correctness,
reproducibility, or trustworthiness:

- **434 unit tests + 1 doc-test** covering all functions, edge cases, combine
  associativity, property-based testing (proptest), and mutation-testing-guided
  coverage. All tests run in under 1 second via `cargo test`.
- **27 end-to-end SQL tests** against a real DuckDB v1.4.4 instance validating
  the complete chain from extension loading through SQL execution to correct
  results, including NULL inputs, empty tables, all 6 funnel modes, 5+
  conditions, and all 8 direction/base combinations.
- **Criterion.rs benchmarks** with 95% confidence intervals, 3+ runs per
  measurement, and documented methodology in [`PERF.md`](PERF.md). Every
  performance claim is reproducible on commodity hardware.
- **88.4% mutation testing kill rate** (130 caught / 17 missed) via
  `cargo-mutants`, systematically verifying that tests detect real faults.
- **Zero clippy warnings** under pedantic, nursery, and cargo lint groups.
- **Deterministic, reproducible builds** — pinned dependencies (`libduckdb-sys
  = "=1.4.4"`), MSRV 1.80 verified in CI, and release profile with LTO and
  single codegen unit.
- **Every optimization session** documents hypothesis, technique, measured
  before/after data with confidence intervals, and negative results are
  reported honestly (5 negative results documented in PERF.md).
- **All source code is publicly auditable** under the MIT license.

AI tooling was used as an accelerator for implementation. All correctness
guarantees rest on automated testing, reproducible benchmarks, and transparent
documentation — not on AI output being assumed correct.

---

## Table of Contents

- [Quick Start](#quick-start)
- [Functions](#functions)
- [Performance](#performance)
- [Community Extension](#community-extension)
- [Quality](#quality)
- [ClickHouse Parity Status](#clickhouse-parity-status)
- [Building](#building)
- [Development](#development)
- [Documentation](#documentation)
- [Requirements](#requirements)
- [License](#license)

## Quick Start

```sql
-- Install from the DuckDB Community Extensions repository
INSTALL behavioral FROM community;
LOAD behavioral;
```

Or build from source:

```bash
# Build the extension
cargo build --release

# Load in DuckDB (locally-built extensions require -unsigned)
duckdb -unsigned -cmd "LOAD 'target/release/libbehavioral.so';"
```

```sql
-- Assign session IDs with a 30-minute inactivity gap
SELECT user_id, event_time,
  sessionize(event_time, INTERVAL '30 minutes') OVER (
    PARTITION BY user_id ORDER BY event_time
  ) as session_id
FROM events;

-- Track conversion funnel steps within a 1-hour window
SELECT user_id,
  window_funnel(INTERVAL '1 hour', event_time,
    event_type = 'page_view',
    event_type = 'add_to_cart',
    event_type = 'purchase'
  ) as furthest_step
FROM events
GROUP BY user_id;
```

## Functions

| Function | Signature | Returns | Description |
|---|---|---|---|
| `sessionize` | `(TIMESTAMP, INTERVAL)` | `BIGINT` | Window function assigning session IDs based on inactivity gaps |
| `retention` | `(BOOLEAN, BOOLEAN, ...)` | `BOOLEAN[]` | Cohort retention analysis |
| `window_funnel` | `(INTERVAL [, VARCHAR], TIMESTAMP, BOOLEAN, ...)` | `INTEGER` | Conversion funnel step tracking with [6 combinable modes](https://tomtom215.github.io/duckdb-behavioral/functions/window-funnel.html) |
| `sequence_match` | `(VARCHAR, TIMESTAMP, BOOLEAN, ...)` | `BOOLEAN` | NFA-based [pattern matching](https://tomtom215.github.io/duckdb-behavioral/functions/sequence-match.html) over event sequences |
| `sequence_count` | `(VARCHAR, TIMESTAMP, BOOLEAN, ...)` | `BIGINT` | Count non-overlapping pattern matches |
| `sequence_match_events` | `(VARCHAR, TIMESTAMP, BOOLEAN, ...)` | `LIST(TIMESTAMP)` | Return matched condition timestamps |
| `sequence_next_node` | `(VARCHAR, VARCHAR, TIMESTAMP, VARCHAR, BOOLEAN, ...)` | `VARCHAR` | Next event value after pattern match |

All functions support **2 to 32 boolean conditions**, matching ClickHouse's limit.
Detailed documentation, examples, and edge case behavior for each function:
[Function Reference](https://tomtom215.github.io/duckdb-behavioral/functions/sessionize.html)

## Performance

All measurements below are Criterion.rs 0.8.2 with 95% confidence intervals,
validated across multiple runs on commodity hardware.

| Function | Scale | Wall Clock | Throughput |
|---|---|---|---|
| `sessionize` | **1 billion** | **1.20 s** | **830 Melem/s** |
| `retention` (combine) | 100 million | 274 ms | 365 Melem/s |
| `window_funnel` | 100 million | 791 ms | 126 Melem/s |
| `sequence_match` | 100 million | 1.05 s | 95 Melem/s |
| `sequence_count` | 100 million | 1.18 s | 85 Melem/s |
| `sequence_match_events` | 100 million | 1.07 s | 93 Melem/s |
| `sequence_next_node` | 10 million | 546 ms | 18 Melem/s |

**Key design choices:**

- **16-byte `Copy` events** with `u32` bitmask conditions — four events per cache
  line, zero heap allocation per event
- **O(1) combine** for `sessionize` and `retention` via boundary tracking and
  bitmask OR
- **In-place combine** for event-collecting functions — O(N) amortized instead
  of O(N^2) from repeated allocation
- **NFA fast paths** — common pattern shapes dispatch to specialized O(n) linear
  scans instead of full NFA backtracking
- **Presorted detection** — O(n) check skips O(n log n) sort when events arrive
  in timestamp order

**Optimization highlights:**

| Optimization | Speedup | Technique |
|---|---|---|
| Event bitmask | 5–13x | `Vec<bool>` replaced with `u32` bitmask, enabling `Copy` semantics |
| In-place combine | up to 2,436x | O(N) amortized extend instead of O(N^2) merge-allocate |
| NFA lazy matching | 1,961x at 1M events | Swapped exploration order so `.*` tries advancing before consuming |
| `Arc<str>` values | 2.1–5.8x | Reference-counted strings for O(1) clone in `sequence_next_node` |
| NFA fast paths | 39–61% | Pattern classification dispatches common shapes to O(n) linear scans |

Five attempted optimizations were measured, found to be regressions, and reverted.
All negative results are documented in [`PERF.md`](PERF.md).

Full methodology, per-session optimization history with confidence intervals, and
reproducible benchmark instructions: [`PERF.md`](PERF.md).

## Community Extension

This extension is listed in the
[DuckDB Community Extensions](https://github.com/duckdb/community-extensions)
repository ([PR #1306](https://github.com/duckdb/community-extensions/pull/1306),
merged 2026-02-15). Install with:

```sql
INSTALL behavioral FROM community;
LOAD behavioral;
```

No build tools, compilation, or `-unsigned` flag required.

### Update Process

The [`community-submission.yml`](.github/workflows/community-submission.yml)
workflow automates the full pre-submission pipeline in 5 phases:

| Phase | Purpose |
|-------|---------|
| Validate | `description.yml` schema, version consistency, required files |
| Quality Gate | `cargo test` (434 + doc-test), `clippy`, `fmt`, `doc` |
| Build & Test | `make configure && make release && make test_release` |
| Pin Ref | Updates `description.yml` ref to the validated commit SHA |
| Submission Package | Uploads artifact, generates step-by-step PR commands |

### Updating the Published Extension

Push changes to this repository, re-run the submission workflow to pin the new
ref, then open a new PR against `duckdb/community-extensions` updating the ref
field in `extensions/behavioral/description.yml`. When DuckDB releases a new
version, update `libduckdb-sys`, `TARGET_DUCKDB_VERSION`, and the
`extension-ci-tools` submodule.

## Quality

| Metric | Value |
|---|---|
| Unit tests | 434 + 1 doc-test |
| E2E tests | 27 (against real DuckDB CLI) |
| Property-based tests | 26 (proptest) |
| Mutation testing | 88.4% kill rate (130/147, cargo-mutants) |
| Clippy warnings | 0 (pedantic + nursery + cargo lint groups) |
| CI jobs | 13 (check, test, clippy, fmt, doc, MSRV, bench, deny, semver, coverage, cross-platform, extension-build) |
| Benchmark files | 7 (Criterion.rs, up to 1 billion elements) |
| Release platforms | 4 (Linux x86_64/ARM64, macOS x86_64/ARM64) |

CI runs on every push and PR: 6 workflows across `.github/workflows/` including
E2E tests against real DuckDB, CodeQL static analysis, SemVer validation, and
4-platform release builds with provenance attestation.

## ClickHouse Parity Status

**COMPLETE** — All ClickHouse behavioral analytics functions are implemented.

| Function | Status |
|---|---|
| `sessionize` | Complete |
| `retention` | Complete |
| `window_funnel` (6 modes) | Complete |
| `sequence_match` | Complete |
| `sequence_count` | Complete |
| `sequence_match_events` | Complete |
| `sequence_next_node` | Complete |
| 32-condition support | Complete |

## Building

**Prerequisites**: Rust 1.80+ (MSRV), a C compiler (for DuckDB sys bindings)

```bash
# Build the extension (release mode)
cargo build --release

# The loadable extension will be at:
# target/release/libbehavioral.so   (Linux)
# target/release/libbehavioral.dylib (macOS)
```

## Development

```bash
cargo test                  # 434 unit tests + 1 doc-test
cargo clippy --all-targets  # Zero warnings required
cargo fmt                   # Format
cargo bench                 # Criterion.rs benchmarks

# Build extension via community Makefile
git submodule update --init
make configure && make release && make test_release
```

This project follows [Semantic Versioning](https://semver.org/).
See the [versioning policy](https://tomtom215.github.io/duckdb-behavioral/operations/security.html#versioning)
for the full SemVer rules applied to SQL function signatures.

## Documentation

- **[Getting Started](https://tomtom215.github.io/duckdb-behavioral/getting-started.html)** — installation, loading, troubleshooting
- **[Function Reference](https://tomtom215.github.io/duckdb-behavioral/functions/sessionize.html)** — detailed docs for all 7 functions
- **[Use Cases](https://tomtom215.github.io/duckdb-behavioral/use-cases.html)** — 5 complete real-world examples with sample data
- **[Engineering Overview](https://tomtom215.github.io/duckdb-behavioral/engineering.html)** — architecture, testing philosophy, design trade-offs
- **[Performance](https://tomtom215.github.io/duckdb-behavioral/internals/performance.html)** — benchmarks, optimization history, methodology
- **[ClickHouse Compatibility](https://tomtom215.github.io/duckdb-behavioral/internals/clickhouse-compatibility.html)** — syntax mapping, semantic parity
- **[Contributing](https://tomtom215.github.io/duckdb-behavioral/contributing.html)** — development setup, testing, PR process

## Requirements

- Rust 1.80+ (MSRV)
- DuckDB 1.4.4 (pinned dependency)
- Python 3.x (for extension metadata tooling)

## License

MIT
