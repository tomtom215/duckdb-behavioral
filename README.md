<p align="center">
  <img src="assets/banner.svg" alt="duckdb-behavioral" width="600">
</p>

<p align="center">
  <strong>Behavioral analytics functions for DuckDB, inspired by ClickHouse.</strong>
</p>

<p align="center">
  <a href="https://github.com/tomtom215/duckdb-behavioral/actions/workflows/ci.yml"><img src="https://github.com/tomtom215/duckdb-behavioral/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/tomtom215/duckdb-behavioral/actions/workflows/e2e.yml"><img src="https://github.com/tomtom215/duckdb-behavioral/actions/workflows/e2e.yml/badge.svg" alt="E2E Tests"></a>
  <a href="https://crates.io/crates/duckdb-behavioral"><img src="https://img.shields.io/crates/v/duckdb-behavioral.svg" alt="Crates.io"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-MIT-blue.svg" alt="License: MIT"></a>
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/MSRV-1.87-blue.svg" alt="MSRV: 1.87"></a>
  <a href="https://tomtom215.github.io/duckdb-behavioral/"><img src="https://img.shields.io/badge/docs-mdBook-blue.svg" alt="Documentation"></a>
</p>

<p align="center">
  <a href="#quick-start">Quick Start</a> &bull;
  <a href="#functions">Functions</a> &bull;
  <a href="#examples">Examples</a> &bull;
  <a href="#performance">Performance</a> &bull;
  <a href="https://tomtom215.github.io/duckdb-behavioral/">Documentation</a>
</p>

---

