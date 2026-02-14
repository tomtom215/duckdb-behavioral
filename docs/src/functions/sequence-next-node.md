# sequence_next_node

Aggregate function that returns the value of the next event after a matched
sequential pattern. Implements ClickHouse's `sequenceNextNode` for flow analysis.

## Signature

```
sequence_next_node(direction VARCHAR, base VARCHAR, timestamp TIMESTAMP,
                   event_column VARCHAR, base_condition BOOLEAN,
                   event1 BOOLEAN [, event2 BOOLEAN, ...]) -> VARCHAR
```

**Parameters:**

| Parameter | Type | Description |
|---|---|---|
| `direction` | `VARCHAR` | `'forward'` or `'backward'` |
| `base` | `VARCHAR` | `'head'`, `'tail'`, `'first_match'`, or `'last_match'` |
| `timestamp` | `TIMESTAMP` | Event timestamp |
| `event_column` | `VARCHAR` | Value column (returned as result) |
| `base_condition` | `BOOLEAN` | Condition for the base/anchor event |
| `event1..eventN` | `BOOLEAN` | Sequential event conditions (1 to 32) |

**Returns:** `VARCHAR` (nullable) -- the value of the adjacent event after a
successful sequential match, or `NULL` if no match or no adjacent event exists.

## Direction

Controls which direction to scan for the next event:

| Direction | Behavior |
|---|---|
| `'forward'` | Match events earliest-to-latest, return the event **after** the last matched step |
| `'backward'` | Match events latest-to-earliest, return the event **before** the earliest matched step |

## Base

Controls which starting point to use when multiple matches exist:

| Base | Forward behavior | Backward behavior |
|---|---|---|
| `'head'` | Start from the first `base_condition` event | Start from the first `base_condition` event |
| `'tail'` | Start from the last `base_condition` event | Start from the last `base_condition` event |
| `'first_match'` | Return the first complete match result | Return the first complete match result (scanning right-to-left) |
| `'last_match'` | Return the last complete match result | Return the last complete match result (scanning right-to-left) |

## Usage

```sql
-- What page do users visit after Home → Product?
SELECT user_id,
  sequence_next_node('forward', 'first_match', event_time, page,
    page = 'Home',        -- base_condition
    page = 'Home',        -- event1
    page = 'Product'      -- event2
  ) as next_page
FROM events
GROUP BY user_id;

-- What page did users come from before reaching Checkout?
SELECT user_id,
  sequence_next_node('backward', 'tail', event_time, page,
    page = 'Checkout',    -- base_condition
    page = 'Checkout'     -- event1
  ) as previous_page
FROM events
GROUP BY user_id;

-- Flow analysis: what happens after the first Home → Product → Cart sequence?
SELECT user_id,
  sequence_next_node('forward', 'first_match', event_time, page,
    page = 'Home',        -- base_condition
    page = 'Home',        -- event1
    page = 'Product',     -- event2
    page = 'Cart'         -- event3
  ) as next_after_cart
FROM events
GROUP BY user_id;
```

## Behavior

1. Events are sorted by timestamp.
2. The function scans in the specified direction to find a sequential chain
   of events matching `event1`, `event2`, ..., `eventN`.
3. The starting event must satisfy `base_condition`.
4. For **forward**: returns the value of the event immediately after the last
   matched step.
5. For **backward**: event1 matches at the starting position (later timestamp),
   then event2 matches at an earlier position, etc. Returns the value of the
   event immediately before the earliest matched step.
6. Returns `NULL` if no complete match is found, or if no adjacent event exists.

## Differences from ClickHouse

| Aspect | ClickHouse | duckdb-behavioral |
|---|---|---|
| Syntax | `sequenceNextNode(direction, base)(ts, val, base_cond, ev1, ...)` | `sequence_next_node(direction, base, ts, val, base_cond, ev1, ...)` |
| Function name | camelCase | snake_case |
| Parameters | Two-level call syntax | Flat parameter list |
| Return type | `Nullable(String)` | `VARCHAR` (nullable) |
| Experimental flag | Requires `allow_experimental_funnel_functions = 1` | Always available |

## Implementation

| Operation | Complexity |
|---|---|
| Update | O(1) amortized (event append) |
| Combine | O(m) where m = events in other state |
| Finalize | O(n * k) sequential scan, where n = events, k = event conditions |
| Space | O(n) -- all events stored (each includes an `Arc<str>` value) |

Note: Unlike other event-collecting functions where the `Event` struct is `Copy`
(16 bytes), `sequence_next_node` uses a dedicated `NextNodeEvent` struct (32 bytes)
that stores an `Arc<str>` value per event. The `Arc<str>` enables O(1) clone via
reference counting, which significantly reduces combine overhead compared to
per-event `String` cloning.

## See Also

- [`sequence_match`](./sequence-match.md) -- check whether a pattern matches (boolean)
- [`sequence_count`](./sequence-count.md) -- count non-overlapping matches of a pattern
- [`sequence_match_events`](./sequence-match-events.md) -- return the timestamps of each matched step
