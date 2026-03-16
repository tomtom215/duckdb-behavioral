# SQL Cookbook

Practical recipes for common behavioral analytics patterns. Each recipe is
self-contained — copy and paste into a DuckDB session with the extension loaded.

---

## Funnel Recipes

### Basic Conversion Funnel

```sql
SELECT user_id,
  window_funnel(INTERVAL '1 hour', event_time,
    event_type = 'page_view',
    event_type = 'add_to_cart',
    event_type = 'purchase'
  ) as furthest_step
FROM events
GROUP BY user_id;
```

### Funnel Drop-off Report

Aggregate per-user funnel results into a conversion report showing where users
abandon:

```sql
WITH funnels AS (
  SELECT user_id,
    window_funnel(INTERVAL '1 hour', event_time,
      event_type = 'page_view',
      event_type = 'add_to_cart',
      event_type = 'checkout',
      event_type = 'purchase'
    ) as step
  FROM events GROUP BY user_id
)
SELECT
  step as reached_step,
  COUNT(*) as users,
  ROUND(100.0 * COUNT(*) / SUM(COUNT(*)) OVER (), 1) as pct
FROM funnels
GROUP BY step
ORDER BY step;
```

### Funnel by Date

Track daily funnel conversion rates:

```sql
SELECT
  event_time::DATE as day,
  COUNT(*) as total_users,
  SUM(CASE WHEN step >= 1 THEN 1 ELSE 0 END) as viewed,
  SUM(CASE WHEN step >= 2 THEN 1 ELSE 0 END) as carted,
  SUM(CASE WHEN step >= 3 THEN 1 ELSE 0 END) as purchased,
  ROUND(100.0 * SUM(CASE WHEN step >= 3 THEN 1 ELSE 0 END) /
    NULLIF(SUM(CASE WHEN step >= 1 THEN 1 ELSE 0 END), 0), 1) as conversion_pct
FROM (
  SELECT user_id, MIN(event_time) as event_time,
    window_funnel(INTERVAL '1 hour', event_time,
      event_type = 'page_view',
      event_type = 'add_to_cart',
      event_type = 'purchase'
    ) as step
  FROM events
  GROUP BY user_id
)
GROUP BY day
ORDER BY day;
```

### Strict Funnel (No Repeated Steps)

Use `strict_increase` mode to require strictly increasing timestamps between
steps — no duplicate timestamps allowed:

```sql
SELECT user_id,
  window_funnel(INTERVAL '1 hour', 'strict_increase', event_time,
    event_type = 'page_view',
    event_type = 'add_to_cart',
    event_type = 'purchase'
  ) as step
FROM events GROUP BY user_id;
```

### Funnel with Re-entry

Allow the funnel to restart when the first condition fires again, tracking the
best attempt:

```sql
SELECT user_id,
  window_funnel(INTERVAL '1 hour', 'allow_reentry', event_time,
    event_type = 'page_view',
    event_type = 'add_to_cart',
    event_type = 'purchase'
  ) as step
FROM events GROUP BY user_id;
```

### Funnel by Segment

Compare funnel performance across user segments (device, campaign, etc.):

```sql
SELECT
  device_type,
  COUNT(*) as users,
  ROUND(AVG(step), 2) as avg_step,
  SUM(CASE WHEN step = 4 THEN 1 ELSE 0 END) as completed,
  ROUND(100.0 * SUM(CASE WHEN step = 4 THEN 1 ELSE 0 END) / COUNT(*), 1) as pct
FROM (
  SELECT user_id, device_type,
    window_funnel(INTERVAL '1 hour', event_time,
      event_type = 'page_view',
      event_type = 'add_to_cart',
      event_type = 'checkout',
      event_type = 'purchase'
    ) as step
  FROM events GROUP BY user_id, device_type
)
GROUP BY device_type
ORDER BY pct DESC;
```

---

## Session Recipes

### Basic Session Assignment

```sql
SELECT user_id, event_time,
  sessionize(event_time, INTERVAL '30 minutes') OVER (
    PARTITION BY user_id ORDER BY event_time
  ) as session_id
FROM events;
```

### Session Metrics (Duration, Page Count, Bounce Rate)

```sql
WITH sessionized AS (
  SELECT user_id, event_time, page_url,
    sessionize(event_time, INTERVAL '30 minutes') OVER (
      PARTITION BY user_id ORDER BY event_time
    ) as session_id
  FROM events
)
SELECT
  user_id,
  session_id,
  COUNT(*) as page_views,
  MIN(event_time) as started_at,
  MAX(event_time) as ended_at,
  EXTRACT(EPOCH FROM MAX(event_time) - MIN(event_time)) as duration_sec,
  CASE WHEN COUNT(*) = 1 THEN true ELSE false END as is_bounce
FROM sessionized
GROUP BY user_id, session_id;
```

