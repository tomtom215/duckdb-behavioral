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
2. The pattern is compiled and matched with the same engine as
   [`sequence_match`](./sequence-match.md).
3. Each time the pattern matches, the count increments and the search resumes
   where the match ended: after the last matched event or, when the pattern
   ends with a time constraint, at the event that satisfied it.
4. Matches are non-overlapping: once a set of events is consumed by a match,
   those events cannot participate in another match. A match that consumes no
   events (for example `.*`, or `(?t<=6).*`) still advances one event, as in
   ClickHouse, so `sequence_count('.*', ...)` counts one match per event.

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

## Errors

A malformed pattern aborts the query with the parser's position-annotated
message instead of silently returning `NULL`:

```text
invalid sequence pattern '(?1)(?': pattern error at position 6: ...
```

An out-of-range condition number and a time constraint with no `(?N)` or `.`
before it are errors too (see [`sequence_match`](./sequence-match.md#errors)).

A `NULL` pattern yields a `NULL` result (lenient), matching SQL aggregate
conventions.

## Implementation

| Operation | Complexity |
|---|---|
| Update | O(1) amortized (event append) |
| Combine | O(m) where m = events in other state |
| Finalize | As [`sequence_match`](./sequence-match.md#implementation) |
| Space | O(n) -- all collected events |

At benchmark scale, `sequence_count` processes **100 million events in 1.18 s**
(85 Melem/s).

## See Also

- [`sequence_match`](./sequence-match.md) -- check whether the pattern matches (boolean)
- [`sequence_match_events`](./sequence-match-events.md) -- return the timestamps of each matched step
- [`sequence_next_node`](./sequence-next-node.md) -- find the next event value after a pattern match
