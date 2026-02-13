# FAQ

Frequently asked questions about `duckdb-behavioral`.

## Loading the Extension

### How do I load the extension?

Build the extension in release mode, then load it from the DuckDB CLI or any
DuckDB client:

```bash
cargo build --release
```

```sql
-- Linux
LOAD 'target/release/libduckdb_behavioral.so';

-- macOS
LOAD 'target/release/libduckdb_behavioral.dylib';
```

You must pass the `-unsigned` flag when using the DuckDB CLI since this extension
is not signed by the DuckDB Foundation:

```bash
duckdb -unsigned -c "LOAD 'target/release/libduckdb_behavioral.so'; SELECT ..."
```

### The extension fails to load. What should I check?

1. **DuckDB version mismatch**: The extension is built against DuckDB C API
   version `v1.2.0` (used by DuckDB v1.4.4). Loading it into a different DuckDB
   version will produce: `file was built for DuckDB C API version 'v1.2.0' but
   we can only load extensions built for DuckDB C API '...'`.

2. **Missing `-unsigned` flag**: DuckDB rejects unsigned extensions by default.
   Use `duckdb -unsigned` or set `allow_unsigned_extensions=true`.

3. **Wrong file path**: Ensure the path points to the actual `.so` or `.dylib`
   file produced by `cargo build --release`.

4. **Platform mismatch**: An extension built on Linux cannot be loaded on macOS,
   and vice versa. Build on the same platform where DuckDB is running.

## Functions

### What is the difference between sequence_match, sequence_count, and sequence_match_events?

All three use the same pattern syntax and NFA engine, but return different results:

| Function | Returns | Use Case |
|---|---|---|
| `sequence_match` | `BOOLEAN` | Did the pattern occur at all? |
| `sequence_count` | `BIGINT` | How many times did the pattern occur (non-overlapping)? |
| `sequence_match_events` | `LIST(TIMESTAMP)` | At what timestamps did each step match? |

Use `sequence_match` for filtering, `sequence_count` for frequency analysis, and
`sequence_match_events` for diagnostic investigation of when each step was
satisfied.

### How many boolean conditions can I use?

All functions support **2 to 32** boolean condition parameters. This matches
ClickHouse's limit. The conditions are stored internally as a `u32` bitmask,
so the 32-condition limit is a hard constraint of the data type.

### How are NULL values handled?

- **NULL timestamps**: Rows with NULL timestamps are ignored during update.
- **NULL conditions**: NULL boolean conditions are treated as `false`.
- **NULL pattern**: An empty or NULL pattern string results in no match.
- **sequence_next_node**: NULL event column values are stored and can be returned
  as the result. The function returns NULL when no match is found or no adjacent
  event exists.

### When should I use window_funnel vs. sequence_match?

Both functions analyze multi-step user journeys, but they serve different purposes:

- **`window_funnel`** answers: "How far did the user get through a specific ordered
  funnel within a time window?" It returns a step count (0 to N) and supports
  behavioral modes (`strict`, `strict_once`, etc.). Best for conversion funnel
  analysis where you want drop-off rates between steps.

- **`sequence_match`** answers: "Did a specific pattern of events occur?" It uses
  a flexible regex-like syntax supporting wildcards (`.`, `.*`) and time
  constraints (`(?t<=3600)`). Best for detecting complex behavioral patterns
  that go beyond simple ordered steps.

Use `window_funnel` when you need step-by-step drop-off metrics. Use
`sequence_match` when you need flexible pattern matching with arbitrary gaps
and time constraints.

## Pattern Syntax

### Quick reference

| Pattern | Description |
|---|---|
| `(?N)` | Match an event where condition N (1-indexed) is true |
| `.` | Match exactly one event (any conditions) |
| `.*` | Match zero or more events (any conditions) |
| `(?t>=N)` | At least N seconds since previous match |
| `(?t<=N)` | At most N seconds since previous match |
| `(?t>N)` | More than N seconds since previous match |
| `(?t<N)` | Less than N seconds since previous match |
| `(?t==N)` | Exactly N seconds since previous match |
| `(?t!=N)` | Not exactly N seconds since previous match |

### Common patterns

```sql
-- User viewed then purchased (any events in between)
'(?1).*(?2)'

-- User viewed then purchased with no intervening events
'(?1)(?2)'

-- User viewed, then purchased within 1 hour (3600 seconds)
'(?1).*(?t<=3600)(?2)'

-- Three-step funnel with time constraints
'(?1).*(?t<=3600)(?2).*(?t<=7200)(?3)'
```

