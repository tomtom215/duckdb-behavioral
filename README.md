# duckdb-behavioral

Behavioral analytics functions for [DuckDB](https://duckdb.org/), inspired by
[ClickHouse](https://clickhouse.com/docs/en/sql-reference/aggregate-functions/parametric-functions).

Provides `sessionize`, `retention`, `window_funnel`, `sequence_match`,
`sequence_count`, `sequence_match_events`, and `sequence_next_node` as a loadable
DuckDB extension written in Rust. **Complete ClickHouse behavioral analytics parity.**

[![CI](https://github.com/tomtom215/duckdb-behavioral/actions/workflows/ci.yml/badge.svg)](https://github.com/tomtom215/duckdb-behavioral/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

## Table of Contents

- [Quick Start](#quick-start)
- [Functions](#functions)
  - [sessionize](#sessionizetimestamp-interval---bigint)
  - [retention](#retentioncond1-cond2----boolean)
  - [window\_funnel](#window_funnelinterval-timestamp-cond1-cond2----integer)
  - [sequence\_match](#sequence_matchpattern-timestamp-cond1-cond2----boolean)
  - [sequence\_count](#sequence_countpattern-timestamp-cond1-cond2----bigint)
  - [sequence\_match\_events](#sequence_match_eventspattern-timestamp-cond1-cond2----listtimestamp)
  - [sequence\_next\_node](#sequence_next_nodedirection-base-timestamp-event_column-cond1-cond2----varchar)
  - [Pattern Syntax](#pattern-syntax)
  - [Window Funnel Modes](#window-funnel-modes)
- [Building](#building)
- [ClickHouse Parity Status](#clickhouse-parity-status)
- [Performance](#performance)
- [Development](#development)
- [Requirements](#requirements)
- [License](#license)

## Quick Start

```bash
# Build the extension
cargo build --release

# Load in DuckDB
duckdb -cmd "LOAD 'target/release/libduckdb_behavioral.so';"
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

### `sessionize(timestamp, interval) -> bigint`

Window function that assigns monotonically increasing session IDs. A new session
starts when the gap between consecutive events exceeds the threshold.

```sql
SELECT user_id, event_time,
  sessionize(event_time, INTERVAL '30 minutes') OVER (
    PARTITION BY user_id ORDER BY event_time
  ) as session_id
FROM events;
```

### `retention(cond1, cond2 [, ...]) -> boolean[]`

Aggregate function for cohort retention analysis. Returns an array where element `i`
is true if both the anchor condition (cond1) and condition `i` were satisfied.

```sql
SELECT cohort_month,
  retention(
    activity_date = cohort_month,
    activity_date = cohort_month + INTERVAL '1 month',
    activity_date = cohort_month + INTERVAL '2 months'
  ) as retained
FROM user_activity
GROUP BY user_id, cohort_month;
```

### `window_funnel(interval[, mode], timestamp, cond1, cond2 [, ...]) -> integer`

Searches for the longest chain of sequential conditions within a time window.
Returns the maximum funnel step reached (0 to N). An optional mode string
parameter controls matching behavior (see [Window Funnel Modes](#window-funnel-modes)).

```sql
-- Default mode
SELECT user_id,
  window_funnel(INTERVAL '1 hour', event_time,
    event_type = 'page_view',
    event_type = 'add_to_cart',
    event_type = 'checkout',
    event_type = 'purchase'
  ) as furthest_step
FROM events
GROUP BY user_id;

-- With mode string
SELECT user_id,
  window_funnel(INTERVAL '1 hour', 'strict_increase, strict_once', event_time,
    event_type = 'page_view',
    event_type = 'add_to_cart',
    event_type = 'purchase'
  ) as furthest_step
FROM events
GROUP BY user_id;
```

### `sequence_match(pattern, timestamp, cond1, cond2 [, ...]) -> boolean`

Checks whether a sequence of events matches a pattern. Uses a mini-regex syntax
over condition references.

```sql
SELECT user_id,
  sequence_match('(?1).*(?2)', event_time,
    event_type = 'view',
    event_type = 'purchase'
  ) as converted
FROM events
GROUP BY user_id;
```

### `sequence_count(pattern, timestamp, cond1, cond2 [, ...]) -> bigint`

Counts the number of non-overlapping occurrences of a pattern in the event stream.

```sql
SELECT user_id,
  sequence_count('(?1)(?2)', event_time,
    event_type = 'view',
    event_type = 'purchase'
  ) as conversion_count
FROM events
GROUP BY user_id;
```

### `sequence_match_events(pattern, timestamp, cond1, cond2 [, ...]) -> list(timestamp)`

Returns the timestamps of each matched `(?N)` step in the pattern as a list.
Returns an empty list if no match is found.

```sql
SELECT user_id,
  sequence_match_events('(?1).*(?2)', event_time,
    event_type = 'view',
    event_type = 'purchase'
  ) as match_timestamps
FROM events
GROUP BY user_id;
-- Returns e.g. [2024-01-01 10:00:00, 2024-01-01 11:30:00]
```

### `sequence_next_node(direction, base, timestamp, event_column, cond1, cond2 [, ...]) -> varchar`

Returns the value of the next (or previous) event after a matched sequence pattern.
Supports forward/backward direction and four base modes for controlling which match
point to use.

```sql
-- What page do users visit after viewing product then adding to cart?
SELECT
  sequence_next_node('forward', 'first_match', event_time, page_name,
    event_type = 'view',
    event_type = 'add_to_cart'
  ) as next_page
FROM events
GROUP BY user_id;

-- What was the last page before a purchase?
SELECT
  sequence_next_node('backward', 'last_match', event_time, page_name,
    event_type = 'checkout',
    event_type = 'purchase'
  ) as previous_page
FROM events
GROUP BY user_id;
```

**Direction**: `'forward'` (next event after match) or `'backward'` (previous event before match).

**Base modes**: `'head'`, `'tail'`, `'first_match'`, `'last_match'` — controls which occurrence
of the pattern match to anchor from.

### Pattern Syntax

| Pattern | Description |
|---------|-------------|
| `(?N)` | Match event where condition N (1-indexed) is true |
| `.` | Match exactly one event (any conditions) |
| `.*` | Match zero or more events (any conditions) |
| `(?t>=N)` | Time constraint: at least N seconds since previous match |
| `(?t<=N)` | Time constraint: at most N seconds since previous match |
| `(?t>N)` | Time constraint: more than N seconds |
| `(?t<N)` | Time constraint: less than N seconds |
| `(?t==N)` | Time constraint: exactly N seconds |
| `(?t!=N)` | Time constraint: not exactly N seconds |

### Window Funnel Modes

The `window_funnel` function supports combinable modes matching ClickHouse semantics:

| Mode | Description |
|------|-------------|
| `strict` | Only events that satisfy condition for step N+1 can follow step N |
| `strict_order` | Events for earlier conditions cannot appear between matched steps |
| `strict_deduplication` | Skip events with the same timestamp as the previous matched step |
| `strict_increase` | Require strictly increasing timestamps between matched steps |
| `strict_once` | Each event can advance the funnel by at most one step |
| `allow_reentry` | Reset the funnel chain when condition 1 fires again after step 1 |

Modes are independently combinable via a comma-separated string parameter:

```sql
-- Combine strict_increase and strict_once
window_funnel(INTERVAL '1 hour', 'strict_increase, strict_once', ts, c1, c2, c3)
```

## ClickHouse Parity Status

**COMPLETE** — All ClickHouse behavioral analytics functions are implemented.

| Function | Status | Session |
|---|---|---|
| `sessionize` | Complete | 1 |
| `retention` | Complete | 1 |
| `window_funnel` (6 modes) | Complete | 1, 5, 6 |
| `sequence_match` | Complete | 1 |
| `sequence_count` | Complete | 1 |
| `sequence_match_events` | Complete | 5 |
| `sequence_next_node` | Complete | 8 |
| 32-condition support | Complete | 6 |

## Building

**Prerequisites**: Rust 1.80+ (MSRV), a C compiler (for DuckDB sys bindings)

```bash
# Build the extension (release mode)
cargo build --release

# The loadable extension will be at:
# target/release/libduckdb_behavioral.so   (Linux)
# target/release/libduckdb_behavioral.dylib (macOS)
```

## Performance

Eleven sessions of measured, Criterion-validated optimizations and feature work:

**Session 1 — Event Bitmask**: Bitmask replaces `Vec<bool>` per event,
eliminating heap allocation and enabling `Copy` semantics (5-13x speedup).

**Session 2 — In-Place Combine**: O(N²) merge-allocate replaced with O(N)
in-place extend, plus pdqsort and pattern clone elimination (up to **2,436x**).

**Session 3 — NFA Lazy Matching**: Fixed `.*` exploration order in NFA executor,
yielding **1,961x** speedup for `sequence_match` at 1M events.

**Session 4 — Billion-Row Benchmarks**: Expanded to 100M/1B elements.
**1 billion sessionize events processed in 1.16 seconds** (862 Melem/s).

**Session 5 — ClickHouse Feature Parity**: Combinable `FunnelMode` bitflags,
three new modes (`strict_increase`, `strict_once`, `allow_reentry`), and
`sequence_match_events` function returning matched condition timestamps.

**Session 6 — Mode FFI + 32-Condition Support**: Exposed funnel modes via SQL
VARCHAR parameter, expanded condition support from 8 to 32 (matching ClickHouse),
and added presorted detection to skip O(n log n) sort for ordered input.

**Session 7 — Benchmark Validation + Negative Results**: Validated all benchmark
baselines with 3 runs. Tested radix sort (4.3x slower — reverted), branchless
sessionize (~5-10% slower — reverted). Documented that current algorithms are
well-optimized. Added 15 tests.

**Session 8 — sequenceNextNode + Complete Parity**: Implemented `sequence_next_node`
with full direction/base support, achieving COMPLETE ClickHouse behavioral analytics
feature parity. Added 42 tests and new benchmarks.

**Session 9 — Rc\<str\> Optimization**: Replaced `String` with `Rc<str>` in
`sequence_next_node` for O(1) clone via reference counting (2.1-5.8x improvement).
Added realistic cardinality benchmarks and community extension infrastructure.

**Session 10 — E2E Validation + Custom C Entry Point**: Discovered and fixed 3
critical bugs that 375 passing unit tests missed: SEGFAULT on load, silent function
registration failures, and incorrect combine propagation. Replaced fragile connection
extraction with a custom C entry point. 11 E2E tests against real DuckDB.

**Session 11 — NFA Fast Paths + Git Mining Demo**: Pattern classification dispatches
common shapes to specialized O(n) scans (39-61% improvement for `sequence_count`).
Added 21 combine propagation tests, 7 fast-path tests, and git mining SQL examples.

**Headline numbers (Criterion-validated, 95% CI, Sessions 7-11):**

| Function | Scale | Wall Clock | Throughput |
|---|---|---|---|
| `sessionize` | **1 billion** | **1.16 s** | **862 Melem/s** |
| `retention` (combine) | **1 billion** | **2.96 s** | **338 Melem/s** |
| `window_funnel` | 100 million | 761 ms | 131 Melem/s |
| `sequence_match` | 100 million | 1.05 s | 95 Melem/s |
| `sequence_count` | 100 million | 1.45 s | 69 Melem/s |
| `sequence_next_node` | 10 million | 1.23 s | 8.1 Melem/s |

All measurements: Criterion.rs, 95% confidence intervals, 3+ runs.
Full methodology, optimization history with CIs, and baseline records:
[`PERF.md`](PERF.md).

## Development

```bash
# Run tests (403 unit tests + 1 doc-test)
cargo test

# Run clippy (zero warnings required)
cargo clippy --all-targets

# Format
cargo fmt

# Run benchmarks
cargo bench

# Build documentation
cargo doc --no-deps --open
```

## Requirements

- Rust 1.80+ (MSRV)
- DuckDB 1.4.4 (pinned dependency)

## License

MIT
