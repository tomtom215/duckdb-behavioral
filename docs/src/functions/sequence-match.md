# sequence_match

Aggregate function that checks whether a sequence of events matches a pattern.
Uses a mini-regex syntax over condition references.

## Signature

```
sequence_match(pattern VARCHAR, timestamp TIMESTAMP,
               cond1 BOOLEAN, cond2 BOOLEAN [, ...]) -> BOOLEAN
```

**Parameters:**

| Parameter | Type | Description |
|---|---|---|
| `pattern` | `VARCHAR` | Pattern string using the syntax described below |
| `timestamp` | `TIMESTAMP` | Event timestamp |
| `cond1..condN` | `BOOLEAN` | Event conditions (2 to 32) |

**Returns:** `BOOLEAN` -- `true` if the event stream contains a subsequence
matching the pattern, `false` otherwise.

## Usage

```sql
-- Did the user view a product and then purchase?
SELECT user_id,
  sequence_match('(?1).*(?2)', event_time,
    event_type = 'view',
    event_type = 'purchase'
  ) as converted
FROM events
GROUP BY user_id;
```

## Pattern Syntax

Patterns are composed of the following elements:

| Pattern | Description |
|---|---|
| `(?N)` | Match an event where condition N (1-indexed, at most the number of conditions passed) is true |
| `.` | Match exactly one event (any conditions) |
| `.*` | Match zero or more events (any conditions) |
| `(?t>=N)` | Time constraint: at least N seconds since the last `(?N)` or `.` event |
| `(?t<=N)` | Time constraint: at most N seconds since the last `(?N)` or `.` event |
| `(?t>N)` | Time constraint: more than N seconds since the last `(?N)` or `.` event |
| `(?t<N)` | Time constraint: less than N seconds since the last `(?N)` or `.` event |
| `(?t==N)` | Time constraint: exactly N seconds since the last `(?N)` or `.` event |
| `(?t!=N)` | Time constraint: not exactly N seconds since the last `(?N)` or `.` event |

### Pattern Examples

```sql
-- View then purchase (any events in between)
'(?1).*(?2)'

-- View then purchase with no intervening events
'(?1)(?2)'

-- View, exactly one event, then purchase
'(?1).(?2)'

-- View, then purchase within 1 hour
'(?1).*(?t<=3600)(?2)'

-- Three-step sequence with time constraints
'(?1).*(?t<=3600)(?2).*(?t<=7200)(?3)'
```

## Behavior

1. Events are sorted by timestamp.
2. The pattern is compiled into a list of steps.
3. Returns `true` if the pattern matches starting at any event. `.*` matches
   as few events as possible (lazy), which decides *which* match
   `sequence_count` and `sequence_match_events` report.

Time constraints are evaluated relative to the timestamp of the event consumed
by the last `(?N)` or `.` step, in seconds. A constraint before any such step
has nothing to measure from and is an error. ClickHouse instead measures a
constraint that follows `.*` from the event after the last match, so
`(?1).*(?t<=3600)(?2)` means "`(?2)` within an hour of `(?1)`" here but not
there; see
[ClickHouse Compatibility](../internals/clickhouse-compatibility.md#known-semantic-differences).

### Time-Constraint Semantics

`(?t op N)` mirrors ClickHouse (verified against its implementation): the
constraint gates the next pattern step, and non-matching events in between
are skipped while the gate can still hold — `(?1)(?t<=10)(?2)` matches `(?2)`
at any event within 10 seconds of `(?1)`. Trailing `(?t<=N)`, `(?t<N)` and
`(?t>=0)` match the empty remainder. `N` is interpreted in seconds with the
elapsed time floored to whole seconds, so `(?t==N)` means "within `[N, N+1)`
seconds" (the faithful generalization of ClickHouse's whole-second `DateTime`
comparison to microsecond timestamps).

### Determinism

Events sort by `(timestamp, conditions)` before matching, so results are
deterministic regardless of thread count or physical row order. Events that
satisfy no condition are dropped before matching (as in ClickHouse), so `.`
and `.*` never consume them and they do not break adjacency.

## Errors

A malformed pattern aborts the query with the parser's position-annotated
message instead of silently returning `NULL`:

```text
invalid sequence pattern '(?1)(?': pattern error at position 6: ...
```

So do a condition number above the number of conditions passed
(`condition (?3) is out of range; 2 conditions were passed`) and a time
constraint with no `(?N)` or `.` before it (`time constraint must follow an
event condition`).

A `NULL` pattern yields a `NULL` result (lenient), matching SQL aggregate
conventions.

## Implementation

| Operation | Complexity |
|---|---|
| Update | O(1) amortized (event append) |
| Combine | O(m) where m = events in other state |
| Finalize | O(n log n) sort; then O(n) for patterns of conditions and `.*` / adjacent conditions only (fast paths), otherwise O(s · n log n) for s pattern steps |
| Space | O(n) -- all collected events |

The last recorded benchmark (PERF.md Session 15) is 100 million events in
1.05 s for the fast-path pattern `(?1).*(?2).*(?3)`.

Patterns outside the fast paths are matched in two passes: a backward pass
marks, for each `(?N)` or `.` step, the events from which the rest of the
pattern can still complete; a forward walk then takes the earliest such event
at each step. This returns the same match as a lazy backtracking search
would find first, without the search's quadratic worst case. Through v0.9.1
the extension used that search, and a group that did not match could take
seconds (8.3–9.1 s for `(?1).*(?t<5)(?2).*(?3)` at 32,000 events). It now
takes 0.004 s, and 0.78–0.89 s at 10 million events in one group (DuckDB
1.5.6, 3 runs each). Finalize uses about 8 bytes per event of working
memory, plus one byte per event for each `(?N)` or `.` step.

## See Also

- [`sequence_count`](./sequence-count.md) -- count non-overlapping matches of the same pattern
- [`sequence_match_events`](./sequence-match-events.md) -- return the timestamps of each matched step
- [`sequence_next_node`](./sequence-next-node.md) -- find the next event value after a pattern match