## Window Funnel Modes

### What modes are available and how do they combine?

Six modes are available, each adding an independent constraint:

| Mode | Effect |
|---|---|
| `strict` | Condition for step N-1 must not refire before step N matches |
| `strict_order` | No earlier conditions may fire between matched steps |
| `strict_deduplication` | Skip events with the same timestamp as the previous step |
| `strict_increase` | Require strictly increasing timestamps between steps |
| `strict_once` | Each event can advance the funnel by at most one step |
| `allow_reentry` | Reset the funnel when condition 1 fires again |

Modes are independently combinable via a comma-separated string:

```sql
window_funnel(INTERVAL '1 hour', 'strict_increase, strict_once',
  ts, cond1, cond2, cond3)
```

## Differences from ClickHouse

### How does the syntax differ from ClickHouse?

ClickHouse uses a two-level call syntax where configuration parameters are
separated from data parameters by an extra set of parentheses. `duckdb-behavioral`
uses a flat parameter list, consistent with standard SQL function syntax.

```sql
-- ClickHouse syntax
windowFunnel(3600)(timestamp, cond1, cond2, cond3)
sequenceMatch('(?1).*(?2)')(timestamp, cond1, cond2)
sequenceNextNode('forward', 'head')(timestamp, value, base_cond, ev1, ev2)

-- duckdb-behavioral syntax
window_funnel(INTERVAL '1 hour', timestamp, cond1, cond2, cond3)
sequence_match('(?1).*(?2)', timestamp, cond1, cond2)
sequence_next_node('forward', 'head', timestamp, value, base_cond, ev1, ev2)
```

### What are the naming convention differences?

| ClickHouse | duckdb-behavioral |
|---|---|
| `windowFunnel` | `window_funnel` |
| `sequenceMatch` | `sequence_match` |
| `sequenceCount` | `sequence_count` |
| `sequenceNextNode` | `sequence_next_node` |
| `retention` | `retention` |

ClickHouse uses `camelCase`. `duckdb-behavioral` uses `snake_case`, following
DuckDB's naming conventions.

### Are there differences in parameter types?

| Parameter | ClickHouse | duckdb-behavioral |
|---|---|---|
| Window size | Integer (seconds) | DuckDB `INTERVAL` type |
| Mode string | Second parameter in the parameter list | Optional `VARCHAR` before timestamp |
| Time constraints in patterns | Seconds (integer) | Seconds (integer) -- same |
| Condition limit | 32 | 32 -- same |

The `INTERVAL` type is more expressive than raw seconds. You can write
`INTERVAL '1 hour'`, `INTERVAL '30 minutes'`, or `INTERVAL '2 days'` instead of
computing the equivalent number of seconds.

### Does this extension have a sessionize equivalent in ClickHouse?

No. The `sessionize` function has no direct equivalent in ClickHouse's behavioral
analytics function set. ClickHouse provides session analysis through different
mechanisms (e.g., `sessionTimeoutSeconds` in `windowFunnel`). The `sessionize`
window function is a DuckDB-specific addition for assigning session IDs based on
inactivity gaps.

### Do I need to set any experimental flags?

No. In ClickHouse, `sequenceNextNode` requires
`SET allow_experimental_funnel_functions = 1`. In `duckdb-behavioral`, all seven
functions are available immediately after loading the extension, with no
experimental flags required.

## Data Preparation

### What columns does my data need?

The required columns depend on which function you use:

| Function | Required Columns |
|---|---|
| `sessionize` | A `TIMESTAMP` column for event time |
| `retention` | Boolean expressions for each cohort period |
| `window_funnel` | A `TIMESTAMP` column and boolean expressions for each funnel step |
| `sequence_match` / `sequence_count` / `sequence_match_events` | A `TIMESTAMP` column and boolean expressions for conditions |
| `sequence_next_node` | A `TIMESTAMP` column, a `VARCHAR` value column, and boolean expressions for conditions |

The boolean "condition" parameters are typically inline expressions rather than
stored columns. For example:

```sql
-- Conditions are computed inline from existing columns
window_funnel(INTERVAL '1 hour', event_time,
  event_type = 'page_view',       -- cond1: computed from event_type
  event_type = 'add_to_cart',     -- cond2: computed from event_type
  event_type = 'purchase'         -- cond3: computed from event_type
)
```

### What is the ideal table structure for behavioral analytics?

A single event-level table with one row per user action works best. The minimum
structure is:

