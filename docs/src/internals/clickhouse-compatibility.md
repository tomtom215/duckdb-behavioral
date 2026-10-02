# ClickHouse Compatibility

`duckdb-behavioral` implements behavioral analytics functions inspired by
ClickHouse's
[parametric aggregate functions](https://clickhouse.com/docs/en/sql-reference/aggregate-functions/parametric-functions).
This page documents the compatibility status, semantic differences, and
remaining gaps.

## Compatibility Matrix

### Behavioral Parametric Functions

All six ClickHouse behavioral parametric functions are implemented. "Verified"
means differential testing against ClickHouse 26.9.8.3 found no difference
outside the deliberate ones listed under
[Known Semantic Differences](#known-semantic-differences).

| ClickHouse Function | duckdb-behavioral | Status |
|---|---|---|
| `retention(cond1, cond2, ...)` | `retention(cond1, cond2, ...)` | Verified |
| `windowFunnel(window)(timestamp, cond1, ...)` | `window_funnel(window, timestamp, cond1, ...)` | Verified |
| `windowFunnel(window, 'strict_deduplication')(...)` | `window_funnel(window, 'strict_deduplication', ...)` (also `'strict'`) | Verified |
| `windowFunnel(window, 'strict_order')(...)` | `window_funnel(window, 'strict_order', ...)` | Verified |
| `windowFunnel(window, 'strict_increase')(...)` | `window_funnel(window, 'strict_increase', ...)` | Verified; ClickHouse defect fixed (difference 1) |
| `windowFunnel(window, 'strict_once')(...)` | `window_funnel(window, 'strict_once', ...)` | Verified; deterministic ties (difference 2) |
| `windowFunnel(window, 'strict_order', 'allow_reentry')(...)` | `window_funnel(window, 'strict_order, allow_reentry', ...)` | Verified |
| `sequenceMatch(pattern)(timestamp, cond1, ...)` | `sequence_match(pattern, timestamp, cond1, ...)` | Verified; anchoring differs (difference 4) |
| `sequenceCount(pattern)(timestamp, cond1, ...)` | `sequence_count(pattern, timestamp, cond1, ...)` | Verified; anchoring differs (difference 4) |
| `sequenceMatchEvents(pattern)(timestamp, cond1, ...)` | `sequence_match_events(pattern, timestamp, cond1, ...)` | Verified; reports the matching events (difference 5) |
| `sequenceNextNode(dir, base)(ts, val, base_cond, ev1, ...)` | `sequence_next_node(dir, base, ts, val, base_cond, ev1, ...)` | Verified; deterministic ties (difference 2) |
| N/A (duckdb-behavioral extension) | `window_funnel_events(window[, mode], timestamp, cond1, ...)` | Extension |

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
| Session function | Not a built-in behavioral function | `sessionize` (used with `OVER`) |
| Condition limit | 32 (`sequenceNextNode`: 64) | 32 |
| Timestamp unit | Column units (seconds for `DateTime`) | Microseconds; `(?t)` thresholds in seconds |

The `sessionize` function has no direct ClickHouse equivalent. ClickHouse
provides session analysis through different mechanisms.

## How Parity Was Verified

Each function was run in both engines on the same randomly generated event
groups (whole-second timestamps, frequent ties, events matching zero, one or
several conditions, every mode combination) and the results compared group by
group. ClickHouse ran as `clickhouse local` 26.9.8.3; every difference was
minimized and explained from ClickHouse's source at tag `v26.9.8.3-stable`.

| Function | Cases | Outcome |
|---|---|---|
| `retention` | 102,500 groups | Identical |
| `window_funnel` (24 mode combinations, 3 windows each) | 1,296,000 (two seeds) | Equal in every case to an exhaustive reference that follows ClickHouse's algorithm but keeps every chain and orders ties canonically. ClickHouse equals that reference for 17 combinations; the other 7 differ only by differences 1 and 2 |
| `sequence_match` / `sequence_count` | 14,857 groups with time constraints only after `(?N)` or `.`, or none | Identical apart from 4 `sequence_count` cases where ClickHouse's answer changed between runs of the same rows (difference 6) |
| `sequence_match_events` | same | Identical apart from difference 5 |
| `sequence_next_node` (6 valid direction/base pairs, 1 to 4 conditions) | 240,000 with distinct (timestamp, value) per event | Identical. With ties, identical when the rows reach ClickHouse in the extension's tie order |

## Semantic Compatibility

### retention

Compatible. `result[0]` reflects the anchor condition; `result[i]` requires
both the anchor and condition `i` to have been true in the group.

### window_funnel

`window_funnel` runs ClickHouse's `windowFunnel` scan
(`AggregateFunctionWindowFunnel.cpp`): each event contributes one entry per
condition it satisfies (so one event can fill several steps, the entry step
included), entries are visited in `(timestamp, condition)` order, and per
funnel level the chain with the latest entry is kept. The modes follow
ClickHouse:

- **strict_deduplication** (also `'strict'`, which ClickHouse 26.9 rejects):
  a condition firing again for a step already reached stops the scan.
- **strict_order**: once a chain has been entered, an event matching no
  condition, or a step arriving before its predecessor, stops the scan.
- **strict_increase**: each step must be strictly later than the one before.
- **strict_once**: an event fills at most one step of a chain.
- **allow_reentry**: requires `strict_order` (an error otherwise, as in
  ClickHouse); a step arriving before its predecessor is skipped instead of
  stopping the scan.
- **timestamp_dedup** _(extension)_: same as `strict_increase`.

### sequence_match, sequence_count, sequence_match_events

Pattern syntax matches ClickHouse: `(?N)` conditions (1-indexed, at most the
number of conditions passed), `.`, `.*`, and `(?t op N)` with `op` one of
`>=`, `<=`, `>`, `<`, `==`, plus the extension operator `!=`. A time
constraint gates the next step without consuming events, and non-matching
events between gated steps are skipped, as in ClickHouse. `sequence_count`
counts non-overlapping matches; a match that consumes no events (for example
`.*`) advances one event, as in ClickHouse.

### sequence_next_node

Matches ClickHouse's `sequenceNextNode`: a single anchor per base
(`head`/`tail` = the literal first/last event, which must satisfy
`base_condition`; `first_match`/`last_match` = the first/last event satisfying
`base_condition` and `event1`), the chain must match consecutive events, and a
failed chain is not retried at another anchor. `forward` with `tail` and
`backward` with `head` are rejected, as in ClickHouse.

## Extensions Beyond ClickHouse

| Function/Feature | Description |
|---|---|
| `sessionize` | Session IDs over an ordered window (no ClickHouse equivalent) |
| `window_funnel_events` | The step timestamps of the chain `window_funnel` reports, as `LIST(TIMESTAMP)` |
| `'timestamp_dedup'` mode | Alias of `strict_increase` |
| `(?t!=N)` time constraint | Not-equal operator in sequence patterns |
| No experimental flags | `sequence_next_node` works without `SET allow_experimental_funnel_functions = 1` |

## Known Semantic Differences

Deliberate, because ClickHouse's behaviour is defective or depends on row
order:

1. **`windowFunnel` with `strict_increase` (without `strict_once`) loses
   chains in ClickHouse.** It keeps one chain per level, and a later entry
   overwrites a valid earlier chain that it then cannot use under the strict
   comparison: events `c1@0, c1@1, c2@1` give 1, although `c1@0 -> c2@1` is a
   valid chain. ClickHouse's own `strict_once` path, which keeps every chain,
   gives 2. The extension keeps the best chain ending before each timestamp
   and gives 2. Combined with `strict_deduplication`, the kept chain can make
   a later repeat count, so the extension's answer can also be lower.

2. **Ties that ClickHouse leaves in arrival order.** `windowFunnel` with
   `strict_once` orders same-timestamp rows by arrival, and with
   `strict_deduplication` its answer then depends on row order (rows
   `c2@2, c1&c2&c3@2, c3@3` give 2 or 3). `sequenceNextNode` keeps events tying
   on `(timestamp, value)` in arrival order. The extension orders ties by a
   total key (the condition bitmask; for `sequence_next_node`,
   `(timestamp, value, base_condition, conditions)`), so results never depend
   on row order or thread count. Every differing `windowFunnel` case checked
   (547) is an answer ClickHouse itself returns for some ordering of the rows.

3. **`sequenceMatch` and friends also order same-timestamp events by
   arrival** (`::sort` on timestamp only); the extension orders them by
   condition bitmask.

4. **Time-constraint anchor.** The extension measures `(?t op N)` from the
   event consumed by the last `(?N)` or `.` step. ClickHouse resets the anchor
   at `.*` to the event after the last match, so `(?1).*(?t>0)(?2)` can never
   match there and `(?t<=N)` directly after `.*` is vacuously true. A
   constraint before any `(?N)` or `.` is an error in the extension;
   ClickHouse measures it from wherever its implicit leading `.*` is.

5. **`sequenceMatchEvents` reports an abandoned attempt.** ClickHouse replaces
   its best chain only when a strictly longer one appears, so after a failed
   attempt it can report that attempt's timestamps for a later successful
   match of the same length (`(?1)(?t==2)` over `c1@0, c1@3, c2@5` reports
   `[0]`). The extension reports the events that matched (`[3]`).

6. **ClickHouse reads past its action list** for a pattern ending in `.*`,
   `(?t<…)`, `(?t<=…)` or `(?t>=0)` when the match consumes the last event
   (`AggregateFunctionSequenceMatch.cpp`, the trailing-skip loop has no end
   check). Its answer for such a group changed between runs; the extension's
   does not.

Other differences:

7. **NULL inputs.** ClickHouse skips a row with any NULL argument. The
   extension treats a NULL condition as false; a NULL timestamp skips the row;
   `sequence_next_node` keeps NULL values as events and can return NULL as the
   next value.

8. **Units.** Windows are DuckDB `INTERVAL`s (month-based ones are rejected);
   timestamps are microseconds and `(?t op N)` thresholds are seconds, with
   the elapsed time floored to whole seconds (`(?t==N)` means `[N, N+1)`).
   Gaps touching DuckDB's `±infinity` timestamps are computed exactly.

9. **Accepted input.** The extension accepts whitespace between pattern
   elements, a single `'strict'` mode string, and two time constraints in a
   row after an event; ClickHouse rejects all three. ClickHouse accepts an
   empty pattern and thresholds above `i64::MAX`; the extension rejects both.

10. **Empty input.** Over zero rows `retention` returns `[]` and
    `sequence_match` / `sequence_count` return NULL; ClickHouse returns zeros.

11. **`sequenceNextNode` condition limit.** ClickHouse allows 64 event
    conditions; the extension 32.
