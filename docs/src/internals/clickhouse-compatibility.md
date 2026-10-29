# ClickHouse Compatibility

`duckdb-behavioral` implements behavioral analytics functions inspired by
ClickHouse's
[parametric aggregate functions](https://clickhouse.com/docs/en/sql-reference/aggregate-functions/parametric-functions).
This page documents the compatibility status, semantic differences, and
remaining gaps.

## Compatibility Matrix

### Behavioral Parametric Functions

All six ClickHouse behavioral parametric functions are implemented. "Verified"
means differential testing against ClickHouse 26.10.2.103 found no difference
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
several conditions) and the results compared group by group. ClickHouse ran as
`clickhouse local` 26.10.2.103; `window_funnel` was also run on 26.9.8.3 and
gave identical results.

| Function | Cases | Outcome |
|---|---|---|
| `retention` | 200,000 groups, 2 to 32 conditions | Identical |
| `window_funnel` | 96,000 groups x 24 mode combinations x 3 windows = 6,912,000 results | 70,314 differ, all within 9 of the 24 combinations; the other 15 are identical in every variant. Six combinations use `strict_increase` without `strict_once` (difference 1). Three use `strict_deduplication` with `strict_once` and differ only when tied rows arrive in an order other than the extension's (difference 2): 353 results with shuffled ties, 0 with ties in the extension's order |
| `sequence_next_node` (6 valid direction/base pairs, 1 to 5 conditions) | 100,000 groups = 600,000 results per variant | Identical with distinct timestamps and with ties in the extension's order; with shuffled ties 10,862 differ (difference 2) |
| `sequence_match`, `sequence_count`, `sequence_match_events` | see below | Every result of both engines is predicted by a model of that engine |

### The sequence functions

`scripts/clickhouse-sequence-diff.py` generates 150 random patterns for each
condition count from 2 to 5 and 1,000 random groups of up to 14 events for
each, runs all three functions in both engines, and compares each engine's
answer with a small Python model of that engine. The runs below are its
default output (seed 1).

- **Pattern classes.** A1: time constraints only after `(?N)` or `.`, and the
  pattern does not end in `.*` or in a constraint that is vacuous at the end
  of the events. A2: it does end in one of those. B: a time constraint directly
  after `.*`.
- **Timestamps.** `unique`: distinct within a group. `ties_extorder`: ties,
  with the rows arriving in the extension's order. `ties_shuffled`: ties, rows
  in random order.

| Case | Cells | `sequence_match_events` differs | Any of the three differs |
|---|---|---|---|
| A1, unique | 600,000 | 5,174 | 5,174 |
| A1, ties in extension order | 600,000 | 4,385 | 4,385 |
| A1, shuffled ties | 600,000 | 39,812 | 48,669 |
| A2, unique | 600,000 | 21,372 | 21,372 |
| A2, ties in extension order | 600,000 | 19,823 | 19,823 |
| A2, shuffled ties | 600,000 | 49,680 | 59,673 |
| B, unique | 600,000 | 56,804 | 129,636 |
| B, ties in extension order | 600,000 | 47,995 | 113,920 |

In all eight cases the extension equals its model and ClickHouse equals its
model, for `sequence_match`, `sequence_count` and `sequence_match_events`
(600,000 of 600,000 each, 28.8 million results in all). The two models share a
lazy depth-first search over the events (the search the extension used before
0.10.0); they differ by the rules in differences 3, 4 and 5 below. The
ClickHouse rules were found by running ClickHouse, not from its source, and
fitted on seed 1. The same eight cases with seed 2 (other groups, other
patterns; `--seed 2`) are a hold-out: every one of another 28.8 million results
is predicted too, with different counts of differences (for example 4,033
`sequence_match_events` differences for A1 with unique timestamps). A model
that predicts every result does not prove the rules are ClickHouse's code,
only that nothing in these inputs tells them apart. Not generated: groups of more than 14 events, more than 5 conditions, `!=`,
two constraints in a row, and a constraint before any `(?N)` or `.`.