```sql
CREATE TABLE events (
    user_id    VARCHAR NOT NULL,   -- or INTEGER, UUID, etc.
    event_time TIMESTAMP NOT NULL,
    event_type VARCHAR NOT NULL    -- the action taken
);
```

For richer analysis, add context columns:

```sql
CREATE TABLE events (
    user_id      VARCHAR NOT NULL,
    event_time   TIMESTAMP NOT NULL,
    event_type   VARCHAR NOT NULL,
    page_url     VARCHAR,            -- for web analytics
    product_id   VARCHAR,            -- for e-commerce
    revenue      DECIMAL(10,2),      -- for purchase events
    device_type  VARCHAR,            -- for segmentation
    campaign     VARCHAR             -- for attribution
);
```

All behavioral functions operate on this flat event stream. There is no need for
pre-aggregated or pivoted data.

### Do events need to be sorted before calling these functions?

No. All event-collecting functions (`window_funnel`, `sequence_match`,
`sequence_count`, `sequence_match_events`, `sequence_next_node`) sort events by
timestamp internally during the finalize phase. You do not need an `ORDER BY`
clause for these aggregate functions.

However, an `ORDER BY` on the timestamp column can still improve performance.
The extension includes a presorted detection optimization: if events arrive
already sorted (which happens when DuckDB's query planner pushes down an
`ORDER BY`), the O(n log n) sort is skipped entirely, reducing finalize to O(n).

The `sessionize` window function **does** require `ORDER BY` in the `OVER` clause
because it is a window function, not an aggregate:

```sql
sessionize(event_time, INTERVAL '30 minutes') OVER (
    PARTITION BY user_id ORDER BY event_time
)
```

### Can I use these functions with Parquet, CSV, or other file formats?

Yes. DuckDB can read from Parquet, CSV, JSON, and many other formats directly.
The behavioral functions operate on DuckDB's internal columnar representation,
so the source format is irrelevant:

```sql
-- Query Parquet files directly
SELECT user_id,
  window_funnel(INTERVAL '1 hour', event_time,
    event_type = 'view', event_type = 'cart', event_type = 'purchase')
FROM read_parquet('events/*.parquet')
GROUP BY user_id;

-- Query CSV files
SELECT user_id,
  retention(
    event_date = '2024-01-01',
    event_date = '2024-01-02',
    event_date = '2024-01-03')
FROM read_csv('events.csv')
GROUP BY user_id;
```

## Scaling and Memory

### How much data can the extension handle?

The extension has been benchmarked at scale with Criterion.rs:

| Function | Tested Scale | Throughput | Memory Model |
|---|---|---|---|
| `sessionize` | 1 billion rows | 862 Melem/s | O(1) per partition segment |
| `retention` | 1 billion rows | 338 Melem/s | O(1) -- single `u32` bitmask |
| `window_funnel` | 100 million rows | 131 Melem/s | O(n) -- 16 bytes per event |
| `sequence_match` | 100 million rows | 95 Melem/s | O(n) -- 16 bytes per event |
| `sequence_count` | 100 million rows | 69 Melem/s | O(n) -- 16 bytes per event |
| `sequence_next_node` | 10 million rows | 8 Melem/s | O(n) -- 32 bytes per event |

In practice, real-world datasets with billions of rows are easily handled because
the functions operate on partitioned groups (e.g., per-user), not the entire table
at once. A table with 1 billion rows across 10 million users has only 100 events
per user on average.

### How much memory do the functions use?

Memory usage depends on the function category:

**O(1)-state functions** (constant memory regardless of group size):
- `sessionize`: Tracks only `first_ts`, `last_ts`, and `boundaries` count. Requires
  a few dozen bytes per partition segment, regardless of how many events exist.
- `retention`: Stores a single `u32` bitmask (4 bytes) per group. One billion
  rows with retention requires effectively zero additional memory.

**Event-collecting functions** (linear memory proportional to group size):
- `window_funnel`, `sequence_match`, `sequence_count`, `sequence_match_events`:
  Store every event as a 16-byte `Event` struct. For a group with 10,000 events,
  this requires approximately 160 KB.
- `sequence_next_node`: Stores every event as a 32-byte `NextNodeEvent` struct
  (includes an `Rc<str>` reference to the value column). For a group with 10,000
  events, this requires approximately 320 KB plus string storage.

**Rule of thumb**: For event-collecting functions, estimate memory as
`16 bytes * (events in largest group)` for most functions, or
`32 bytes * (events in largest group)` for `sequence_next_node`. The group is
defined by `GROUP BY` for aggregate functions or `PARTITION BY` for `sessionize`.

### What happens if a single user has millions of events?

