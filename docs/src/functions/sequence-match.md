# sequence_match

Aggregate function that checks whether a sequence of events matches a pattern.
Uses a mini-regex syntax over condition references, executed by an NFA engine.

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
| `(?N)` | Match an event where condition N (1-indexed) is true |
| `.` | Match exactly one event (any conditions) |
| `.*` | Match zero or more events (any conditions) |
| `(?t>=N)` | Time constraint: at least N seconds since previous match |
| `(?t<=N)` | Time constraint: at most N seconds since previous match |
| `(?t>N)` | Time constraint: more than N seconds since previous match |
| `(?t<N)` | Time constraint: less than N seconds since previous match |
| `(?t==N)` | Time constraint: exactly N seconds since previous match |
| `(?t!=N)` | Time constraint: not exactly N seconds since previous match |

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
2. The pattern is compiled into an NFA (nondeterministic finite automaton).
3. The NFA is executed against the event stream using lazy matching semantics
   for `.*` (prefer advancing the pattern over consuming additional events).
4. Returns `true` if any accepting state is reached.

Time constraints are evaluated relative to the timestamp of the previously
matched condition step. The time difference is computed in seconds, matching
ClickHouse semantics.

## Implementation

| Operation | Complexity |
|---|---|
| Update | O(1) amortized (event append) |
| Combine | O(m) where m = events in other state |
| Finalize | O(n * s) NFA execution, where n = events, s = pattern steps |
| Space | O(n) -- all collected events |

At benchmark scale, `sequence_match` processes **100 million events in 1.05 s**
(95 Melem/s).

## See Also

- [`sequence_count`](./sequence-count.md) -- count non-overlapping matches of the same pattern
- [`sequence_match_events`](./sequence-match-events.md) -- return the timestamps of each matched step
- [`sequence_next_node`](./sequence-next-node.md) -- find the next event value after a pattern match
