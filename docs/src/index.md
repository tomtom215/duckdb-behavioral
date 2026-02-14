# duckdb-behavioral

**Behavioral analytics for DuckDB -- session analysis, conversion funnels,
retention cohorts, and event sequence pattern matching, all inside your SQL
queries.**

`duckdb-behavioral` is a loadable DuckDB extension written in Rust that brings
[ClickHouse-style behavioral analytics functions](https://clickhouse.com/docs/en/sql-reference/aggregate-functions/parametric-functions)
to DuckDB. It ships seven battle-tested functions that cover the core patterns
of user behavior analysis, with complete ClickHouse feature parity and
benchmark-validated performance at billion-row scale.

No external services, no data pipelines, no additional infrastructure. Load the
extension, write SQL, get answers.

---

## What Can You Do With This?

### Session Analysis

Break a continuous stream of events into logical sessions based on inactivity
gaps. Identify how many sessions a user has per day, how long each session lasts,
and where sessions begin and end.

```sql
SELECT user_id, event_time,
  sessionize(event_time, INTERVAL '30 minutes') OVER (
    PARTITION BY user_id ORDER BY event_time
  ) as session_id
FROM events;
```

### Conversion Funnels

Track how far users progress through a multi-step conversion funnel (page view,
add to cart, checkout, purchase) within a time window. Identify exactly where
users drop off.

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

### Retention Cohorts

Measure whether users who appeared in a cohort (e.g., signed up in January)
returned in subsequent periods. Build the classic retention triangle directly in
SQL.

```sql
SELECT cohort_month,
  retention(
    activity_date = cohort_month,
    activity_date = cohort_month + INTERVAL '1 month',
    activity_date = cohort_month + INTERVAL '2 months',
    activity_date = cohort_month + INTERVAL '3 months'
  ) as retained
FROM user_activity
GROUP BY user_id, cohort_month;
```

### Event Sequence Pattern Matching

Detect complex behavioral patterns using a mini-regex over event conditions.
Find users who viewed a product, then purchased within one hour -- with any
number of intervening events.

```sql
SELECT user_id,
  sequence_match('(?1).*(?t<=3600)(?2)', event_time,
    event_type = 'view',
    event_type = 'purchase'
  ) as converted_within_hour
FROM events
GROUP BY user_id;
```

### User Journey / Flow Analysis

Discover what users do *after* a specific behavioral sequence. What page do
users visit after navigating from Home to Product?

```sql
SELECT
  sequence_next_node('forward', 'first_match', event_time, page,
    page = 'Home',
    page = 'Home',
    page = 'Product'
  ) as next_page,
  COUNT(*) as user_count
FROM events
GROUP BY ALL
ORDER BY user_count DESC;
```

---

## Quick Installation

### From Source

```bash
git clone https://github.com/tomtom215/duckdb-behavioral.git
cd duckdb-behavioral
cargo build --release
```

Then load the extension in DuckDB:

```sql
LOAD 'path/to/target/release/libbehavioral.so';  -- Linux
LOAD 'path/to/target/release/libbehavioral.dylib'; -- macOS
```

> **Note:** DuckDB requires the `-unsigned` flag for locally-built extensions:
> `duckdb -unsigned`

For detailed installation instructions, troubleshooting, and a complete
worked example, see the [Getting Started](./getting-started.md) guide.

---

## Functions

Seven functions covering the full spectrum of behavioral analytics:

| Function | Type | Returns | Description |
|---|---|---|---|
| [`sessionize`](./functions/sessionize.md) | Window | `BIGINT` | Assigns session IDs based on inactivity gaps |
| [`retention`](./functions/retention.md) | Aggregate | `BOOLEAN[]` | Cohort retention analysis |
| [`window_funnel`](./functions/window-funnel.md) | Aggregate | `INTEGER` | Conversion funnel step tracking |
| [`sequence_match`](./functions/sequence-match.md) | Aggregate | `BOOLEAN` | Pattern matching over event sequences |
| [`sequence_count`](./functions/sequence-count.md) | Aggregate | `BIGINT` | Count non-overlapping pattern matches |
| [`sequence_match_events`](./functions/sequence-match-events.md) | Aggregate | `LIST(TIMESTAMP)` | Return matched condition timestamps |
| [`sequence_next_node`](./functions/sequence-next-node.md) | Aggregate | `VARCHAR` | Next event value after pattern match |

All functions support **2 to 32 boolean conditions**, matching ClickHouse's
limit. See the [ClickHouse Compatibility](./internals/clickhouse-compatibility.md)
page for the full parity matrix.

---

## Performance

All functions are engineered for large-scale analytical workloads. Every
performance claim below is backed by
[Criterion.rs](https://bheisler.github.io/criterion.rs/book/) benchmarks with
95% confidence intervals, validated across multiple runs.

| Function | Scale | Wall Clock | Throughput |
|---|---|---|---|
| **`sessionize`** | **1 billion rows** | **1.18 s** | **848 Melem/s** |
| **`retention`** | **1 billion rows** | **2.94 s** | **340 Melem/s** |
| `window_funnel` | 100 million rows | 715 ms | 140 Melem/s |
| `sequence_match` | 100 million rows | 902 ms | 111 Melem/s |
| `sequence_count` | 100 million rows | 1.05 s | 95 Melem/s |
| `sequence_match_events` | 100 million rows | 921 ms | 109 Melem/s |
| `sequence_next_node` | 10 million rows | 438 ms | 23 Melem/s |

Key design choices that enable this performance:

- **16-byte `Copy` events** with `u32` bitmask conditions -- four events per
  cache line, zero heap allocation per event
- **O(1) combine** for `sessionize` and `retention` via boundary tracking and
  bitmask OR
- **In-place combine** for event-collecting functions -- O(N) amortized instead
  of O(N^2) from repeated allocation
- **NFA fast paths** -- common pattern shapes dispatch to specialized O(n) linear
  scans instead of full NFA backtracking
- **Presorted detection** -- O(n) check skips O(n log n) sort when events arrive
  in timestamp order (common for `ORDER BY` queries)

Full methodology, per-element cost analysis, and optimization history are
documented in the [Performance](./internals/performance.md) section.

---

## Engineering Highlights

This project demonstrates depth across systems programming, database internals,
algorithm design, performance engineering, and software quality practices.
For a comprehensive technical overview, see the
[Engineering Overview](./engineering.md).

| Area | Highlights |
|---|---|
| **Language & Safety** | Pure Rust core with `unsafe` confined to 6 FFI files. Zero clippy warnings under pedantic, nursery, and cargo lint groups. |
| **Testing Rigor** | 403 unit tests, 11 E2E tests against real DuckDB, 26 property-based tests (proptest), 88.4% mutation testing kill rate (cargo-mutants). |
| **Performance** | Twelve sessions of measured optimization with Criterion.rs. Billion-row benchmarks with 95% confidence intervals. Three negative results documented honestly. |
| **Algorithm Design** | Custom NFA pattern engine with recursive descent parser, fast-path classification, and lazy backtracking. Bitmask-based retention with O(1) combine. |
| **Database Internals** | Raw DuckDB C API integration via custom entry point. 31 function set overloads per variadic function. Correct combine semantics for segment tree windowing. |
| **CI/CD** | 13 CI jobs, 4-platform release builds, SemVer validation, artifact attestation, MSRV verification. |
| **Feature Completeness** | Complete ClickHouse behavioral analytics parity: 7 functions, 6 combinable funnel modes, 32-condition support, time-constrained pattern syntax. |

---

## Documentation

| Section | Contents |
|---|---|
| [Engineering Overview](./engineering.md) | Technical depth, architecture, quality standards, domain significance |
| [Getting Started](./getting-started.md) | Installation, loading, troubleshooting, your first analysis |
| [Function Reference](./functions/sessionize.md) | Detailed docs for all 7 functions with examples |
| [Use Cases](./use-cases.md) | Five complete real-world examples with sample data and queries |
| [FAQ](./faq.md) | Common questions about loading, patterns, modes, NULLs |
| [Architecture](./internals/architecture.md) | Module structure, design decisions, FFI bridge |
| [Performance](./internals/performance.md) | Benchmarks, algorithmic complexity, optimization history |
| [ClickHouse Compatibility](./internals/clickhouse-compatibility.md) | Syntax mapping, semantic parity matrix |
| [Operations](./operations/ci-cd.md) | CI/CD, security and supply chain, benchmarking methodology |
| [Contributing](./contributing.md) | Development setup, testing expectations, PR process |

---

## Requirements

- **DuckDB 1.4.4** (C API version v1.2.0)
- **Rust 1.80+** (MSRV) for building from source
- A C compiler for DuckDB system bindings

## Source Code

The source code is available at
[github.com/tomtom215/duckdb-behavioral](https://github.com/tomtom215/duckdb-behavioral).

## License

MIT