The extension will process it correctly, but memory usage scales linearly with the
number of events in that group. A single user with 10 million events will require
approximately 160 MB of memory for event-collecting functions (16 bytes times 10M).

If you have users with extremely large event counts, consider pre-filtering to a
relevant time window before applying behavioral functions:

```sql
-- Pre-filter to last 90 days before funnel analysis
SELECT user_id,
  window_funnel(INTERVAL '1 hour', event_time,
    event_type = 'view', event_type = 'cart', event_type = 'purchase')
FROM events
WHERE event_time >= CURRENT_TIMESTAMP - INTERVAL '90 days'
GROUP BY user_id;
```

### Does the extension support out-of-core processing?

The extension itself does not implement out-of-core (disk-spilling) strategies.
However, DuckDB's query engine handles memory management for the overall query
pipeline. The aggregate state for each group is held in memory during execution.
For most workloads where `GROUP BY` partitions data into reasonably sized groups
(thousands to tens of thousands of events per user), this is not a concern.

## Combine and Aggregate Behavior

### How does GROUP BY work with these functions?

The aggregate functions (`retention`, `window_funnel`, `sequence_match`,
`sequence_count`, `sequence_match_events`, `sequence_next_node`) follow standard
SQL `GROUP BY` semantics. Each group produces one output row:

```sql
-- One result per user
SELECT user_id,
  window_funnel(INTERVAL '1 hour', event_time,
    event_type = 'view', event_type = 'purchase') as steps
FROM events
GROUP BY user_id;

-- One result per user per day
SELECT user_id, event_time::DATE as day,
  sequence_match('(?1).*(?2)', event_time,
    event_type = 'view', event_type = 'purchase') as converted
FROM events
GROUP BY user_id, day;
```

If you omit `GROUP BY`, the function aggregates over the entire table (all rows
form a single group):

```sql
-- Single result for the entire table
SELECT retention(
    event_date = '2024-01-01',
    event_date = '2024-01-02'
  ) as overall_retention
FROM events;
```

### What is the combine operation and why does it matter?

DuckDB processes aggregate functions using a segment tree, which requires merging
partial aggregate states. The `combine` operation merges two states into one.
For event-collecting functions, this means appending one state's events to
another's.

This is an internal implementation detail that you do not need to worry about for
correctness. However, it affects performance:

- `sessionize` and `retention` have O(1) combine (constant time regardless of
  data size), making them extremely fast even at billion-row scale.
- Event-collecting functions have O(m) combine where m is the number of events
  in the source state. This is still fast -- 100 million events process in
  under 1 second -- but it is the dominant cost at scale.

### Can I use these functions with PARTITION BY (window functions)?

Only `sessionize` is a window function. All other functions are aggregate
functions.

```sql
-- Correct: sessionize is a window function
SELECT sessionize(event_time, INTERVAL '30 minutes') OVER (
    PARTITION BY user_id ORDER BY event_time
) as session_id
FROM events;

-- Correct: window_funnel is an aggregate function
SELECT user_id,
  window_funnel(INTERVAL '1 hour', event_time, cond1, cond2)
FROM events
GROUP BY user_id;
```

You can use `GROUP BY` with the aggregate functions to partition results by any
column or expression -- user ID, date, campaign, device type, or any combination.

### Can I nest these functions or use them in subqueries?

Yes. The results of behavioral functions can be used in subqueries, CTEs, and
outer queries like any other SQL expression:

```sql
-- Use window_funnel results in a downstream aggregation
WITH user_funnels AS (
    SELECT user_id,
      window_funnel(INTERVAL '1 hour', event_time,
        event_type = 'view', event_type = 'cart', event_type = 'purchase'
      ) as steps
    FROM events
    GROUP BY user_id
)
SELECT steps, COUNT(*) as user_count
FROM user_funnels
GROUP BY steps
ORDER BY steps;
```

## Integration

### How do I use this extension with Python?

Use the `duckdb` Python package. Load the extension after creating a connection:

```python
import duckdb

conn = duckdb.connect()
conn.execute("SET allow_unsigned_extensions = true")
conn.execute("LOAD 'target/release/libduckdb_behavioral.so'")

# Run behavioral queries and get results as a DataFrame
df = conn.execute("""
    SELECT user_id,
      window_funnel(INTERVAL '1 hour', event_time,
        event_type = 'view',
        event_type = 'cart',
        event_type = 'purchase') as steps
    FROM events
    GROUP BY user_id
""").fetchdf()
```

You can also pass pandas DataFrames directly to DuckDB:

```python
import pandas as pd

events_df = pd.DataFrame({
    'user_id': ['u1', 'u1', 'u1', 'u2', 'u2'],
    'event_time': pd.to_datetime([
        '2024-01-01 10:00', '2024-01-01 10:30', '2024-01-01 11:00',
        '2024-01-01 09:00', '2024-01-01 09:15'
    ]),
    'event_type': ['view', 'cart', 'purchase', 'view', 'cart']
})

result = conn.execute("""
    SELECT user_id,
      window_funnel(INTERVAL '1 hour', event_time,
        event_type = 'view', event_type = 'cart', event_type = 'purchase'
      ) as steps
    FROM events_df
    GROUP BY user_id
""").fetchdf()
```

### How do I use this extension with Node.js?

Use the `duckdb-async` or `duckdb` npm package:

```javascript
const duckdb = require('duckdb');
const db = new duckdb.Database(':memory:', {
    allow_unsigned_extensions: 'true'
});

db.run("LOAD 'target/release/libduckdb_behavioral.so'", (err) => {
    if (err) throw err;

    db.all(`
        SELECT user_id,
          window_funnel(INTERVAL '1 hour', event_time,
            event_type = 'view',
            event_type = 'cart',
            event_type = 'purchase') as steps
        FROM read_parquet('events.parquet')
        GROUP BY user_id
    `, (err, rows) => {
        console.log(rows);
    });
});
```

### How do I use this extension with dbt?

dbt-duckdb supports loading extensions. Add the extension to your `profiles.yml`:

```yaml
my_project:
  target: dev
  outputs:
    dev:
      type: duckdb
      path: ':memory:'
      extensions:
        - path: /absolute/path/to/libduckdb_behavioral.so
          unsigned: true
```

Then use the behavioral functions directly in your dbt models:

```sql
-- models/staging/stg_user_funnels.sql
SELECT
    user_id,
    window_funnel(
        INTERVAL '1 hour',
        event_time,
        event_type = 'view',
        event_type = 'cart',
        event_type = 'purchase'
    ) as furthest_step
FROM {{ ref('stg_events') }}
GROUP BY user_id
```

```sql
-- models/staging/stg_user_sessions.sql
SELECT
    user_id,
    event_time,
    sessionize(event_time, INTERVAL '30 minutes') OVER (
        PARTITION BY user_id ORDER BY event_time
    ) as session_id
FROM {{ ref('stg_events') }}
```

### Can I use the extension in DuckDB's WASM or HTTP client?

No. The extension is a native loadable binary (`.so` on Linux, `.dylib` on macOS)
and requires the native DuckDB runtime. It cannot be loaded in DuckDB-WASM
(browser) or through the DuckDB HTTP API without a native DuckDB server process
backing the connection.

### Does the extension work with DuckDB MotherDuck?

MotherDuck supports a curated set of extensions. Since `duckdb-behavioral` is not
yet part of the official DuckDB extension repository, it cannot be loaded in
MotherDuck. You can use it with any local or self-hosted DuckDB instance.

## Performance

### How fast is the extension?

Headline benchmarks (Criterion.rs, 95% CI):

| Function | Scale | Throughput |
|---|---|---|
| `sessionize` | 1 billion | 862 Melem/s |
| `retention` | 1 billion | 338 Melem/s |
| `window_funnel` | 100 million | 131 Melem/s |
| `sequence_match` | 100 million | 95 Melem/s |
| `sequence_count` | 100 million | 69 Melem/s |

See the [Performance](./internals/performance.md) page for full methodology and
optimization history.

### Can I run the benchmarks myself?

Yes:

```bash
cargo bench                    # Run all benchmarks
cargo bench -- sessionize      # Run a specific group
cargo bench -- sequence_match  # Another specific group
```

Results are stored in `target/criterion/` and automatically compared against
previous runs.

### What can I do to improve query performance?

1. **Pre-filter by time range.** Reduce the number of events before applying
   behavioral functions. A `WHERE event_time >= '2024-01-01'` clause can
   dramatically reduce the working set.

2. **Use `GROUP BY` to keep group sizes reasonable.** Partitioning by `user_id`
   ensures each group contains only one user's events rather than the entire table.

3. **Order by timestamp.** While not required for correctness, an `ORDER BY` on
   the timestamp column enables the presorted detection optimization, which
   skips the internal O(n log n) sort.

4. **Use Parquet format.** Parquet's columnar storage and predicate pushdown
   work well with DuckDB's query optimizer, reducing I/O for behavioral queries
   that typically read only a few columns.
