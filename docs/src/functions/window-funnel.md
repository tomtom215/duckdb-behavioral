# window_funnel

Aggregate function for conversion funnel analysis. Searches for the longest chain
of sequential conditions within a time window. Returns the maximum funnel step
reached.

## Signature

```
window_funnel(window INTERVAL, timestamp TIMESTAMP,
              cond1 BOOLEAN, cond2 BOOLEAN [, ...]) -> INTEGER

window_funnel(window INTERVAL, mode VARCHAR, timestamp TIMESTAMP,
              cond1 BOOLEAN, cond2 BOOLEAN [, ...]) -> INTEGER
```

**Parameters:**

| Parameter | Type | Description |
|---|---|---|
| `window` | `INTERVAL` | Maximum time window from the first step |
| `mode` | `VARCHAR` | Optional comma-separated mode string |
| `timestamp` | `TIMESTAMP` | Event timestamp |
| `cond1..condN` | `BOOLEAN` | Funnel step conditions (2 to 32) |

**Returns:** `INTEGER` -- the number of matched funnel steps (0 to N). A return
value of 0 means the entry condition was never satisfied.

## Usage

### Default Mode

```sql
SELECT user_id,
  window_funnel(INTERVAL '1 hour', event_time,
    event_type = 'page_view',
    event_type = 'add_to_cart',
    event_type = 'checkout',
    event_type = 'purchase'
  ) as furthest_step
FROM events
GROUP BY user_id;
```

### With Mode String

```sql
SELECT user_id,
  window_funnel(INTERVAL '1 hour', 'strict_increase, strict_once',
    event_time,
    event_type = 'page_view',
    event_type = 'add_to_cart',
    event_type = 'purchase'
  ) as furthest_step
FROM events
GROUP BY user_id;
```

## Behavior

1. Events are sorted by timestamp.
2. For each event matching the entry condition (`cond1`), a forward scan begins.
3. The scan attempts to match `cond2`, `cond3`, ..., `condN` in order, subject
   to the time window constraint: each subsequent event must occur within
   `window` of the **entry** event.
4. The maximum step reached across all entry points is returned.

A single event can advance multiple funnel steps in default mode. For example,
if an event satisfies both `cond2` and `cond3`, it advances the funnel by two
steps in a single pass.

### Example

Given events for a user with a 1-hour window and 3-step funnel:

| event_time | page_view | add_to_cart | purchase |
|---|---|---|---|
| 10:00 | true | false | false |
| 10:20 | false | true | false |
| 11:30 | false | false | true |

Result: `2`

- Step 1 matched at 10:00 (page_view).
- Step 2 matched at 10:20 (add_to_cart, within 1 hour of 10:00).
- Step 3 at 11:30 is outside the 1-hour window from the entry at 10:00.

## Modes

Modes are independently combinable via a comma-separated string parameter.
Each mode adds an additional constraint on top of the default greedy scan.

| Mode | Description |
|---|---|
| `strict` | If condition `i` fires, condition `i-1` must not fire again before condition `i+1`. Prevents backwards movement. |
| `strict_order` | Events must satisfy conditions in exact sequential order. No events matching earlier conditions are allowed between matched steps. |
| `strict_deduplication` | Events with the same timestamp as the previously matched step are skipped for the next step. |
| `strict_increase` | Requires strictly increasing timestamps between consecutive matched steps. Same-timestamp events cannot advance the funnel. |
| `strict_once` | Each event can advance the funnel by at most one step, even if it satisfies multiple consecutive conditions. |
| `allow_reentry` | If the entry condition fires again after step 1, the funnel resets from that new entry point. |

### Mode Combinations

Modes can be combined freely:

```sql
-- Require strictly increasing timestamps and one step per event
window_funnel(INTERVAL '1 hour', 'strict_increase, strict_once',
  ts, cond1, cond2, cond3)

-- Strict order with reentry
window_funnel(INTERVAL '1 hour', 'strict_order, allow_reentry',
  ts, cond1, cond2, cond3)
```

## Implementation

Events are collected during the update phase and sorted by timestamp during
finalize. A greedy forward scan from each entry point finds the longest chain.

| Operation | Complexity |
|---|---|
| Update | O(1) amortized (event append) |
| Combine | O(m) where m = events in other state |
| Finalize | O(n * k) where n = events, k = conditions |
| Space | O(n) -- all collected events |

At benchmark scale, `window_funnel` processes **100 million events in 715 ms**
(140 Melem/s).

## See Also

- [ClickHouse Compatibility](../internals/clickhouse-compatibility.md) -- full compatibility matrix including all mode mappings
- [`sequence_match`](./sequence-match.md) -- pattern matching with NFA for more flexible event sequences