Provides `sessionize`, `retention`, `window_funnel`, `window_funnel_events`,
`sequence_match`, `sequence_count`, `sequence_match_events`, and
`sequence_next_node` as a loadable
[DuckDB](https://duckdb.org/) extension written in Rust. **Complete
[ClickHouse](https://clickhouse.com/docs/en/sql-reference/aggregate-functions/parametric-functions)
behavioral analytics parity.**

> **Personal Project Disclaimer**: This is a personal project developed on my own
> time. It is not affiliated with, endorsed by, or related to my employer or
> professional role in any way.

> **AI-Assisted Development**: Built with Claude (Anthropic). Correctness is
> validated by automated testing — not assumed from AI output. See [Quality](#quality).

## Table of Contents

- [Quick Start](#quick-start)
- [Functions](#functions)
- [Examples](#examples)
- [Integrations](#integrations)
- [Performance](#performance)
- [Community Extension](#community-extension)
- [Quality](#quality)
- [ClickHouse Parity Status](#clickhouse-parity-status)
- [Building](#building)
- [Development](#development)
- [Documentation](#documentation)
- [Known Limitations](#known-limitations)
- [Requirements](#requirements)
- [License](#license)

## Quick Start

```sql
-- Install from the DuckDB Community Extensions repository
INSTALL behavioral FROM community;
LOAD behavioral;
```

Or build from source (DuckDB loads only `.duckdb_extension` files that carry
its metadata footer; `make` adds it):

```bash
git submodule update --init --recursive
make configure release
duckdb -unsigned -c "LOAD 'build/release/behavioral.duckdb_extension'; SELECT behavioral_version();"
```

**Verify it works** — run these after loading:

```sql
-- Session IDs (should return 1, 1, 2)
SELECT sessionize(ts, INTERVAL '30 minutes') OVER (ORDER BY ts) AS session_id
FROM (VALUES (TIMESTAMP '2024-01-01 10:00'), (TIMESTAMP '2024-01-01 10:10'),
             (TIMESTAMP '2024-01-01 12:00')) t(ts);

-- Retention (should return [true, false])
SELECT retention(true, false);

-- Funnel progress (should return 2: one event satisfying conditions 1 and 2
-- fills both steps, as in ClickHouse)
SELECT window_funnel(INTERVAL '1 hour', TIMESTAMP '2024-01-01', true, true, false);
```

## Functions

| Function | Signature | Returns | Description |
|---|---|---|---|
| `sessionize` | `(TIMESTAMP, INTERVAL)` | `BIGINT` | Window function assigning session IDs based on inactivity gaps |
| `retention` | `(BOOLEAN, BOOLEAN, ...)` | `BOOLEAN[]` | Cohort retention analysis |
| `window_funnel` | `(INTERVAL [, VARCHAR], TIMESTAMP, BOOLEAN, ...)` | `INTEGER` | Conversion funnel step tracking with [6 combinable modes](https://tomtom215.github.io/duckdb-behavioral/functions/window-funnel.html) |
| `window_funnel_events` | `(INTERVAL [, VARCHAR], TIMESTAMP, BOOLEAN, ...)` | `TIMESTAMP[]` | Timestamps of the best funnel chain |
| `sequence_match` | `(VARCHAR, TIMESTAMP, BOOLEAN, ...)` | `BOOLEAN` | [Pattern matching](https://tomtom215.github.io/duckdb-behavioral/functions/sequence-match.html) over event sequences |
| `sequence_count` | `(VARCHAR, TIMESTAMP, BOOLEAN, ...)` | `BIGINT` | Count non-overlapping pattern matches |
| `sequence_match_events` | `(VARCHAR, TIMESTAMP, BOOLEAN, ...)` | `LIST(TIMESTAMP)` | Return matched condition timestamps |
| `sequence_next_node` | `(VARCHAR, VARCHAR, TIMESTAMP, VARCHAR, BOOLEAN, ...)` | `VARCHAR` | Next event value after pattern match |

Condition limits: `window_funnel` / `window_funnel_events` take 1 to 32
conditions, `retention` and the `sequence_*` pattern functions 2 to 32, and
`sequence_next_node` a base condition plus 1 to 32 event conditions.
`behavioral_version()` returns the loaded extension version for diagnostics.
Invalid configuration (unknown modes, malformed patterns, out-of-range
condition numbers, month-based intervals) raises descriptive SQL errors
instead of silently wrong results.
Results are **deterministic under parallel execution**: events sort with total
tie-breaking keys, so thread count and row order never change a result (in two
places ClickHouse's results do depend on row order; see
[ClickHouse Parity](#clickhouse-parity-status)). Gaps touching DuckDB's
`±infinity` timestamps are computed exactly instead of wrapping.
Detailed documentation, examples, and edge case behavior for each function:
[Function Reference](https://tomtom215.github.io/duckdb-behavioral/functions/sessionize.html)

### Choosing the Right Function

| I want to... | Use |
|---|---|
| Break events into sessions by inactivity gap | `sessionize` |
| Check if users returned in later time periods | `retention` |
| Measure how far users get through ordered steps | `window_funnel` |
| See when each funnel step happened | `window_funnel_events` |
| Detect whether a pattern of events occurred | `sequence_match` |
| Count how many times a pattern occurred | `sequence_count` |
| Get timestamps of each matched pattern step | `sequence_match_events` |
| Find what happened immediately after/before a pattern | `sequence_next_node` |

## Examples

### Conversion Funnel

Track how far users progress through a purchase flow within 1 hour:

```sql
SELECT user_id,
  window_funnel(INTERVAL '1 hour', event_time,
    event_type = 'page_view',
    event_type = 'add_to_cart',
    event_type = 'checkout',
    event_type = 'purchase'
  ) as furthest_step
FROM events
GROUP BY user_id;
```

### Session Analysis

Assign session IDs with a 30-minute inactivity gap, then compute metrics:

```sql
WITH sessionized AS (
    SELECT user_id, event_time,
      sessionize(event_time, INTERVAL '30 minutes') OVER (
        PARTITION BY user_id ORDER BY event_time
      ) as session_id
    FROM events
)
SELECT user_id, session_id,
  COUNT(*) as page_views,
  MIN(event_time) as session_start,
  MAX(event_time) as session_end
FROM sessionized
GROUP BY user_id, session_id;
```

### Weekly Retention

Measure week-over-week retention for signup cohorts:

```sql
SELECT cohort_week,
  COUNT(*) as cohort_size,
  SUM(CASE WHEN r[1] THEN 1 ELSE 0 END) as week_0,
  SUM(CASE WHEN r[2] THEN 1 ELSE 0 END) as week_1,
  SUM(CASE WHEN r[3] THEN 1 ELSE 0 END) as week_2
FROM (
  SELECT user_id, cohort_week,
    retention(
      activity_date >= cohort_week AND activity_date < cohort_week + INTERVAL '7 days',
      activity_date >= cohort_week + INTERVAL '7 days' AND activity_date < cohort_week + INTERVAL '14 days',
      activity_date >= cohort_week + INTERVAL '14 days' AND activity_date < cohort_week + INTERVAL '21 days'
    ) as r
  FROM activity GROUP BY user_id, cohort_week
)
GROUP BY cohort_week ORDER BY cohort_week;
```

### Pattern Detection with Time Constraints

Find users who viewed then purchased within 1 hour:

```sql
SELECT user_id,
  sequence_match('(?1).*(?t<=3600)(?2)', event_time,
    event_type = 'page_view',
    event_type = 'purchase'
  ) as converted_within_hour
FROM events GROUP BY user_id;
```

### Funnel Drop-off Report

Aggregate funnel results into a conversion report:

```sql
WITH funnels AS (
  SELECT user_id,
    window_funnel(INTERVAL '1 hour', event_time,
      event_type = 'page_view', event_type = 'add_to_cart',
      event_type = 'checkout', event_type = 'purchase'
    ) as step
  FROM events GROUP BY user_id
)
SELECT step as reached_step,
  COUNT(*) as users,
  ROUND(100.0 * COUNT(*) / SUM(COUNT(*)) OVER (), 1) as pct
FROM funnels GROUP BY step ORDER BY step;
```

### User Flow Analysis

Discover what page users visit after Home → Product. The inner query
computes one next page per user; the outer query counts users per page
(without the inner `GROUP BY user_id`, all users' events would form one
sequence):

```sql
SELECT next_page, COUNT(*) AS user_count
FROM (
  SELECT user_id,
    sequence_next_node('forward', 'first_match', event_time, page,
      page = 'Home', page = 'Home', page = 'Product') AS next_page
  FROM events
  GROUP BY user_id
)
GROUP BY next_page
ORDER BY user_count DESC;
```

### Pattern Frequency

Count how many times users repeat a view → cart cycle:

```sql
SELECT user_id,
  sequence_count('(?1).*(?2)', event_time,
    event_type = 'page_view',
    event_type = 'add_to_cart'
  ) as view_cart_cycles
FROM events GROUP BY user_id ORDER BY view_cart_cycles DESC;
```

### Matched Event Timestamps

Get the exact timestamps when each funnel step was satisfied:

```sql
SELECT user_id,
  sequence_match_events('(?1).*(?2).*(?3)', event_time,
    event_type = 'page_view',
    event_type = 'add_to_cart',
    event_type = 'purchase'
  ) as step_timestamps
FROM events GROUP BY user_id;
```

For 5 complete real-world examples with sample data, see
[Use Cases](https://tomtom215.github.io/duckdb-behavioral/use-cases.html).
For a comprehensive recipe collection, see
[SQL Cookbook](https://tomtom215.github.io/duckdb-behavioral/cookbook.html).

## Integrations

### Python

```python
import duckdb

conn = duckdb.connect()
conn.execute("INSTALL behavioral FROM community")
conn.execute("LOAD behavioral")

df = conn.execute("""
    SELECT user_id,
      window_funnel(INTERVAL '1 hour', event_time,
        event_type = 'view', event_type = 'cart', event_type = 'purchase'
      ) as steps
    FROM events GROUP BY user_id
""").fetchdf()
```

### Node.js

```javascript
const duckdb = require('duckdb');
const db = new duckdb.Database(':memory:');

db.run("INSTALL behavioral FROM community");
db.run("LOAD behavioral");
```

### dbt

```yaml
# profiles.yml
my_project:
  outputs:
    dev:
      type: duckdb
      extensions:
        - name: behavioral
          repo: community
```

### Parquet / CSV / JSON

```sql
-- Query any file format directly
SELECT user_id,
  window_funnel(INTERVAL '1 hour', event_time,
    event_type = 'view', event_type = 'purchase')
FROM read_parquet('events/*.parquet')
GROUP BY user_id;
```

## Performance

All measurements below are Criterion.rs 0.8.2 microbenchmarks of the Rust
state machines (not SQL queries) with 95% confidence intervals, recorded in
[`PERF.md`](PERF.md) Session 15, before v0.8.0. They have not been re-measured
since: v0.8.0 changed hot paths, and the `window_funnel` engine was replaced
with ClickHouse's algorithm in this release (in an interleaved before/after
run its `finalize` benchmark ranged from no measurable change to ~16% slower,
see `CHANGELOG.md`).

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
- **Sequence fast paths** — common pattern shapes dispatch to specialized O(n)
  linear scans; every other pattern uses a feasibility pass plus a greedy walk,
  O(s · n log n), replacing a backtracking search that was quadratic per group
- **Presorted detection** — O(n) check skips O(n log n) sort when events arrive
  in timestamp order

**Optimization highlights:**

| Optimization | Speedup | Technique |
|---|---|---|
| Event bitmask | 5–13x | `Vec<bool>` replaced with `u32` bitmask, enabling `Copy` semantics |
| In-place combine | up to 2,436x | O(N) amortized extend instead of O(N^2) merge-allocate |
| NFA lazy matching | 1,961x at 1M events | Swapped exploration order so `.*` tries advancing before consuming |
| `Arc<str>` values | 1.8–5.8x | Reference-counted strings for O(1) clone in `sequence_next_node` |
| NFA fast paths | 39–60% | Pattern classification dispatches common shapes to O(n) linear scans |

Five attempted optimizations were measured, found to be regressions, and reverted.
All negative results are documented in [`PERF.md`](PERF.md).

Full methodology, per-session optimization history with confidence intervals, and
reproducible benchmark instructions: [`PERF.md`](PERF.md).

## Community Extension

This extension is listed in the
[DuckDB Community Extensions](https://github.com/duckdb/community-extensions)
repository ([PR #1306](https://github.com/duckdb/community-extensions/pull/1306)).
The published v0.9.1 was built for exactly one DuckDB release (v1.5.5); from
this release the extension uses the stable C API, so one build serves every
DuckDB release the community repository builds for. Install with:

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
| Quality Gate | `cargo test`, `clippy`, `fmt`, `doc` |
| Build & Test | `make configure && make release && make test_release` |
| Pin Ref | Updates `description.yml` ref to the validated commit SHA |
| Submission Package | Uploads artifact, generates step-by-step PR commands |

### Updating the Published Extension

Push changes to this repository, re-run the submission workflow to pin the new
ref, then open a new PR against `duckdb/community-extensions` updating the ref
field in `extensions/behavioral/description.yml`. A new DuckDB release needs
no extension change to load the binary. To build against its headers, bump
`libduckdb-sys` / `duckdb`, `DUCKDB_TEST_VERSION`, the E2E `DUCKDB_VERSION`
and `compat` matrix; leave `TARGET_DUCKDB_VERSION` at the C API version
(`v1.2.0`).

## Quality

| Metric | Value |
|---|---|
| Unit tests | 518 + 1 doc-test |
| Integration tests | 21 (in-process: real extension loaded via `InMemoryDb`, all functions exercised through SQL incl. error paths, infinity timestamps, and parallel-determinism probes) |
| E2E tests | 12 workflow steps (2 platforms) + 8 SQL logic test files (against real DuckDB CLI), plus a compat job loading one binary into DuckDB 1.3.2, 1.4.4, 1.5.0 and 1.5.6 |
| Differential tests | `window_funnel`, `retention`, `sequence_*` and `sequence_next_node` fuzzed against ClickHouse 26.9.8.3 (see below) |
| Property-based tests | 29 (proptest) |
| Mutation testing | 88.4% kill rate (130/147, cargo-mutants), measured on v0.4.x and not re-measured since |
| Clippy warnings | 0 (pedantic + nursery + cargo lint groups) |
| CI jobs | 14 (check, wasm-check, test, clippy, fmt, doc, MSRV, bench-compile, deny, semver, coverage, cross-platform, extension-build, ci-gate) |
| Benchmark files | 7 (Criterion.rs, up to 1 billion elements) |
| Release platforms | 4 (Linux x86_64/ARM64, macOS x86_64/ARM64) |

CI runs on every push and PR: 6 workflows across `.github/workflows/` including
E2E tests against real DuckDB, CodeQL static analysis, SemVer validation, and
4-platform release builds with provenance attestation.

## ClickHouse Parity Status

All six ClickHouse behavioral parametric functions are implemented, and each
was fuzzed against ClickHouse 26.9.8.3 (`clickhouse local`) with random event
groups: ties, events matching several conditions, every mode combination.

| Function | Result of differential testing |
|---|---|
| `retention` | Identical in all 102,500 groups |
| `window_funnel` | Ported to ClickHouse's algorithm; identical for 17 of 24 mode combinations. The rest differ only where ClickHouse is defective (below) |
| `sequence_match` / `sequence_count` | Identical when time constraints follow `(?N)` or `.`; differ by design after `.*` (below) |
| `sequence_match_events` | As above; also reports the events that actually matched where ClickHouse reports an abandoned attempt |
| `sequence_next_node` | Identical in 240,000 NULL-free cases with distinct (timestamp, value) per event; with ties, identical whenever ClickHouse receives rows in the extension's tie order (below) |
| `sessionize`, `window_funnel_events` | Extension-only (checked against SQL references) |

Deliberate differences, each because ClickHouse's behaviour is defective or
depends on row order:

- `window_funnel` with `strict_increase` (without `strict_once`): ClickHouse
  keeps one chain per level and loses a valid one (`c1@0, c1@1, c2@1` gives 1;
  the extension gives 2).
- `window_funnel` with `strict_deduplication` + `strict_once`, and
  `sequence_next_node` with rows tying on timestamp and value: ClickHouse's
  answer changes with row order; the extension orders ties deterministically.
- `sequence_*` time constraints after `.*` are measured from the last matched
  event; ClickHouse measures from the event after it, so
  `(?1).*(?t>0)(?2)` can never match there. A constraint before any `(?N)` or
  `.` is an error.

Other differences: NULL conditions count as false (ClickHouse skips the
row), timestamps are microseconds with `(?t)` thresholds in seconds, windows
are `INTERVAL`s, and `retention` over zero rows returns `[]`. The complete
list is in the
[ClickHouse Compatibility](https://tomtom215.github.io/duckdb-behavioral/internals/clickhouse-compatibility.html)
page.

## Building

**Prerequisites**: Rust 1.87+ (MSRV), a C compiler (for DuckDB sys bindings)

```bash
git submodule update --init --recursive   # extension-ci-tools
make configure release                    # cargo build --release + metadata footer
# Loadable extension: build/release/behavioral.duckdb_extension
```

`cargo build --release` alone produces `target/release/libbehavioral.so`
(`.dylib` on macOS), which DuckDB refuses to load until the metadata footer is
appended; see [Getting Started](https://tomtom215.github.io/duckdb-behavioral/getting-started.html).

## Development

```bash
DUCKDB_DOWNLOAD_LIB=1 cargo test   # 518 unit + 21 integration + 1 doc-test (prebuilt libduckdb, no C++ build)
cargo clippy --all-targets  # Zero warnings required
cargo fmt -- --check        # Format check
cargo bench                 # Criterion.rs benchmarks
cargo doc --no-deps         # Build API documentation

# Run all quality checks at once
./scripts/check.sh

# Build extension via community Makefile
git submodule update --init
make configure && make release && make test_release
```

This project follows [Semantic Versioning](https://semver.org/).
See the [versioning policy](https://tomtom215.github.io/duckdb-behavioral/operations/security.html#versioning)
for the full SemVer rules applied to SQL function signatures.

## Documentation

- **[Getting Started](https://tomtom215.github.io/duckdb-behavioral/getting-started.html)** — installation, loading, troubleshooting
- **[Function Reference](https://tomtom215.github.io/duckdb-behavioral/functions/sessionize.html)** — detailed docs for all 8 functions
- **[Use Cases](https://tomtom215.github.io/duckdb-behavioral/use-cases.html)** — 5 complete real-world examples with sample data
- **[SQL Cookbook](https://tomtom215.github.io/duckdb-behavioral/cookbook.html)** — practical recipes for common analytics patterns
- **[Quick Reference](https://tomtom215.github.io/duckdb-behavioral/quick-reference.html)** — one-page cheat sheet for all functions and patterns
- **[Engineering Overview](https://tomtom215.github.io/duckdb-behavioral/engineering.html)** — architecture, testing philosophy, design trade-offs
- **[Performance](https://tomtom215.github.io/duckdb-behavioral/internals/performance.html)** — benchmarks, optimization history, methodology
- **[ClickHouse Compatibility](https://tomtom215.github.io/duckdb-behavioral/internals/clickhouse-compatibility.html)** — syntax mapping, semantic parity
- **[Contributing](https://tomtom215.github.io/duckdb-behavioral/contributing.html)** — development setup, testing, PR process

## Known Limitations

- **Crash in DuckDB's C API aggregate path.** Three query shapes make DuckDB
  read out of bounds and usually segfault, for every C API aggregate, not
  just this extension
  ([duckdb/duckdb#26109](https://github.com/duckdb/duckdb/issues/26109);
  present in DuckDB 1.5.6): `ORDER BY` inside the call
  (`retention(c1, c2 ORDER BY ts)`), `OVER ()`, and a frame written as
  `BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING`. None is needed: the
  functions sort by timestamp themselves, `OVER (PARTITION BY 1)` replaces
  `OVER ()`, and `OVER (PARTITION BY user_id)` (no `ORDER BY`) gives the
  whole-partition value. Details and the verified-safe shapes are in the
  [FAQ](https://tomtom215.github.io/duckdb-behavioral/faq.html#which-query-shapes-crash-duckdb).
- **Running window frames are quadratic.** In a running frame
  (`OVER (ORDER BY ts)`) each row is a separate aggregate over all earlier
  rows, and DuckDB holds about 2,048 such frames at once: one 40,000-row partition took 11.1 s and 1.25 GB for
  `window_funnel` (DuckDB 1.5.6, one thread). Keep running frames to small
  partitions (`PARTITION BY user_id`), bound them (`ROWS 1000 PRECEDING`), or
  use `GROUP BY`. `sessionize` is the exception: its state is constant-size.
- **No cancellation within one group.** DuckDB's C API gives an aggregate no
  way to observe an interrupt, so Ctrl-C takes effect only once the group
  being finalized is done. For the `sequence_*` functions that time grows
  with the number of events times the number of pattern steps: 0.8–1.3 s for
  10 million events and a 3- to 5-step pattern, 3.3 s for 1 million events
  and a 201-step pattern (DuckDB 1.5.6). Patterns are capped at 1024 steps.
- **Memory outside `memory_limit`.** Collected events (16 bytes each; more for
  `sequence_next_node` values) and the sequence matcher's working memory live
  on the Rust heap, which DuckDB does not account for or spill. An allocation
  that fails raises an `out of memory` error instead of crashing.

## Requirements

- Rust 1.87+ (MSRV)
- DuckDB 1.3.2 or later at run time (built against 1.5.6 via `libduckdb-sys =1.10506.0`; the extension uses only the stable C API, and CI loads the same binary into 1.3.2, 1.4.4, 1.5.0 and 1.5.6)
- Python 3.x (for extension metadata tooling)

## License

MIT
