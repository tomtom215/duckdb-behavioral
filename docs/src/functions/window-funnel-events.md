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
| `timestamp` | `TIMESTAMP` or `TIMESTAMPTZ` | Event timestamp (see [Timestamp types](#timestamp-types)) |
| `cond1..condN` | `BOOLEAN` | Funnel step conditions (1 to 32) |

**Returns:** `TIMESTAMP[]` (`TIMESTAMPTZ[]` for `TIMESTAMPTZ` input) -- one timestamp per matched funnel step, in match
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

- Same scan as `window_funnel` (ClickHouse's `windowFunnel` algorithm), all
  [modes](./window-funnel.md#modes) supported.
- Returns the chain that reached the most steps. Among chains reaching that
  many, the one with the latest entry is returned (the chain the scan keeps
  per step; on a tie, the one completed last).
- An event that fills several steps contributes its timestamp once per step,
  so the list length always equals the step count.

## Errors

Shares `window_funnel`'s validation: unknown mode strings, month-based or
negative windows, and `allow_reentry` without `strict_order` abort the query
with a descriptive SQL error. A row whose window is `NULL` is skipped; a
`NULL` mode means no mode.

The `window` and `mode` arguments must be the same for every row of a group
(normally a literal). A group with two different non-`NULL` values is an
error, whatever the row order (`... the window argument must be the same for
every row of a group`). `NULL` values are ignored.

### Timestamp types

`TIMESTAMP` and `TIMESTAMPTZ` are both accepted and read as microseconds since
the epoch; for `TIMESTAMPTZ` that is the instant itself, independent of the
session time zone. Casting `TIMESTAMPTZ` to `TIMESTAMP` instead converts to
local time, which can reorder events around a daylight-saving change.
`TIMESTAMP_S`, `TIMESTAMP_MS` and `DATE` are cast to `TIMESTAMP` implicitly.
`TIMESTAMP_NS` is too, which truncates to microseconds: events less than a
microsecond apart become ties.

## Implementation

Shares `WindowFunnelState` and the update/combine FFI callbacks with
`window_funnel` — only finalize differs. The scan is generic over how a chain
stores its step timestamps: `window_funnel` stores none, `window_funnel_events`
stores one timestamp per step.

This function is an extension beyond ClickHouse: `windowFunnel` has no
timestamp-returning companion there.

## See Also

- [window_funnel](./window-funnel.md) — step counts and mode semantics
- [sequence_match_events](./sequence-match-events.md) — matched timestamps for
  sequence patterns