### Sessions Per User Per Day

```sql
WITH sessionized AS (
  SELECT user_id, event_time,
    sessionize(event_time, INTERVAL '30 minutes') OVER (
      PARTITION BY user_id ORDER BY event_time
    ) as session_id
  FROM events
)
SELECT
  user_id,
  event_time::DATE as day,
  COUNT(DISTINCT session_id) as sessions
FROM sessionized
GROUP BY user_id, day
ORDER BY user_id, day;
```

### Average Session Duration by Day

```sql
WITH sessionized AS (
  SELECT user_id, event_time,
    sessionize(event_time, INTERVAL '30 minutes') OVER (
      PARTITION BY user_id ORDER BY event_time
    ) as session_id
  FROM events
),
session_stats AS (
  SELECT
    user_id, session_id,
    MIN(event_time)::DATE as day,
    EXTRACT(EPOCH FROM MAX(event_time) - MIN(event_time)) as duration_sec
  FROM sessionized
  GROUP BY user_id, session_id
)
SELECT
  day,
  COUNT(*) as sessions,
  ROUND(AVG(duration_sec), 0) as avg_duration_sec,
  ROUND(AVG(duration_sec) / 60.0, 1) as avg_duration_min
FROM session_stats
GROUP BY day
ORDER BY day;
```

### Entry Page Analysis

Identify the first page of each session for channel attribution:

```sql
WITH sessionized AS (
  SELECT user_id, event_time, page_url, referrer,
    sessionize(event_time, INTERVAL '30 minutes') OVER (
      PARTITION BY user_id ORDER BY event_time
    ) as session_id
  FROM events
)
SELECT
  page_url as entry_page,
  COUNT(*) as sessions,
  ROUND(100.0 * COUNT(*) / SUM(COUNT(*)) OVER (), 1) as pct
FROM (
  SELECT DISTINCT ON (user_id, session_id) user_id, session_id, page_url, referrer
  FROM sessionized
  ORDER BY user_id, session_id, event_time
)
GROUP BY page_url
ORDER BY sessions DESC;
```

---

## Retention Recipes

### Weekly Cohort Retention

```sql
SELECT
  cohort_week,
  COUNT(*) as cohort_size,
  SUM(CASE WHEN r[1] THEN 1 ELSE 0 END) as week_0,
  SUM(CASE WHEN r[2] THEN 1 ELSE 0 END) as week_1,
  SUM(CASE WHEN r[3] THEN 1 ELSE 0 END) as week_2,
  SUM(CASE WHEN r[4] THEN 1 ELSE 0 END) as week_3,
  ROUND(100.0 * SUM(CASE WHEN r[2] THEN 1 ELSE 0 END) /
    NULLIF(SUM(CASE WHEN r[1] THEN 1 ELSE 0 END), 0), 1) as w1_pct,
  ROUND(100.0 * SUM(CASE WHEN r[3] THEN 1 ELSE 0 END) /
    NULLIF(SUM(CASE WHEN r[1] THEN 1 ELSE 0 END), 0), 1) as w2_pct,
  ROUND(100.0 * SUM(CASE WHEN r[4] THEN 1 ELSE 0 END) /
    NULLIF(SUM(CASE WHEN r[1] THEN 1 ELSE 0 END), 0), 1) as w3_pct
FROM (
  SELECT user_id, cohort_week,
    retention(
      activity_date >= cohort_week AND activity_date < cohort_week + INTERVAL '7 days',
      activity_date >= cohort_week + INTERVAL '7 days' AND activity_date < cohort_week + INTERVAL '14 days',
      activity_date >= cohort_week + INTERVAL '14 days' AND activity_date < cohort_week + INTERVAL '21 days',
      activity_date >= cohort_week + INTERVAL '21 days' AND activity_date < cohort_week + INTERVAL '28 days'
    ) as r
  FROM activity GROUP BY user_id, cohort_week
)
GROUP BY cohort_week
ORDER BY cohort_week;
```

### Day-1 / Day-7 / Day-30 Retention

Classic mobile/SaaS retention metrics:

```sql
SELECT
  signup_date,
  COUNT(*) as new_users,
  SUM(CASE WHEN r[1] THEN 1 ELSE 0 END) as day_0,
  ROUND(100.0 * SUM(CASE WHEN r[2] THEN 1 ELSE 0 END) /
    NULLIF(COUNT(*), 0), 1) as d1_pct,
  ROUND(100.0 * SUM(CASE WHEN r[3] THEN 1 ELSE 0 END) /
    NULLIF(COUNT(*), 0), 1) as d7_pct,
  ROUND(100.0 * SUM(CASE WHEN r[4] THEN 1 ELSE 0 END) /
    NULLIF(COUNT(*), 0), 1) as d30_pct
FROM (
  SELECT user_id, signup_date,
    retention(
      activity_date = signup_date,
      activity_date = signup_date + INTERVAL '1 day',
      activity_date = signup_date + INTERVAL '7 days',
      activity_date = signup_date + INTERVAL '30 days'
    ) as r
  FROM user_activity GROUP BY user_id, signup_date
)
GROUP BY signup_date
ORDER BY signup_date;
```

