# FAQ

Frequently asked questions about `duckdb-behavioral`.

## Loading the Extension

### How do I install the extension?

The extension is available in the
[DuckDB Community Extensions](https://github.com/duckdb/community-extensions)
repository. Install and load with:

```sql
INSTALL behavioral FROM community;
LOAD behavioral;
```

No build tools, compilation, or `-unsigned` flag required.

### Can I build from source instead?

Yes. DuckDB only loads files ending in `.duckdb_extension` that carry its
metadata footer, so build in release mode and append the footer first:

```bash
cargo build --release
git submodule update --init --recursive   # first time only
cp target/release/libbehavioral.so /tmp/behavioral.duckdb_extension   # .dylib on macOS
python3 extension-ci-tools/scripts/append_extension_metadata.py \
  -l /tmp/behavioral.duckdb_extension -n behavioral \
  -p linux_amd64 -dv v1.2.0 -ev v0.9.1 \
  -o /tmp/behavioral.duckdb_extension
```

Locally built extensions are unsigned, so load them with `-unsigned`:

```bash
duckdb -unsigned -c "LOAD '/tmp/behavioral.duckdb_extension'; SELECT behavioral_version();"
```

`make configure release` does the same through DuckDB's `extension-ci-tools`
and writes `build/release/behavioral.duckdb_extension`. See
[Getting Started](./getting-started.md) for the platform names.

### The extension fails to load. What should I check?

1. **DuckDB version mismatch**: The community repository publishes a build
   per DuckDB release; a release newer than the last community build may not
   have one yet. A locally built extension is stamped for the stable C API
   (`-dv v1.2.0`, ABI type `C_STRUCT`) and loads into any DuckDB release with
   that C API or newer; stamping it `C_STRUCT_UNSTABLE` would pin it to the one
   release named by `-dv`.

2. **Missing `-unsigned` flag** (local builds only): DuckDB rejects unsigned
   extensions by default. Use `duckdb -unsigned` or set
   `allow_unsigned_extensions=true`. This does not apply when installing via
   `INSTALL behavioral FROM community`.

3. **Wrong file path** (local builds only): Point `LOAD` at the stamped
   `.duckdb_extension` file (`build/release/behavioral.duckdb_extension` from
   `make configure release`, or the file you ran `append_extension_metadata.py`
   on). DuckDB refuses the raw `.so`/`.dylib` from `cargo build --release`
   ("DuckDB extensions are files ending with '.duckdb_extension'").

4. **Platform mismatch** (local builds only): An extension built on Linux cannot
   be loaded on macOS, and vice versa. The community extension handles platform
   selection automatically.

## Functions

### What is the difference between sequence_match, sequence_count, and sequence_match_events?

All three use the same pattern syntax and matching engine, but return different results:

| Function | Returns | Use Case |
|---|---|---|
| `sequence_match` | `BOOLEAN` | Did the pattern occur at all? |
| `sequence_count` | `BIGINT` | How many times did the pattern occur (non-overlapping)? |
| `sequence_match_events` | `LIST(TIMESTAMP)` | At what timestamps did each step match? |

Use `sequence_match` for filtering, `sequence_count` for frequency analysis, and
`sequence_match_events` for diagnostic investigation of when each step was
satisfied.

### How many boolean conditions can I use?

| Function | Conditions |
|---|---|
| `window_funnel`, `window_funnel_events` | 1 to 32 |
| `retention`, `sequence_match`, `sequence_count`, `sequence_match_events` | 2 to 32 |
| `sequence_next_node` | a base condition plus 1 to 32 event conditions |

The conditions are stored internally as a `u32` bitmask, so 32 is a hard
limit of the data type. ClickHouse's `windowFunnel` and `sequenceMatch` also
stop at 32; its `sequenceNextNode` allows up to 64 event conditions.

### How are NULL values handled?

- **NULL timestamps**: Rows with NULL timestamps are ignored during update.
- **NULL conditions**: NULL boolean conditions are treated as `false`.
- **NULL pattern**: `sequence_match` and `sequence_count` return `NULL`;
  `sequence_match_events` returns an empty list. An empty pattern string is
  malformed and raises an error.
- **NULL configuration** (other than the pattern): a `NULL` `window_funnel`
  window skips that row; a `NULL` mode means no mode; a `NULL`
  `sequence_next_node` direction is treated as `'forward'` and a `NULL` base as
  `'first_match'`.
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

Five ClickHouse-compatible modes plus one extension mode are available, each
adding an independent constraint:

| Mode | Effect |
|---|---|
| `strict` | Previously-matched condition must not refire before next step matches |
| `strict_deduplication` | Alias for `strict` (matches ClickHouse semantics) |
| `strict_order` | No earlier conditions may fire between matched steps |
| `strict_increase` | Require strictly increasing timestamps between steps |
| `strict_once` | Each event can advance the funnel by at most one step |
| `allow_reentry` | Reset the funnel when condition 1 fires again |
| `timestamp_dedup` | _Extension._ Skip events with the same timestamp as the previous step |

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
| Condition limit | 32 (64 for `sequenceNextNode`) | 32 |

The `INTERVAL` type is more expressive than raw seconds. You can write
`INTERVAL '1 hour'`, `INTERVAL '30 minutes'`, or `INTERVAL '2 days'` instead of
computing the equivalent number of seconds.

### Does this extension have a sessionize equivalent in ClickHouse?

No. The `sessionize` function has no direct equivalent in ClickHouse's behavioral
analytics function set. It is an addition in this extension for assigning
session IDs based on inactivity gaps, designed to be used as a window
aggregate with `OVER (PARTITION BY ... ORDER BY ...)`.

### Do I need to set any experimental flags?

No. In ClickHouse, `sequenceNextNode` requires
`SET allow_experimental_funnel_functions = 1`. In `duckdb-behavioral`, all eight
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

Do **not** put an `ORDER BY` inside the function call
(`window_funnel(... ORDER BY ts)`): DuckDB runs such ordered aggregates
through a code path that crashes every C API aggregate, this extension's
included (see [Which query shapes crash DuckDB?](#which-query-shapes-crash-duckdb)).
It is never needed, because every function sorts by timestamp itself.

When events already arrive in timestamp order, a presorted check skips the
O(n log n) sort, reducing finalize to O(n).

`sessionize` is the exception: it is an aggregate meant to be used with
`OVER (... ORDER BY ...)`, and the `ORDER BY` in the `OVER` clause is what
makes it assign a running session ID per row (without `OVER` it returns the
number of sessions in the group):

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

The Rust aggregate state has been benchmarked at scale with Criterion.rs.
These are microbenchmarks of update/combine/finalize, not end-to-end SQL
queries, taken from PERF.md Session 15 (before v0.8.0, not re-measured since):

| Benchmark | Tested Scale | Throughput | Memory Model |
|---|---|---|---|
| `sessionize_update` | 1 billion events | 830 Melem/s | O(1) per partition segment |
| `retention_combine` | 100 million states | 365 Melem/s | O(1) -- single `u32` bitmask |
| `window_funnel_finalize` | 100 million events | 126 Melem/s | O(n) -- 16 bytes per event |
| `sequence_match` | 100 million events | 95 Melem/s | O(n) -- 16 bytes per event |
| `sequence_count` | 100 million events | 85 Melem/s | O(n) -- 16 bytes per event |
| `sequence_match_events` | 100 million events | 93 Melem/s | O(n) -- 16 bytes per event |
| `sequence_next_node` | 10 million events | 18 Melem/s | O(n) -- 32 bytes per event |

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
  (includes an `Arc<str>` reference to the value column). For a group with 10,000
  events, this requires approximately 320 KB plus string storage.

**Rule of thumb**: For event-collecting functions, estimate memory as
`16 bytes * (events in largest group)` for most functions, or
`32 bytes * (events in largest group)` for `sequence_next_node`. The group is
defined by `GROUP BY` for aggregate functions or `PARTITION BY` for `sessionize`.

### What happens if a single user has millions of events?

Memory scales linearly with the number of events in that group: a single user
with 10 million events needs about 160 MB for the event-collecting functions
(16 bytes per event). While finalizing, `sequence_*` patterns outside the
fast paths below also need about 8 bytes per event, plus one byte per event
for each `(?N)` or `.` step.

Time is linear for `window_funnel`, `retention`, `sessionize`,
`sequence_next_node`, and for `sequence_*` patterns built only from
conditions and `.*`. Other `sequence_*` patterns (time constraints, `.`,
adjacent conditions mixed with `.*`) take O(s · n log n) for `s` pattern
steps. With one group of 10 million events, `sequence_count` took 0.78–0.90 s
for `(?1).*(?t<5)(?2).*(?3)` and for `(?1)(?t>=5)(?3)` (DuckDB 1.5.6, 3 runs
each). Through v0.9.1 these patterns used a backtracking search that was
quadratic when the pattern did not complete: 8.3–9.1 s at 32,000 events, and
32–35 s for `sequence_match_events`. The matchers do not check for query
interruption, so a single very large group cannot be cancelled mid-finalize.

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
  in the source state. For scale: in the Criterion microbenchmarks, updating
  and finalizing a single 100-million-event state takes 791 ms for
  `window_funnel` and 1.05-1.18 s for the `sequence_*` functions; with many
  partial states, combine becomes the dominant cost.

### Can I use these functions with PARTITION BY (window functions)?

Yes. DuckDB can run any aggregate as a window function, and `sessionize` is
designed to be used that way:

```sql
SELECT sessionize(event_time, INTERVAL '30 minutes') OVER (
    PARTITION BY user_id ORDER BY event_time
) as session_id
FROM events;
```

The other functions are normally used with `GROUP BY`:

```sql
SELECT user_id,
  window_funnel(INTERVAL '1 hour', event_time, cond1, cond2)
FROM events
GROUP BY user_id;
```

They also work over running or sliding frames, for example
`OVER (PARTITION BY user_id ORDER BY event_time)` gives a running funnel
step per row. Avoid frames that cover the whole partition; see the next
question.

### Which query shapes crash DuckDB?

A defect in DuckDB's C API for aggregate functions
([duckdb/duckdb#26109](https://github.com/duckdb/duckdb/issues/26109)) makes
DuckDB read out of bounds, and usually crash with a segmentation fault, when
**any** aggregate registered through the C API, including every function in
this extension, is called in these shapes:

| Shape | Example |
|---|---|
| `ORDER BY` inside the call | `retention(c1, c2 ORDER BY ts)` |
| Empty window | `window_funnel(...) OVER ()` |
| Frame covering the whole partition | `OVER (PARTITION BY u ORDER BY ts ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING)` |

The extension cannot detect or refuse these: DuckDB passes a one-element state
array while reporting several rows, and the C API has no bind hook through
which an aggregate could reject the query. Verified on DuckDB 1.5.6 (still
present) with valgrind, which also showed no invalid reads for `GROUP BY`,
`FILTER`, `DISTINCT`, running frames (`OVER (... ORDER BY ts)`), sliding
frames, `EXCLUDE`, or `OVER (PARTITION BY u)`.

Workarounds: drop the `ORDER BY` inside the call (the functions sort by
timestamp themselves), and compute a whole-partition value with `GROUP BY`
and join it back:

```sql
WITH f AS (
  SELECT user_id, window_funnel(INTERVAL '1 hour', ts, c1, c2) AS step
  FROM events GROUP BY user_id
)
SELECT e.*, f.step FROM events e JOIN f USING (user_id);
```

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

Use the `duckdb` Python package. Install and load the extension from the
community repository:

```python
import duckdb

conn = duckdb.connect()
conn.execute("INSTALL behavioral FROM community")
conn.execute("LOAD behavioral")

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
const db = new duckdb.Database(':memory:');

db.run("INSTALL behavioral FROM community", (err) => {
    if (err) throw err;
    db.run("LOAD behavioral", (err) => {
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
});
```

### How do I use this extension with dbt?

dbt-duckdb supports loading community extensions. Add the extension to your
`profiles.yml`:

```yaml
my_project:
  target: dev
  outputs:
    dev:
      type: duckdb
      path: ':memory:'
      extensions:
        - name: behavioral
          repo: community
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

## Performance

### How fast is the extension?

Headline microbenchmarks of the Rust aggregate state (Criterion.rs, 95% CI,
PERF.md Session 15, recorded before v0.8.0 and not re-measured; these are not
end-to-end SQL timings):

| Benchmark | Scale | Throughput |
|---|---|---|
| `sessionize_update` | 1 billion events | 830 Melem/s |
| `retention_combine` | 100 million states | 365 Melem/s |
| `window_funnel_finalize` | 100 million | 126 Melem/s |
| `sequence_match` | 100 million | 95 Melem/s |
| `sequence_count` | 100 million | 85 Melem/s |
| `sequence_match_events` | 100 million | 93 Melem/s |
| `sequence_next_node` | 10 million | 18 Melem/s |

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

3. **Don't add `ORDER BY` for the functions' sake.** Every event-collecting
   function sorts by timestamp itself, and when rows already arrive in
   timestamp order (for example, from a file written in that order) a
   presorted check skips the O(n log n) sort. Never put `ORDER BY` inside the
   call (`window_funnel(... ORDER BY ts)`): it crashes DuckDB (see
   [Which query shapes crash DuckDB?](#which-query-shapes-crash-duckdb)).

4. **Use Parquet format.** Parquet's columnar storage and predicate pushdown
   work well with DuckDB's query optimizer, reducing I/O for behavioral queries
   that typically read only a few columns.
