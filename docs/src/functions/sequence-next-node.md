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

Selects the single anchor event the chain starts from:

| Base | Anchor | Valid with |
|---|---|---|
| `'head'` | The first event; it must satisfy `base_condition` | `forward` only |
| `'tail'` | The last event; it must satisfy `base_condition` | `backward` only |
| `'first_match'` | The first event satisfying `base_condition` and `event1` | both |
| `'last_match'` | The last event satisfying `base_condition` and `event1` | both |

If the chain does not match from that anchor, the result is `NULL`; other
anchors are not tried.

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

Follows ClickHouse's `sequenceNextNode` (differential testing against
ClickHouse 26.9.8.3 found no difference for events with distinct
`(timestamp, value)`):

1. Events are sorted by `(timestamp, value, base_condition, conditions)`.
   ClickHouse sorts by `(timestamp, value)` and keeps events tying on both in
   arrival order, so its result can change with row order; the extra keys
   make the order total here.
2. A **single anchor** is selected by `base`:
   - `head` / `tail`: the literal first/last event in sorted order, which must
     itself satisfy `base_condition` — otherwise the result is `NULL`.
   - `first_match` / `last_match`: the first/last event satisfying both
     `base_condition` **and** `event1`.
3. The chain must match **consecutive** events: `eventK` must hold at the
   K-th position from the anchor (ascending for `forward`, descending for
   `backward`). Interleaved non-matching events break the chain, and a failed
   chain is **not** retried at other anchors.
4. On a full match, the value of the event immediately after (`forward`) or
   before (`backward`) the chain is returned; `NULL` when that position falls
   off either end.
5. Returns `NULL` if no anchor exists or the chain does not match.

## Differences from ClickHouse

| Aspect | ClickHouse | duckdb-behavioral |
|---|---|---|
| Syntax | `sequenceNextNode(direction, base)(ts, val, base_cond, ev1, ...)` | `sequence_next_node(direction, base, ts, val, base_cond, ev1, ...)` |
| Function name | camelCase | snake_case |
| Parameters | Two-level call syntax | Flat parameter list |
| Return type | `Nullable(String)` | `VARCHAR` (nullable) |
| Experimental flag | Requires `allow_experimental_funnel_functions = 1` | Always available |

## Errors

Unknown configuration values abort the query with a descriptive SQL error
instead of silently returning `NULL`:

- **Unknown direction** — expected `'forward'` or `'backward'`
- **Unknown base** — expected `'head'`, `'tail'`, `'first_match'`, or
  `'last_match'`
- **`forward` with `tail`, `backward` with `head`** — the chain would start at
  an end of the sequence, so there is never an adjacent event (ClickHouse
  rejects these too)

A `NULL` direction is treated as `'forward'` and a `NULL` base as
`'first_match'`.

NULL inputs: a row with a NULL timestamp is skipped; a NULL value is kept as an
event and can be returned as the next value; a NULL `base_condition` or event
condition counts as false. (ClickHouse skips any row with a NULL argument.)

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
per-event deep string copying.

## See Also

- [`sequence_match`](./sequence-match.md) -- check whether a pattern matches (boolean)
- [`sequence_count`](./sequence-count.md) -- count non-overlapping matches of a pattern
- [`sequence_match_events`](./sequence-match-events.md) -- return the timestamps of each matched step
