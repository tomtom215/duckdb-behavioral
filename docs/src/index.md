# duckdb-behavioral

Behavioral analytics functions for [DuckDB](https://duckdb.org/), inspired by
[ClickHouse](https://clickhouse.com/docs/en/sql-reference/aggregate-functions/parametric-functions).

`duckdb-behavioral` is a loadable DuckDB extension written in Rust that provides
seven functions for session analysis, cohort retention, conversion funnels, and
event sequence pattern matching.

## Functions

| Function | Type | Returns | Description |
|---|---|---|---|
| [`sessionize`](./functions/sessionize.md) | Window | `BIGINT` | Assigns session IDs based on inactivity gaps |
| [`retention`](./functions/retention.md) | Aggregate | `BOOLEAN[]` | Cohort retention analysis |
| [`window_funnel`](./functions/window-funnel.md) | Aggregate | `INTEGER` | Conversion funnel step tracking |
| [`sequence_match`](./functions/sequence-match.md) | Aggregate | `BOOLEAN` | Pattern matching over event sequences |
| [`sequence_count`](./functions/sequence-count.md) | Aggregate | `BIGINT` | Count non-overlapping pattern matches |
| [`sequence_match_events`](./functions/sequence-match-events.md) | Aggregate | `LIST(TIMESTAMP)` | Return matched condition timestamps |
| [`sequence_next_node`](./functions/sequence-next-node.md) | Aggregate | `VARCHAR` | Next event value after pattern match |

## Quick Example

```sql
-- Load the extension
LOAD 'libduckdb_behavioral.so';

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

## Performance

All functions are engineered for large-scale analytical workloads. Performance
claims are backed by [Criterion.rs](https://bheisler.github.io/criterion.rs/book/)
benchmarks with 95% confidence intervals, validated across multiple runs.

| Function | Scale | Throughput |
|---|---|---|
| `sessionize` | 1 billion rows | 862 Melem/s |
| `retention` | 1 billion rows | 338 Melem/s |
| `window_funnel` | 100 million rows | 131 Melem/s |
| `sequence_match` | 100 million rows | 95 Melem/s |
| `sequence_count` | 100 million rows | 69 Melem/s |

Full methodology and optimization history are documented in the
[Performance](./internals/performance.md) section.

## Requirements

- **Rust 1.80+** (MSRV) for building from source
- **DuckDB 1.4.4** (pinned dependency)
- A C compiler for DuckDB system bindings

## Source Code

The source code is available at
[github.com/tomtom215/duckdb-behavioral](https://github.com/tomtom215/duckdb-behavioral).

## License

MIT
