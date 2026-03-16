# Quick Reference

One-page cheat sheet for all `duckdb-behavioral` functions and patterns.

---

## Installation

```sql
INSTALL behavioral FROM community;
LOAD behavioral;
```

---

## Functions at a Glance

### sessionize — Session IDs from inactivity gaps

```sql
sessionize(timestamp_col, INTERVAL 'gap') OVER (
  PARTITION BY user_id ORDER BY timestamp_col
) → BIGINT
```

**Key facts:** Window function (not aggregate). Requires `OVER` clause. Returns
1-indexed session IDs.

---

### retention — Cohort retention analysis

```sql
retention(cond1, cond2, ..., condN) → BOOLEAN[]
```

**Key facts:** Aggregate function. Returns array where `result[i]` is true if
`cond1` AND `cond[i]` were both satisfied somewhere in the group. Supports 2–32
conditions.

---

### window_funnel — Conversion funnel steps

```sql
window_funnel(INTERVAL 'window', timestamp_col, cond1, cond2, ..., condN) → INTEGER
window_funnel(INTERVAL 'window', 'mode_str', timestamp_col, cond1, ...) → INTEGER
```

**Key facts:** Returns furthest step reached (0 = no step matched). Optional
mode string before timestamp.

**Modes:**

| Mode | Effect |
|---|---|
| `strict` | Matched condition must not refire before next step |
| `strict_deduplication` | Alias for `strict` |
| `strict_order` | No earlier conditions between matched steps |
| `strict_increase` | Strictly increasing timestamps between steps |
| `strict_once` | Each event advances at most one step |
| `allow_reentry` | Reset funnel when condition 1 fires again |
| `timestamp_dedup` | Skip events with same timestamp as previous step |

Combine modes: `'strict_increase, strict_once'`

---

### sequence_match — Did pattern occur?

```sql
sequence_match('pattern', timestamp_col, cond1, cond2, ...) → BOOLEAN
```

---

### sequence_count — How many times?

```sql
sequence_count('pattern', timestamp_col, cond1, cond2, ...) → BIGINT
```

Counts **non-overlapping** matches.

---

### sequence_match_events — When did each step match?

```sql
sequence_match_events('pattern', timestamp_col, cond1, cond2, ...) → LIST(TIMESTAMP)
```

Returns list of timestamps, one per matched condition in the pattern.

---

### sequence_next_node — What happened next/before?

```sql
sequence_next_node('direction', 'base', timestamp_col, value_col,
  base_condition, event1_cond, event2_cond, ...) → VARCHAR
```

**Directions:** `'forward'`, `'backward'`

**Bases:** `'head'` (alias: `'first_match'`), `'tail'` (alias: `'last_match'`)

---

## Pattern Syntax

| Element | Syntax | Meaning |
|---|---|---|
| Condition match | `(?N)` | Event where condition N is true (1-indexed) |
| Any one event | `.` | Exactly one event, any conditions |
| Any events | `.*` | Zero or more events |
| Time ≤ | `(?t<=N)` | At most N seconds since previous match |
| Time ≥ | `(?t>=N)` | At least N seconds since previous match |
| Time < | `(?t<N)` | Less than N seconds |
| Time > | `(?t>N)` | More than N seconds |
| Time = | `(?t==N)` | Exactly N seconds |
| Time ≠ | `(?t!=N)` | Not exactly N seconds |

### Common Patterns

| Pattern | Description |
|---|---|
| `(?1).*(?2)` | Condition 1 then 2, any gap |
| `(?1)(?2)` | Condition 1 immediately followed by 2 |
| `(?1).*(?t<=3600)(?2)` | Condition 1 then 2 within 1 hour |
| `(?1).(?2)` | Condition 1, exactly one event, then 2 |
| `(?1).*(?2).*(?3)` | Three-step sequence, any gaps |
| `(?1).*(?t<=300)(?2).*(?t<=300)(?3)` | Three steps, each within 5 minutes |

---

## NULL Handling

| Input | Behavior |
|---|---|
| NULL timestamp | Row is ignored |
| NULL boolean condition | Treated as `false` |
| NULL pattern string | No match (returns false/0/empty) |
| NULL value in `sequence_next_node` | Stored and can be returned |

---

## Limits

| Limit | Value |
|---|---|
| Boolean conditions | 2 – 32 per function call |
| Interval type | No month-based intervals (days, hours, minutes, seconds only) |
| `sessionize` function type | Window function (requires `OVER` clause) |
| All other functions | Aggregate functions (use `GROUP BY`) |

---

## ClickHouse Translation

| ClickHouse | DuckDB behavioral |
|---|---|
| `windowFunnel(3600)(ts, c1, c2)` | `window_funnel(INTERVAL '1 hour', ts, c1, c2)` |
| `sequenceMatch('pat')(ts, c1, c2)` | `sequence_match('pat', ts, c1, c2)` |
| `sequenceCount('pat')(ts, c1, c2)` | `sequence_count('pat', ts, c1, c2)` |
| `sequenceNextNode('f','h')(ts, v, b, e1)` | `sequence_next_node('f', 'h', ts, v, b, e1)` |
| `retention(c1, c2, c3)` | `retention(c1, c2, c3)` |
