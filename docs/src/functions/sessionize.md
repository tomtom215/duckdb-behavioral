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
| `timestamp` | `TIMESTAMP` | Event timestamp |
| `gap` | `INTERVAL` | Maximum allowed inactivity gap between events in the same session |

**Returns:** `BIGINT` -- the session ID (1-indexed, monotonically increasing within each partition).

## Usage

`sessionize` is used as a window function with `OVER (PARTITION BY ... ORDER BY ...)`.

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

At benchmark scale, `sessionize` processes **1 billion rows in 1.18 seconds**
(848 Melem/s).
