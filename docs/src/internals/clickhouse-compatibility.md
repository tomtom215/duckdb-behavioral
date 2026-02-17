# ClickHouse Compatibility

`duckdb-behavioral` implements behavioral analytics functions inspired by
ClickHouse's
[parametric aggregate functions](https://clickhouse.com/docs/en/sql-reference/aggregate-functions/parametric-functions).
This page documents the compatibility status, semantic differences, and
remaining gaps.

## Compatibility Matrix

### Behavioral Parametric Functions

All six ClickHouse behavioral parametric functions are implemented:

| ClickHouse Function | duckdb-behavioral | Status |
|---|---|---|
| `retention(cond1, cond2, ...)` | `retention(cond1, cond2, ...)` | Complete |
| `windowFunnel(window)(timestamp, cond1, ...)` | `window_funnel(window, timestamp, cond1, ...)` | Complete |
| `windowFunnel(window, 'strict')(...)` | `window_funnel(window, 'strict', ...)` | Complete |
| `windowFunnel(window, 'strict_order')(...)` | `window_funnel(window, 'strict_order', ...)` | Complete |
| `windowFunnel(window, 'strict_deduplication')(...)` | `window_funnel(window, 'strict_deduplication', ...)` | Complete (alias for `'strict'`) |
| `windowFunnel(window, 'strict_increase')(...)` | `window_funnel(window, 'strict_increase', ...)` | Complete |
| `windowFunnel(window, 'strict_once')(...)` | `window_funnel(window, 'strict_once', ...)` | Complete |
| `windowFunnel(window, 'allow_reentry')(...)` | `window_funnel(window, 'allow_reentry', ...)` | Complete |
| `sequenceMatch(pattern)(timestamp, cond1, ...)` | `sequence_match(pattern, timestamp, cond1, ...)` | Complete |
| `sequenceCount(pattern)(timestamp, cond1, ...)` | `sequence_count(pattern, timestamp, cond1, ...)` | Complete |
| N/A (duckdb-behavioral extension) | `sequence_match_events(pattern, timestamp, cond1, ...)` | Extension |
| `sequenceNextNode(dir, base)(ts, val, base_cond, ev1, ...)` | `sequence_next_node(dir, base, ts, val, base_cond, ev1, ...)` | Complete |

### Non-Behavioral Parametric Functions (Out of Scope)

ClickHouse's [parametric aggregate functions](https://clickhouse.com/docs/sql-reference/aggregate-functions/parametric-functions)
page documents 10 functions total. Six are behavioral analytics functions (all
implemented above). The remaining four are general-purpose aggregate functions
that share the parametric calling convention but operate on fundamentally
different problem domains. They are **not** behavioral analytics functions and
are therefore **not** in scope for this extension.

The distinction is precise: behavioral analytics functions analyze **sequences
of user actions over time** — they require a timestamp, operate on ordered
event streams, and answer questions about user journeys, funnels, retention,
and patterns. The four excluded functions do not operate on event sequences,
do not require timestamps, and do not model user behavior.

| ClickHouse Function | Signature | Return Type | What It Does | Why Not Behavioral |
|---|---|---|---|---|
| `histogram(number_of_bins)(values)` | `(UInt)(Column)` | `Array(Tuple(Float64, Float64, Float64))` | Calculates an adaptive histogram over numeric values using a streaming parallel decision tree algorithm. Returns array of `(lower_bound, upper_bound, height)` tuples. | **Statistical distribution.** Aggregates continuous numeric values into frequency buckets. No timestamps, no event ordering, no user actions. Equivalent to numpy's `histogram` or SQL `WIDTH_BUCKET`. |
| `uniqUpTo(N)(x)` | `(UInt8)(Column)` | `UInt64` | Counts distinct values up to threshold N (max 100). Returns exact count if ≤ N, returns N+1 if exceeded. | **Cardinality estimation.** Counts unique values with a cap. No timestamps, no sequences. Equivalent to `COUNT(DISTINCT x)` with a limit. |
| `sumMapFiltered(keys_to_keep)(keys, values)` | `(Array)(Array, Array)` | `Tuple(Array, Array)` | Sums numeric values grouped by key, filtering to only the specified keys. Operates on parallel key/value arrays. | **Map aggregation.** Performs key-filtered summation over parallel arrays. No timestamps, no event ordering. Equivalent to a filtered `GROUP BY` with `SUM`. |
| `sumMapFilteredWithOverflow(keys_to_keep)(keys, values)` | `(Array)(Array, Array)` | `Tuple(Array, Array)` | Identical to `sumMapFiltered` except it preserves the input data type for summation instead of promoting to avoid overflow. | **Map aggregation variant.** Same as above with different overflow semantics. |

These four functions happen to share ClickHouse's parametric calling convention
`function(params)(args)` with the behavioral functions, which is why they appear
on the same documentation page. However, the parametric convention is a syntax
pattern, not a semantic category. DuckDB does not use this convention at all
(all parameters are flat), making the grouping irrelevant to our extension's
scope.

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

All five ClickHouse modes are implemented, plus one extension mode:

- **Default**: Greedy forward scan, multi-step advancement per event.
- **strict** / **strict_deduplication**: In ClickHouse, these are aliases for
  the same behavior — the chain breaks when the previously-matched condition
  fires again. Our extension accepts both SQL strings and maps them to the same
  internal mode (`STRICT`, 0x01).
- **strict_order**: No earlier conditions may fire between matched steps.
- **strict_increase**: Strictly increasing timestamps required between steps.
- **strict_once**: At most one step advancement per event.
- **allow_reentry**: Entry condition resets the funnel mid-chain.
- **timestamp_dedup** _(extension)_: Timestamp-based deduplication — events with
  the same timestamp as the previously-matched step are skipped. This mode is
  not present in ClickHouse and is accessed via the SQL string `'timestamp_dedup'`.

Modes are independently combinable (e.g., `'strict_increase, strict_once'`),
matching ClickHouse semantics.

> **Note on `strict` semantics**: Our `STRICT` mode includes an additional guard:
> if an event matches both the previously-matched condition AND the next target
> condition, it does NOT break the chain (it advances instead). ClickHouse's
> `strict` mode may break the chain unconditionally when the previous condition
> fires, regardless of whether the current condition also fires. This difference
> only manifests when events satisfy multiple conditions simultaneously, which is
> uncommon in typical funnel data.

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

## Extensions Beyond ClickHouse

`duckdb-behavioral` includes functionality beyond ClickHouse's behavioral
analytics functions:

| Function/Feature | Description |
|---|---|
| `sessionize` | Window function for session ID assignment (no ClickHouse equivalent) |
| `sequence_match_events` | Returns matched timestamps as `LIST(TIMESTAMP)` |
| `'timestamp_dedup'` mode | Timestamp-based deduplication in `window_funnel` |
| `(?t!=N)` time constraint | Not-equal operator in sequence patterns |
| No experimental flags | `sequence_next_node` works without `SET allow_experimental_funnel_functions = 1` |

## Feature Parity Status

All six ClickHouse behavioral parametric functions are implemented with complete
coverage of documented parameters, modes, and return types. The `sessionize`
function and `sequence_match_events` function are extensions beyond ClickHouse's
feature set.

### Known Semantic Differences

1. **`strict` mode guard**: Our implementation does not break the chain when an
   event matches both the previously-matched condition and the next target
   condition. ClickHouse may break unconditionally. This affects only events
   satisfying multiple conditions simultaneously.

2. **`strict_deduplication` mapping**: In ClickHouse, `'strict_deduplication'`
   is an alias for `'strict'`. Our extension correctly maps both strings to the
   same behavior as of Session 16. Previous versions (≤ Session 15) mapped
   `'strict_deduplication'` to a different timestamp-based dedup behavior.

3. **Window parameter type**: ClickHouse accepts an integer (seconds); our
   extension accepts DuckDB's `INTERVAL` type. The semantics are equivalent.
