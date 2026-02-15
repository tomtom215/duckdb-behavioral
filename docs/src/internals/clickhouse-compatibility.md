# ClickHouse Compatibility

`duckdb-behavioral` implements behavioral analytics functions inspired by
ClickHouse's
[parametric aggregate functions](https://clickhouse.com/docs/en/sql-reference/aggregate-functions/parametric-functions).
This page documents the compatibility status, semantic differences, and
remaining gaps.

## Compatibility Matrix

| ClickHouse Function | duckdb-behavioral | Status |
|---|---|---|
| `retention(cond1, cond2, ...)` | `retention(cond1, cond2, ...)` | Complete |
| `windowFunnel(window)(timestamp, cond1, ...)` | `window_funnel(window, timestamp, cond1, ...)` | Complete |
| `windowFunnel(window, 'strict')(...)` | `window_funnel(window, 'strict', ...)` | Complete |
| `windowFunnel(window, 'strict_order')(...)` | `window_funnel(window, 'strict_order', ...)` | Complete |
| `windowFunnel(window, 'strict_deduplication')(...)` | `window_funnel(window, 'strict_deduplication', ...)` | Complete |
| `windowFunnel(window, 'strict_increase')(...)` | `window_funnel(window, 'strict_increase', ...)` | Complete |
| `windowFunnel(window, 'strict_once')(...)` | `window_funnel(window, 'strict_once', ...)` | Complete |
| `windowFunnel(window, 'allow_reentry')(...)` | `window_funnel(window, 'allow_reentry', ...)` | Complete |
| `sequenceMatch(pattern)(timestamp, cond1, ...)` | `sequence_match(pattern, timestamp, cond1, ...)` | Complete |
| `sequenceCount(pattern)(timestamp, cond1, ...)` | `sequence_count(pattern, timestamp, cond1, ...)` | Complete |
| N/A (duckdb-behavioral extension) | `sequence_match_events(pattern, timestamp, cond1, ...)` | Complete |
| `sequenceNextNode(dir, base)(ts, val, base_cond, ev1, ...)` | `sequence_next_node(dir, base, ts, val, base_cond, ev1, ...)` | Complete |

## Syntax Differences

ClickHouse uses a two-level call syntax for parametric aggregate functions:

```sql
-- ClickHouse syntax
windowFunnel(3600)(timestamp, cond1, cond2, cond3)

-- duckdb-behavioral syntax
window_funnel(INTERVAL '1 hour', timestamp, cond1, cond2, cond3)
```

Key differences:

| Aspect | ClickHouse | duckdb-behavioral |
|---|---|---|
| Window parameter | Seconds as integer | DuckDB `INTERVAL` type |
| Mode parameter | Second argument in parameter list | Optional `VARCHAR` before timestamp |
| Function name | camelCase | snake_case |
| Session function | Not a built-in behavioral function | `sessionize` (window function) |
| Condition limit | 32 | 32 |

The `sessionize` function has no direct ClickHouse equivalent. ClickHouse
provides session analysis through different mechanisms.

## Semantic Compatibility

### retention

Fully compatible. The anchor condition semantics match ClickHouse: `result[0]`
reflects the anchor, `result[i]` requires both the anchor and condition `i` to
have been true in the group.

### window_funnel

All six modes are implemented and match ClickHouse behavior:

- **Default**: Greedy forward scan, multi-step advancement per event.
- **strict**: Previous step condition must not refire before next step matches.
- **strict_order**: No earlier conditions may fire between matched steps.
- **strict_deduplication**: Same-timestamp events deduplicated per condition.
- **strict_increase**: Strictly increasing timestamps required between steps.
- **strict_once**: At most one step advancement per event.
- **allow_reentry**: Entry condition resets the funnel mid-chain.

Modes are independently combinable (e.g., `'strict_increase, strict_once'`),
matching ClickHouse semantics.

### sequence_match and sequence_count

Pattern syntax matches ClickHouse:

- `(?N)` condition references (1-indexed)
- `.` single event wildcard
- `.*` zero-or-more event wildcard
- `(?t>=N)`, `(?t<=N)`, `(?t>N)`, `(?t<N)`, `(?t==N)`, `(?t!=N)` time constraints (seconds)

The NFA executor uses lazy matching for `.*`, consistent with ClickHouse
semantics.

### sequence_match_events

Analogous to ClickHouse's pattern matching with timestamp extraction. Returns
the timestamps of each `(?N)` step as a `LIST(TIMESTAMP)`.

### sequence_next_node

Implements ClickHouse's `sequenceNextNode` for flow analysis. Uses simple
sequential matching (not NFA patterns) with direction and base parameters.

The `direction` parameter (`'forward'` / `'backward'`) controls scan direction.
The `base` parameter (`'head'` / `'tail'` / `'first_match'` / `'last_match'`)
controls which starting point to use.

Uses a dedicated `NextNodeEvent` struct with per-event `Arc<str>` storage
(separate from the `Copy` `Event` struct used by other functions).

## Feature Parity

All ClickHouse behavioral analytics functions are implemented. There are no
remaining feature gaps.
