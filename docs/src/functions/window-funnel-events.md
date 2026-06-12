# window_funnel_events

Aggregate function returning the timestamps of the best conversion funnel
chain. Companion to [`window_funnel`](./window-funnel.md): where
`window_funnel` tells you *how far* users got, `window_funnel_events` tells
you *when* each step happened — invaluable for debugging funnels and
computing step-to-step latencies.

## Signature

```
window_funnel_events(window INTERVAL, timestamp TIMESTAMP,
                     cond1 BOOLEAN, cond2 BOOLEAN [, ...]) -> TIMESTAMP[]

window_funnel_events(window INTERVAL, mode VARCHAR, timestamp TIMESTAMP,
                     cond1 BOOLEAN, cond2 BOOLEAN [, ...]) -> TIMESTAMP[]
```

**Parameters:**

| Parameter | Type | Description |
|---|---|---|
| `window` | `INTERVAL` | Maximum time window from the first step |
| `mode` | `VARCHAR` | Optional comma-separated mode string |
| `timestamp` | `TIMESTAMP` | Event timestamp |
| `cond1..condN` | `BOOLEAN` | Funnel step conditions (2 to 32) |

**Returns:** `TIMESTAMP[]` -- one timestamp per matched funnel step, in match
order. The list length always equals `window_funnel`'s return value for the
same arguments. Empty list when the entry condition never matched.

## Usage

```sql
SELECT user_id,
  window_funnel_events(INTERVAL '1 hour', event_time,
    event_type = 'page_view',
    event_type = 'add_to_cart',
    event_type = 'purchase'
  ) as funnel_chain
FROM events
GROUP BY user_id;
```

### Step-to-Step Latency

```sql
WITH chains AS (
  SELECT user_id,
    window_funnel_events(INTERVAL '1 hour', event_time,
      event_type = 'page_view',
      event_type = 'add_to_cart',
      event_type = 'purchase'
    ) as chain
  FROM events
  GROUP BY user_id
)
SELECT user_id,
  chain[2] - chain[1] AS view_to_cart,
  chain[3] - chain[2] AS cart_to_purchase
FROM chains
WHERE len(chain) = 3;
```

## Behavior

- Identical event selection to `window_funnel`: greedy forward scan from each
  entry-condition event, all six [modes](./window-funnel.md#modes) supported.
- The **best chain** wins: the chain reaching the highest step. Among chains
  reaching the same maximum, the earliest entry wins (mirroring
  `window_funnel`'s greedy scan order).
- An event that satisfies several consecutive conditions advances multiple
  steps and contributes its timestamp once per step, so the list length always
  equals the step count.
- With `allow_reentry`, a chain reset starts recording from the new entry
  event.

## Errors

Shares `window_funnel`'s validation: unknown mode strings, month-based or
negative windows abort the query with a descriptive SQL error. A `NULL`
window or mode is skipped leniently.

## Implementation

Shares `WindowFunnelState` and the update/combine FFI callbacks with
`window_funnel` — only finalize differs. The greedy scan is generic over a
zero-sized step recorder, so `window_funnel`'s hot path compiles to identical
code while `window_funnel_events` records the winning chain into a reusable
scratch buffer (no per-candidate allocation).

This function is an extension beyond ClickHouse: `windowFunnel` has no
timestamp-returning companion there.

## See Also

- [window_funnel](./window-funnel.md) — step counts and mode semantics
- [sequence_match_events](./sequence-match-events.md) — matched timestamps for
  sequence patterns
