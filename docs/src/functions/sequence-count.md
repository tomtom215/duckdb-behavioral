# sequence_count

Aggregate function that counts the number of non-overlapping occurrences of a
pattern in the event stream.

## Signature

```
sequence_count(pattern VARCHAR, timestamp TIMESTAMP,
               cond1 BOOLEAN, cond2 BOOLEAN [, ...]) -> BIGINT
```

**Parameters:**

| Parameter | Type | Description |
|---|---|---|
| `pattern` | `VARCHAR` | Pattern string (same syntax as `sequence_match`) |
| `timestamp` | `TIMESTAMP` | Event timestamp |
| `cond1..condN` | `BOOLEAN` | Event conditions (2 to 32) |

**Returns:** `BIGINT` -- the number of non-overlapping matches of the pattern
in the event stream.

## Usage

```sql
-- Count how many times a user viewed then purchased
SELECT user_id,
  sequence_count('(?1)(?2)', event_time,
    event_type = 'view',
    event_type = 'purchase'
  ) as conversion_count
FROM events
GROUP BY user_id;
```

## Behavior

1. Events are sorted by timestamp.
2. The pattern is compiled and executed using the NFA engine.
3. Each time the pattern matches, the count increments and the NFA restarts
   from the event following the last matched event.
4. Matches are non-overlapping: once a set of events is consumed by a match,
   those events cannot participate in another match.

### Example

Given events for a user with pattern `(?1)(?2)`:

| event_time | cond1 (view) | cond2 (purchase) |
|---|---|---|
| 10:00 | true | false |
| 10:10 | false | true |
| 10:20 | true | false |
| 10:30 | false | true |
| 10:40 | true | false |

Result: `2`

- First match: events at 10:00 and 10:10.
- Second match: events at 10:20 and 10:30.
- The event at 10:40 has no subsequent `cond2` event.

## Pattern Syntax

Uses the same pattern syntax as [`sequence_match`](./sequence-match.md). Refer
to that page for the full syntax reference.

## Implementation

| Operation | Complexity |
|---|---|
| Update | O(1) amortized (event append) |
| Combine | O(m) where m = events in other state |
| Finalize | O(n * s) NFA execution, where n = events, s = pattern steps |
| Space | O(n) -- all collected events |

At benchmark scale, `sequence_count` processes **100 million events in 1.05 s**
(95 Melem/s).

## See Also

- [`sequence_match`](./sequence-match.md) -- check whether the pattern matches (boolean)
- [`sequence_match_events`](./sequence-match-events.md) -- return the timestamps of each matched step
- [`sequence_next_node`](./sequence-next-node.md) -- find the next event value after a pattern match