### Retention by Segment

Compare retention across user segments (plan type, acquisition channel, etc.):

```sql
SELECT
  plan_type,
  COUNT(*) as users,
  ROUND(100.0 * SUM(CASE WHEN r[2] THEN 1 ELSE 0 END) /
    NULLIF(SUM(CASE WHEN r[1] THEN 1 ELSE 0 END), 0), 1) as w1_retention,
  ROUND(100.0 * SUM(CASE WHEN r[3] THEN 1 ELSE 0 END) /
    NULLIF(SUM(CASE WHEN r[1] THEN 1 ELSE 0 END), 0), 1) as w2_retention
FROM (
  SELECT user_id, plan_type,
    retention(
      activity_week = signup_week,
      activity_week = signup_week + INTERVAL '7 days',
      activity_week = signup_week + INTERVAL '14 days'
    ) as r
  FROM user_activity GROUP BY user_id, plan_type
)
GROUP BY plan_type
ORDER BY w1_retention DESC;
```

---

## Sequence Pattern Recipes

### Detect a Specific Event Sequence

Did the user view a product then add it to cart (with any events in between)?

```sql
SELECT user_id,
  sequence_match('(?1).*(?2)', event_time,
    event_type = 'page_view',
    event_type = 'add_to_cart'
  ) as viewed_then_carted
FROM events GROUP BY user_id;
```

### Time-Constrained Pattern

User signed up and completed onboarding within 10 minutes:

```sql
SELECT user_id,
  sequence_match('(?1).*(?t<=600)(?2)', event_time,
    event_type = 'signup',
    event_type = 'onboarding_complete'
  ) as fast_onboarder
FROM events GROUP BY user_id;
```

### Count Repeated Patterns

How many times does each user repeat the browse → cart cycle?

```sql
SELECT user_id,
  sequence_count('(?1).*(?2)', event_time,
    event_type = 'page_view',
    event_type = 'add_to_cart'
  ) as browse_cart_cycles
FROM events
GROUP BY user_id
ORDER BY browse_cart_cycles DESC;
```

### Multi-Step Pattern with Wildcards

Detect users who viewed, added to cart, then purchased — but NOT immediately
(at least one event between cart and purchase):

```sql
SELECT user_id,
  sequence_match('(?1).*(?2).(?3)', event_time,
    event_type = 'page_view',
    event_type = 'add_to_cart',
    event_type = 'purchase'
  ) as indirect_purchase
FROM events GROUP BY user_id;
```

### Get Matched Timestamps

Retrieve the exact timestamps when each step in a 3-step pattern matched:

```sql
SELECT user_id,
  sequence_match_events('(?1).*(?2).*(?3)', event_time,
    event_type = 'signup',
    event_type = 'first_purchase',
    event_type = 'second_purchase'
  ) as milestone_times
FROM events GROUP BY user_id;
```

### Time Between Pattern Steps

Calculate the time between matched pattern steps:

```sql
WITH matched AS (
  SELECT user_id,
    sequence_match_events('(?1).*(?2).*(?3)', event_time,
      event_type = 'signup',
      event_type = 'first_purchase',
      event_type = 'review'
    ) as ts
  FROM events GROUP BY user_id
)
SELECT user_id,
  ts[1] as signup_time,
  ts[2] as purchase_time,
  ts[3] as review_time,
  EXTRACT(EPOCH FROM ts[2] - ts[1]) / 3600.0 as hours_to_purchase,
  EXTRACT(EPOCH FROM ts[3] - ts[2]) / 3600.0 as hours_to_review
FROM matched
WHERE len(ts) = 3;
```

---

## User Flow Recipes

### Forward Flow — What Happens Next?

After Home → Product, what do users do next?

```sql
SELECT
  COALESCE(next_page, '(end of session)') as next_page,
  COUNT(*) as users,
  ROUND(100.0 * COUNT(*) / SUM(COUNT(*)) OVER (), 1) as pct
FROM (
  SELECT user_id,
    sequence_next_node('forward', 'first_match', event_time, page,
      page = 'Home', page = 'Home', page = 'Product'
    ) as next_page
  FROM events GROUP BY user_id
)
GROUP BY next_page
ORDER BY users DESC;
```

### Backward Flow — What Led Here?

What page do users visit immediately before reaching the Checkout page?

