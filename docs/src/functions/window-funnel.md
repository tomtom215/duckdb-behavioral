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
| `cond1..condN` | `BOOLEAN` | Funnel step conditions (1 to 32) |

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

`window_funnel` follows ClickHouse's `windowFunnel`:

1. Each event contributes one entry per condition it satisfies, and entries
   are visited in `(timestamp, condition)` order.
2. An entry for `cond1` starts a chain. An entry for `condK` extends a chain
   that has reached step `K - 1`, provided it is within `window` of that
   chain's **entry** event.
3. The result is the highest step any chain reached.

Because every satisfied condition is an entry, one event can fill several
steps, including the entry step: an event satisfying `cond1` and `cond2`
reaches step 2 on its own (`strict_once` and `strict_increase` prevent this).
A gap exactly equal to `window` is inside the window.

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

### Determinism

Events sort by `(timestamp, conditions)` before the scan, so results are
deterministic regardless of thread count, physical row order, or the order in
which DuckDB's parallel aggregation combines partial states, including
same-timestamp event bursts. (ClickHouse orders same-timestamp rows by arrival
under `strict_once`; see
[ClickHouse Compatibility](../internals/clickhouse-compatibility.md#known-semantic-differences).)

## Modes

Modes combine via a comma-separated string parameter, as in ClickHouse.

| Mode | Description |
|---|---|
| `strict_deduplication` | A condition firing again for a step already reached stops the scan; the result is the step reached so far. `'strict'` is accepted as an alias (ClickHouse 26.9 rejects `'strict'`). |
| `strict_order` | Once a chain has been entered, an event matching no condition, or a step arriving before its predecessor has been reached, stops the scan. A repeated entry or step does not. |
| `strict_increase` | Each step must be strictly later than the step before it, so one event cannot fill two steps. |
| `strict_once` | An event fills at most one step of a chain. |
| `allow_reentry` | Requires `strict_order`. A step arriving before its predecessor is skipped instead of stopping the scan. |
| `timestamp_dedup` | _Extension mode._ Same as `strict_increase`. |

Under `strict_increase` this implementation keeps a valid chain that
ClickHouse loses (events `c1@0, c1@1, c2@1` give 2 here, 1 in ClickHouse); see
[ClickHouse Compatibility](../internals/clickhouse-compatibility.md#known-semantic-differences).

### Mode Combinations

Modes can be combined freely:

```sql
-- Require strictly increasing timestamps and one step per event
window_funnel(INTERVAL '1 hour', 'strict_increase, strict_once',
  ts, cond1, cond2, cond3)

-- Strict order, skipping steps that arrive too early
window_funnel(INTERVAL '1 hour', 'strict_order, allow_reentry',
  ts, cond1, cond2, cond3)
```

## Errors

Invalid configuration aborts the query with a descriptive SQL error instead of
silently producing wrong results:

- **Unknown mode string** — `window_funnel: unknown mode 'strict_typo'; valid
  modes are 'strict', 'strict_deduplication', 'strict_order',
  'strict_increase', 'strict_once', 'allow_reentry', 'timestamp_dedup'
  (comma-separated for combinations)`
- **Month-based window** — month intervals are ambiguous (28-31 days); use
  day/hour/minute/second units (e.g. `INTERVAL '30 days'`)
- **Negative window** — the window must be non-negative
- **`allow_reentry` without `strict_order`** — `window_funnel: mode
  'allow_reentry' requires 'strict_order'`

A row whose window is `NULL` is skipped, like a row with a `NULL` timestamp.
A `NULL` mode means no mode.

## Implementation

Events are collected during the update phase and sorted during finalize. One
pass over the sorted events keeps, per funnel step, the chain with the latest
entry (and, for `strict_increase`, separately the best chain ending before the
current timestamp). Under `strict_once`, whether a chain can use distinct
same-timestamp events for consecutive steps is decided by bipartite matching
(at most 32 steps), so a large same-timestamp burst stays polynomial.

| Operation | Complexity |
|---|---|
| Update | O(1) amortized (event append) |
| Combine | O(m) where m = events in other state |
| Finalize | O(n log n) sort + O(n * k) scan, n = events, k = conditions |
| Space | O(n) -- all collected events |

The last recorded benchmark (PERF.md Session 15, the previous engine) is 100
million events in 791 ms; it has not been re-measured for this engine.

## See Also

- [`sequence_match`](./sequence-match.md) -- pattern matching for more flexible event sequences
- [`sequence_count`](./sequence-count.md) -- count non-overlapping pattern occurrences
- [`sequence_next_node`](./sequence-next-node.md) -- find what happens after a matched pattern
- [ClickHouse Compatibility](../internals/clickhouse-compatibility.md) -- full compatibility matrix including all mode mappings