## Semantic Compatibility

### retention

Compatible. `result[1]` reflects the anchor condition (SQL lists are 1-based); `result[i]` requires
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
| No settings required | `sequence_next_node` works without `SET enable_funnel_functions = 1` (ClickHouse's former name for it, `allow_experimental_funnel_functions`, is still an alias there) |

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
   on row order or thread count. The extension's `window_funnel` equals an
   exhaustive reference (`funnel_matches_exhaustive_reference` in
   `src/oracle_tests.rs`) that follows ClickHouse's algorithm but keeps every
   chain and orders ties canonically.

3. **`sequenceMatch` and friends keep same-timestamp events in arrival
   order**; the extension orders them by condition bitmask. With shuffled ties
   a model that keeps tied rows in arrival order reproduces every ClickHouse
   result (groups of up to 14 events; larger groups were not tested).

4. **Time-constraint anchor.** The extension measures `(?t op N)` from the
   event consumed by the last `(?N)` or `.` step. After `.*`, ClickHouse
   measures it from the event where `.*` stopped consuming, which is the event
   after the last consumed one when `.*` consumes nothing. So
   `(?1).*(?t>0)(?2)` and `(?1).*(?t>=5)(?2)` can never match there (over
   `c1@0, c2@5` the extension matches both), and `(?t<=N)` directly after `.*`
   is vacuously true (`(?1).*(?t<=2)(?2)` matches over `c1@0, c2@5`; the
   extension does not). In ClickHouse neither `.*` nor a skipping constraint
   can absorb the last event, so a pattern ending in `.*(?t<0)` matches only if
   its earlier steps already consumed the last event; the extension also
   matches when `.*` can absorb the rest, because it takes a trailing `(?t<0)`
   as matching the empty rest. A constraint before any `(?N)` or `.` is an error in the extension;
   ClickHouse measures it from wherever its implicit leading `.*` is.

5. **`sequenceMatchEvents` reports the first chain that reached the longest
   length, even if that attempt failed.** ClickHouse replaces its answer only
   when a strictly longer chain appears. An earlier attempt that matched every
   `(?N)` step but then failed a later constraint therefore stays the answer
   when a later attempt completes the pattern, because the later chain is not
   longer. Example: `(?1)(?t==2)` over `c1@0, c1@3, c2@5` reports `[0]`, the
   extension `[3]`. The failed attempt can share its first step with the
   match: `(?2).*(?1)(?t==9)` over `c2&c1@7, c2@8, c2&c1@13, c1@15, c2&c1@21,
   c2@24` reports `[7, 13]` (nothing is 9 s after 13 s), the extension
   `[7, 15]` (24 s is 9 s after 15 s). Deleting the events before the match
   therefore does not remove the failed attempt. A model whose answer is the
   first strictly-longest chain of the search reproduces ClickHouse's events in
   every case above; the extension returns the chain that matched (pinned by
   `test_events_report_the_completing_chain_not_an_abandoned_attempt`).

6. **ClickHouse may read past its action list** for a pattern ending in `.*`,
   `(?t<…)`, `(?t<=…)` or `(?t>=0)` when the match consumes the last event
   (its loop that skips trailing items has no end check). On 26.9.8.3,
   identical input gave different `sequenceMatch` and `sequenceCount` answers
   between runs: 308 and 144 of 1,800,000 results in four runs of the A2 cases.
   On 26.10.2.103 none of 1,800,000 results varied in four runs, and the model
   predicts every A2 result; whether the fault was fixed or does not show in
   this setup is not known. The extension's answer does not depend on memory
   past the pattern.

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
    Over rows that carry no true condition, `sequence_match('.*', ...)` is
    true and `sequence_count` is 0, as in ClickHouse; a pattern with a `(?N)`
    step is false.

11. **`sequenceNextNode` condition limit.** ClickHouse allows 64 event
    conditions; the extension 32.
