# retention

Aggregate function for cohort retention analysis. Takes N boolean conditions and
returns an array indicating which conditions co-occurred with the anchor condition.

## Signature

```
retention(cond1 BOOLEAN, cond2 BOOLEAN [, ...]) -> BOOLEAN[]
```

**Parameters:**

| Parameter | Type | Description |
|---|---|---|
| `cond1` | `BOOLEAN` | Anchor condition (e.g., user appeared in cohort) |
| `cond2..condN` | `BOOLEAN` | Retention conditions for subsequent periods |

Supports 2 to 32 boolean parameters.

**Returns:** `BOOLEAN[]` -- an array of length N where element `i` indicates
whether condition `i` was satisfied alongside the anchor condition.

## Usage

```sql
SELECT cohort_month,
  retention(
    activity_date = cohort_month,
    activity_date = cohort_month + INTERVAL '1 month',
    activity_date = cohort_month + INTERVAL '2 months',
    activity_date = cohort_month + INTERVAL '3 months'
  ) as retained
FROM user_activity
GROUP BY user_id, cohort_month;
```

## Behavior

- `result[0]` is `true` if the anchor condition (`cond1`) was satisfied by any
  row in the group.
- `result[i]` (for `i > 0`) is `true` if **both** the anchor condition and
  condition `i` were satisfied by some row(s) in the group. The conditions do
  not need to be satisfied by the same row.
- If the anchor condition was never satisfied, **all** results are `false`.
  A user who never appeared in the cohort cannot be retained.

### Example

Given three rows for a user in a monthly cohort analysis:

| Row | cond1 (month 0) | cond2 (month 1) | cond3 (month 2) |
|---|---|---|---|
| 1 | true | false | false |
| 2 | false | true | false |
| 3 | false | false | false |

Result: `[true, true, false]`

- The anchor was met (row 1).
- Month 1 retention is confirmed (row 2 satisfies `cond2`, anchor was met by row 1).
- Month 2 was never satisfied.

## Implementation

Conditions are tracked as a `u32` bitmask, where bit `i` is set when condition
`i` evaluates to true for any row. The combine operation is a single bitwise OR.

| Operation | Complexity |
|---|---|
| Update | O(k) where k = number of conditions |
| Combine | O(1) |
| Finalize | O(k) |
| Space | O(1) -- a single `u32` bitmask |

At benchmark scale, `retention` combines **100 million states in 259 ms**
(386 Melem/s).

## See Also

- [`sessionize`](./sessionize.md) -- session assignment based on timestamp gaps
- [`window_funnel`](./window-funnel.md) -- conversion funnel step tracking within time windows
