# sessionize

Window function that assigns monotonically increasing session IDs. A new session
begins when the gap between consecutive events exceeds a configurable threshold.

## Signature

```
sessionize(timestamp TIMESTAMP, gap INTERVAL) -> BIGINT
```

**Parameters:**

| Parameter | Type | Description |
|---|---|---|
| `timestamp` | `TIMESTAMP` or `TIMESTAMPTZ` | Event timestamp (see [Timestamp types](#timestamp-types)) |
| `gap` | `INTERVAL` | Maximum allowed inactivity gap between events in the same session |

**Returns:** `BIGINT` -- the session ID (1-indexed, monotonically increasing within each partition).

## Usage

`sessionize` is an aggregate designed to be used as a window function with
`OVER (PARTITION BY ... ORDER BY timestamp)`. Order the window by the
timestamp, ascending, and keep the default frame (or any frame ending at
`CURRENT ROW`). Do not use `OVER ()` or a `BETWEEN UNBOUNDED PRECEDING AND
UNBOUNDED FOLLOWING` frame: DuckDB crashes on those for every C API aggregate
([FAQ](../faq.md#which-query-shapes-crash-duckdb)). Without `OVER`, it returns
the number of sessions in the group.

```sql
SELECT user_id, event_time,
  sessionize(event_time, INTERVAL '30 minutes') OVER (
    PARTITION BY user_id ORDER BY event_time
  ) as session_id
FROM events;
```

## Behavior

- The first event in each partition is assigned session ID 1.
- Each subsequent event is compared to the previous event's timestamp.
- If the gap exceeds the threshold, the session ID increments.
- A gap exactly equal to the threshold does **not** start a new session; the gap
  must strictly exceed the threshold.
- The threshold may differ per row: each row is compared with its own gap.
- Gaps touching DuckDB's `±infinity` timestamps are computed exactly.
- With a descending `ORDER BY`, gaps are never positive and every row starts
  a new session; order ascending.

### Example

Given events for a single user with a 30-minute threshold:

| event_time | session_id | Reason |
|---|---|---|
| 10:00 | 1 | First event |
| 10:15 | 1 | 15 min gap (within threshold) |
| 10:25 | 1 | 10 min gap (within threshold) |
| 11:30 | 2 | 65 min gap (exceeds threshold) |
| 11:45 | 2 | 15 min gap (within threshold) |
| 13:00 | 3 | 75 min gap (exceeds threshold) |

## Errors

Invalid gap intervals abort the query with a descriptive SQL error instead of
silently sessionizing with a zero threshold:

- **Month-based gap** — month intervals are ambiguous (28-31 days); use
  day/hour/minute/second units (e.g. `INTERVAL '30 minutes'`)
- **Negative gap** — the gap must be non-negative

With a frame ending at the current row (the default), a `NULL` timestamp
produces a `NULL` session ID for that row. The rule follows the frame's last
row, so with a frame that ends elsewhere (`... AND 1 FOLLOWING`) it applies to
that row instead. A row whose gap is `NULL` is left out of the session chain
and receives the current session ID (`NULL` if no earlier row counted).


### Timestamp types

`TIMESTAMP` and `TIMESTAMPTZ` are both accepted and read as microseconds since
the epoch; for `TIMESTAMPTZ` that is the instant itself, independent of the
session time zone. Casting `TIMESTAMPTZ` to `TIMESTAMP` instead converts to
local time, which can reorder events around a daylight-saving change.
`TIMESTAMP_S`, `TIMESTAMP_MS` and `DATE` are cast to `TIMESTAMP` implicitly.
`TIMESTAMP_NS` is too, which truncates to microseconds: events less than a
microsecond apart become ties.

## Implementation

The state tracks the first timestamp, last timestamp, and the number of session
boundaries (gaps exceeding the threshold). The `combine` operation is O(1),
which enables efficient evaluation via DuckDB's segment tree windowing machinery.

| Operation | Complexity |
|---|---|
| Update | O(1) |
| Combine | O(1) |
| Finalize | O(1) |
| Space | O(1) per partition segment |

At benchmark scale, `sessionize` processes **1 billion rows in 1.20 seconds**
(830 Melem/s).

## See Also

- [`retention`](./retention.md) -- cohort retention analysis using boolean conditions
- [`window_funnel`](./window-funnel.md) -- conversion funnel step tracking within time windows
