# sequence_match_events

Aggregate function that returns the timestamps of each matched condition step
in a pattern. This is the diagnostic counterpart to `sequence_match`: instead
of returning a boolean, it returns the actual timestamps where each `(?N)` step
was satisfied.

## Signature

```
sequence_match_events(pattern VARCHAR, timestamp TIMESTAMP,
                      cond1 BOOLEAN, cond2 BOOLEAN [, ...]) -> LIST(TIMESTAMP)
```

**Parameters:**

| Parameter | Type | Description |
|---|---|---|
| `pattern` | `VARCHAR` | Pattern string (same syntax as `sequence_match`) |
| `timestamp` | `TIMESTAMP` | Event timestamp |
| `cond1..condN` | `BOOLEAN` | Event conditions (2 to 32) |

**Returns:** `LIST(TIMESTAMP)` -- a list of timestamps corresponding to each
matched `(?N)` step in the pattern. Returns an empty list if no match is found.

## Usage

```sql
-- Get the timestamps where view and purchase occurred
SELECT user_id,
  sequence_match_events('(?1).*(?2)', event_time,
    event_type = 'view',
    event_type = 'purchase'
  ) as match_timestamps
FROM events
GROUP BY user_id;
-- Returns e.g. [2024-01-01 10:00:00, 2024-01-01 11:30:00]
```

## Behavior

1. Events are sorted by timestamp.
2. The pattern is compiled and executed using a timestamp-collecting variant of
   the NFA engine.
3. When a `(?N)` condition step matches, the event's timestamp is recorded.
4. If the full pattern matches, the collected timestamps are returned as a list.
5. If no match is found, an empty list is returned.

The returned list contains one timestamp per `(?N)` step in the pattern.
Wildcard steps (`.` and `.*`) and time constraint steps do not contribute
timestamps to the output.

### Example

Given events for a user with pattern `(?1).*(?2)`:

| event_time | cond1 (view) | cond2 (purchase) |
|---|---|---|
| 10:00 | true | false |
| 10:15 | false | false |
| 10:30 | false | true |

Result: `[10:00, 10:30]`

The timestamp of the `(?1)` match (10:00) and the `(?2)` match (10:30).

## Pattern Syntax

Uses the same pattern syntax as [`sequence_match`](./sequence-match.md). Refer
to that page for the full syntax reference.

## Implementation

The NFA executor uses a separate `NfaStateWithTimestamps` type that tracks
`collected: Vec<i64>` alongside the standard state index. This keeps the
timestamp collection path separate from the performance-critical
`execute_pattern` and `count_pattern` code paths.

| Operation | Complexity |
|---|---|
| Update | O(1) amortized (event append) |
| Combine | O(m) where m = events in other state |
| Finalize | O(n * s) NFA execution, where n = events, s = pattern steps |
| Space | O(n) -- all collected events |

At benchmark scale, `sequence_match_events` processes **100 million events in 988 ms**
(101 Melem/s).

## See Also

- [`sequence_match`](./sequence-match.md) -- check whether the pattern matches (boolean)
- [`sequence_count`](./sequence-count.md) -- count non-overlapping matches of the same pattern
- [`sequence_next_node`](./sequence-next-node.md) -- find the next event value after a pattern match