```sql
SELECT
  COALESCE(prev_page, '(start of session)') as prev_page,
  COUNT(*) as users,
  ROUND(100.0 * COUNT(*) / SUM(COUNT(*)) OVER (), 1) as pct
FROM (
  SELECT user_id,
    sequence_next_node('backward', 'first_match', event_time, page,
      page = 'Checkout', page = 'Checkout'
    ) as prev_page
  FROM events GROUP BY user_id
)
GROUP BY prev_page
ORDER BY users DESC;
```

### Last-Match Flow

Using `last_match` base to find the next page after the *last* occurrence of
the Home → Product pattern:

```sql
SELECT user_id,
  sequence_next_node('forward', 'last_match', event_time, page,
    page = 'Home', page = 'Home', page = 'Product'
  ) as next_page_after_last
FROM events GROUP BY user_id;
```

---

## Combined Analysis Recipes

### A/B Test Behavioral Comparison

Compare funnel depth and conversion speed between test groups:

```sql
WITH funnel AS (
  SELECT user_id, test_group,
    window_funnel(INTERVAL '2 hours', event_time,
      event_type = 'signup',
      event_type = 'profile_setup',
      event_type = 'first_action'
    ) as step
  FROM events GROUP BY user_id, test_group
),
speed AS (
  SELECT user_id, test_group,
    sequence_match('(?1).*(?t<=1800)(?2).*(?t<=1800)(?3)', event_time,
      event_type = 'signup',
      event_type = 'profile_setup',
      event_type = 'first_action'
    ) as completed_fast
  FROM events GROUP BY user_id, test_group
)
SELECT
  f.test_group,
  COUNT(*) as users,
  ROUND(AVG(f.step), 2) as avg_depth,
  SUM(CASE WHEN f.step = 3 THEN 1 ELSE 0 END) as completed,
  ROUND(100.0 * SUM(CASE WHEN f.step = 3 THEN 1 ELSE 0 END) / COUNT(*), 1) as complete_pct,
  SUM(CASE WHEN s.completed_fast THEN 1 ELSE 0 END) as fast_completers
FROM funnel f
JOIN speed s ON f.user_id = s.user_id
GROUP BY f.test_group;
```

### Session + Funnel Combined

Analyze funnel performance per session:

```sql
WITH sessionized AS (
  SELECT *, sessionize(event_time, INTERVAL '30 minutes') OVER (
    PARTITION BY user_id ORDER BY event_time
  ) as session_id
  FROM events
)
SELECT user_id, session_id,
  window_funnel(INTERVAL '1 hour', event_time,
    event_type = 'page_view',
    event_type = 'add_to_cart',
    event_type = 'purchase'
  ) as step
FROM sessionized
GROUP BY user_id, session_id
ORDER BY user_id, session_id;
```

### Power Users Detection

Find users with the most repeated behavioral patterns:

```sql
SELECT user_id,
  sequence_count('(?1).*(?2)', event_time,
    event_type = 'search',
    event_type = 'page_view'
  ) as search_browse_cycles,
  sequence_count('(?1).*(?2)', event_time,
    event_type = 'page_view',
    event_type = 'add_to_cart'
  ) as browse_cart_cycles,
  window_funnel(INTERVAL '24 hours', event_time,
    event_type = 'page_view',
    event_type = 'add_to_cart',
    event_type = 'purchase'
  ) as funnel_depth
FROM events
GROUP BY user_id
ORDER BY browse_cart_cycles DESC
LIMIT 20;
```

### Querying Parquet Files Directly

All recipes work with any DuckDB-supported file format:

```sql
SELECT user_id,
  window_funnel(INTERVAL '1 hour', event_time,
    event_type = 'view', event_type = 'cart', event_type = 'purchase'
  ) as step
FROM read_parquet('s3://my-bucket/events/*.parquet')
WHERE event_time >= '2024-01-01'
GROUP BY user_id;
```

---

## Pattern Syntax Quick Reference

| Pattern | Meaning |
|---|---|
| `(?N)` | Match event where condition N is true (1-indexed) |
| `.` | Match exactly one event (any) |
| `.*` | Match zero or more events (any) |
| `(?t<=N)` | At most N seconds since previous match |
| `(?t>=N)` | At least N seconds since previous match |
| `(?t<N)` | Less than N seconds since previous match |
| `(?t>N)` | More than N seconds since previous match |
| `(?t==N)` | Exactly N seconds since previous match |
| `(?t!=N)` | Not exactly N seconds since previous match |

**Common patterns:**

```
(?1).*(?2)              -- cond1 then cond2, any gap
(?1)(?2)                -- cond1 immediately followed by cond2
(?1).*(?t<=3600)(?2)    -- cond1 then cond2 within 1 hour
(?1).(?2)               -- cond1, one event, then cond2
(?1).*(?2).*(?3)        -- three-step sequence
```
