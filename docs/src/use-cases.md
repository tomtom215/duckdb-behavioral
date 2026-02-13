# Use Cases

This page presents five real-world use cases for `duckdb-behavioral`, each with a
problem description, sample data, the analytical query, and interpretation of
results. All examples are self-contained -- you can copy and paste each section
into a DuckDB session with the extension loaded.

---

## E-Commerce Conversion Funnel Analysis

### Problem

An e-commerce team wants to measure how far users progress through the purchase
funnel: product page view, add to cart, begin checkout, and complete purchase.
Understanding where users drop off helps prioritize UX improvements. The team
needs per-user funnel progress within a 1-hour conversion window, and an
aggregate drop-off report across all users.

### Sample Data

```sql
CREATE TABLE ecommerce_events (
    user_id    VARCHAR NOT NULL,
    event_time TIMESTAMP NOT NULL,
    event_type VARCHAR NOT NULL,
    product_id VARCHAR,
    revenue    DECIMAL(10,2)
);

INSERT INTO ecommerce_events VALUES
    -- User A: completes full funnel
    ('user_a', '2024-01-15 10:00:00', 'page_view',    'prod_1', NULL),
    ('user_a', '2024-01-15 10:05:00', 'add_to_cart',  'prod_1', NULL),
    ('user_a', '2024-01-15 10:12:00', 'checkout',     'prod_1', NULL),
    ('user_a', '2024-01-15 10:15:00', 'purchase',     'prod_1', 49.99),

    -- User B: views and adds to cart, but abandons
    ('user_b', '2024-01-15 11:00:00', 'page_view',    'prod_2', NULL),
    ('user_b', '2024-01-15 11:03:00', 'add_to_cart',  'prod_2', NULL),
    ('user_b', '2024-01-15 11:30:00', 'page_view',    'prod_3', NULL),

    -- User C: views only, never adds to cart
    ('user_c', '2024-01-15 14:00:00', 'page_view',    'prod_1', NULL),
    ('user_c', '2024-01-15 14:10:00', 'page_view',    'prod_4', NULL),

    -- User D: completes funnel but checkout is outside window
    ('user_d', '2024-01-15 09:00:00', 'page_view',    'prod_5', NULL),
    ('user_d', '2024-01-15 09:10:00', 'add_to_cart',  'prod_5', NULL),
    ('user_d', '2024-01-15 10:30:00', 'checkout',     'prod_5', NULL),
    ('user_d', '2024-01-15 10:35:00', 'purchase',     'prod_5', 29.99);
```

### Analytical Query

```sql
-- Step 1: Per-user funnel progress
WITH user_funnels AS (
    SELECT
        user_id,
        window_funnel(
            INTERVAL '1 hour',
            event_time,
            event_type = 'page_view',
            event_type = 'add_to_cart',
            event_type = 'checkout',
            event_type = 'purchase'
        ) AS furthest_step
    FROM ecommerce_events
    GROUP BY user_id
)
-- Step 2: Aggregate into a drop-off report
SELECT
    furthest_step,
    COUNT(*) AS user_count,
    ROUND(100.0 * COUNT(*) / SUM(COUNT(*)) OVER (), 1) AS pct
FROM user_funnels
GROUP BY furthest_step
ORDER BY furthest_step;
```

### Expected Results

| furthest_step | user_count | pct |
|---|---|---|
| 1 | 1 | 25.0 |
| 2 | 2 | 50.0 |
| 4 | 1 | 25.0 |

### Interpretation

- **User A** reached step 4 (purchase) -- the full funnel was completed within
  1 hour.
- **User B** reached step 2 (add to cart). The second page view did not advance
  the funnel further because it matched step 1 again, not step 3.
- **User C** reached step 1 (page view only). No add-to-cart event occurred.
- **User D** reached step 2 (add to cart). Although checkout and purchase events
  exist, they occurred more than 1 hour after the initial page view (09:00 to
  10:30 is 90 minutes), so they fall outside the window.

The drop-off report shows 50% of users stalling at the add-to-cart stage, which
suggests the checkout flow needs attention. To use `strict_increase` mode and
ensure each step has a strictly later timestamp:

```sql
window_funnel(
    INTERVAL '1 hour',
    'strict_increase',
    event_time,
    event_type = 'page_view',
    event_type = 'add_to_cart',
    event_type = 'checkout',
    event_type = 'purchase'
)
```

---

## SaaS User Retention Cohort Analysis

### Problem

A SaaS product team wants to measure weekly retention: of users who were active
in week 0, what fraction returned in week 1, week 2, and week 3? This cohort
analysis helps track product stickiness and identify retention trends across
signup cohorts.

### Sample Data

```sql
CREATE TABLE saas_activity (
    user_id       VARCHAR NOT NULL,
    activity_date DATE NOT NULL,
    cohort_week   DATE NOT NULL  -- the Monday of the user's signup week
);

INSERT INTO saas_activity VALUES
    -- User A: active in weeks 0, 1, and 3
    ('user_a', '2024-01-01', '2024-01-01'),
    ('user_a', '2024-01-09', '2024-01-01'),
    ('user_a', '2024-01-22', '2024-01-01'),

    -- User B: active in weeks 0 and 1 only
    ('user_b', '2024-01-02', '2024-01-01'),
    ('user_b', '2024-01-08', '2024-01-01'),

    -- User C: active in week 0 only
    ('user_c', '2024-01-03', '2024-01-01'),

    -- User D: active in weeks 0, 1, 2, and 3
    ('user_d', '2024-01-01', '2024-01-01'),
    ('user_d', '2024-01-10', '2024-01-01'),
    ('user_d', '2024-01-15', '2024-01-01'),
    ('user_d', '2024-01-23', '2024-01-01'),

    -- User E: different cohort (week of Jan 8), active in weeks 0 and 2
    ('user_e', '2024-01-08', '2024-01-08'),
    ('user_e', '2024-01-22', '2024-01-08');
```

### Analytical Query

```sql
-- Per-user retention array
WITH user_retention AS (
    SELECT
        user_id,
        cohort_week,
        retention(
            activity_date >= cohort_week
                AND activity_date < cohort_week + INTERVAL '7 days',
            activity_date >= cohort_week + INTERVAL '7 days'
                AND activity_date < cohort_week + INTERVAL '14 days',
            activity_date >= cohort_week + INTERVAL '14 days'
                AND activity_date < cohort_week + INTERVAL '21 days',
            activity_date >= cohort_week + INTERVAL '21 days'
                AND activity_date < cohort_week + INTERVAL '28 days'
        ) AS retained
    FROM saas_activity
    GROUP BY user_id, cohort_week
)
-- Aggregate retention rates per cohort
SELECT
    cohort_week,
    COUNT(*) AS cohort_size,
    SUM(CASE WHEN retained[1] THEN 1 ELSE 0 END) AS week_0,
    SUM(CASE WHEN retained[2] THEN 1 ELSE 0 END) AS week_1,
    SUM(CASE WHEN retained[3] THEN 1 ELSE 0 END) AS week_2,
    SUM(CASE WHEN retained[4] THEN 1 ELSE 0 END) AS week_3,
    ROUND(100.0 * SUM(CASE WHEN retained[2] THEN 1 ELSE 0 END) /
        NULLIF(SUM(CASE WHEN retained[1] THEN 1 ELSE 0 END), 0), 1)
        AS week_1_pct,
    ROUND(100.0 * SUM(CASE WHEN retained[3] THEN 1 ELSE 0 END) /
        NULLIF(SUM(CASE WHEN retained[1] THEN 1 ELSE 0 END), 0), 1)
        AS week_2_pct,
    ROUND(100.0 * SUM(CASE WHEN retained[4] THEN 1 ELSE 0 END) /
        NULLIF(SUM(CASE WHEN retained[1] THEN 1 ELSE 0 END), 0), 1)
        AS week_3_pct
FROM user_retention
GROUP BY cohort_week
ORDER BY cohort_week;
```

### Expected Results

| cohort_week | cohort_size | week_0 | week_1 | week_2 | week_3 | week_1_pct | week_2_pct | week_3_pct |
|---|---|---|---|---|---|---|---|---|
| 2024-01-01 | 4 | 4 | 3 | 1 | 2 | 75.0 | 25.0 | 50.0 |
| 2024-01-08 | 1 | 1 | 0 | 1 | 0 | 0.0 | 100.0 | 0.0 |

### Interpretation

For the January 1 cohort (4 users):
- All 4 users were active in week 0 (by definition, since they have activity in
  the cohort week).
- 3 of 4 (75%) returned in week 1 (users A, B, D).
- Only 1 of 4 (25%) was active in week 2 (user D).
- 2 of 4 (50%) returned in week 3 (users A, D).

The retention curve shows a steep drop after week 1, with partial recovery in
week 3. This "smile curve" pattern suggests users who survive week 1 become
long-term engaged, while week 2 is a critical churn risk period.

Note that `retention` checks whether the anchor condition (week 0) and each
subsequent condition were both satisfied somewhere in the group. The conditions
do not need to be satisfied by the same row.

---

## Web Analytics Session Analysis

### Problem

A web analytics team needs to segment user activity into sessions based on a
30-minute inactivity threshold, then compute session-level metrics: page count
per session, session duration, and sessions per user. This is foundational for
engagement analysis, bounce rate computation, and session-level attribution.

### Sample Data

```sql
CREATE TABLE page_views (
    user_id    VARCHAR NOT NULL,
    event_time TIMESTAMP NOT NULL,
    page_url   VARCHAR NOT NULL,
    referrer   VARCHAR
);

INSERT INTO page_views VALUES
    -- User A: two distinct sessions
    ('user_a', '2024-01-15 10:00:00', '/home',     'google.com'),
    ('user_a', '2024-01-15 10:05:00', '/products',  NULL),
    ('user_a', '2024-01-15 10:08:00', '/product/1', NULL),
    ('user_a', '2024-01-15 10:25:00', '/cart',       NULL),
    -- 2-hour gap
    ('user_a', '2024-01-15 12:30:00', '/home',     'email-campaign'),
    ('user_a', '2024-01-15 12:35:00', '/products',  NULL),

    -- User B: single long session
    ('user_b', '2024-01-15 09:00:00', '/home',     'direct'),
    ('user_b', '2024-01-15 09:10:00', '/about',     NULL),
    ('user_b', '2024-01-15 09:15:00', '/pricing',   NULL),
    ('user_b', '2024-01-15 09:20:00', '/signup',    NULL),

    -- User C: three short sessions (bounce-like)
    ('user_c', '2024-01-15 08:00:00', '/home',     'google.com'),
    -- 45-minute gap
    ('user_c', '2024-01-15 08:45:00', '/home',     'google.com'),
    -- 1-hour gap
    ('user_c', '2024-01-15 09:45:00', '/blog/1',   'twitter.com');
```

### Analytical Query

```sql
-- Step 1: Assign session IDs
WITH sessionized AS (
    SELECT
        user_id,
        event_time,
        page_url,
        referrer,
        sessionize(event_time, INTERVAL '30 minutes') OVER (
            PARTITION BY user_id ORDER BY event_time
        ) AS session_id
    FROM page_views
),
-- Step 2: Session-level metrics
session_metrics AS (
    SELECT
        user_id,
        session_id,
        COUNT(*) AS pages_viewed,
        MIN(event_time) AS session_start,
        MAX(event_time) AS session_end,
        EXTRACT(EPOCH FROM MAX(event_time) - MIN(event_time)) AS duration_seconds,
        FIRST(referrer) AS entry_referrer
    FROM sessionized
    GROUP BY user_id, session_id
)
-- Step 3: User-level summary
SELECT
    user_id,
    COUNT(*) AS total_sessions,
    ROUND(AVG(pages_viewed), 1) AS avg_pages_per_session,
    ROUND(AVG(duration_seconds), 0) AS avg_duration_seconds,
    SUM(CASE WHEN pages_viewed = 1 THEN 1 ELSE 0 END) AS bounce_sessions
FROM session_metrics
GROUP BY user_id
ORDER BY user_id;
```

### Expected Results

| user_id | total_sessions | avg_pages_per_session | avg_duration_seconds | bounce_sessions |
|---|---|---|---|---|
| user_a | 2 | 3.0 | 900 | 0 |
| user_b | 1 | 4.0 | 1200 | 0 |
| user_c | 3 | 1.0 | 0 | 3 |

### Interpretation

- **User A** had 2 sessions. The first session (4 pages, 25 minutes from 10:00
  to 10:25) shows engaged browsing. The 2-hour gap started a new session (2
  pages, 5 minutes) from an email campaign.
- **User B** had 1 session spanning 20 minutes across 4 pages, ending at the
  signup page -- a high-intent user.
- **User C** had 3 bounce sessions (1 page each). Each visit had a gap exceeding
  30 minutes, so each was a separate session. All sessions have zero duration
  because there is only one page view per session.

This analysis shows that User C is a repeat but non-engaged visitor, while Users
A and B demonstrate meaningful engagement. The `entry_referrer` field from the
session CTE can be used for channel attribution at the session level.

---

## User Journey and Flow Analysis

### Problem

A product analytics team wants to understand user navigation flow: after visiting
the Home page and then the Product page, what page do users visit next? This
"next node" analysis reveals common user journeys and helps identify navigation
bottlenecks. The team also wants to understand what pages lead users to the
Checkout page (backward analysis).

### Sample Data

```sql
CREATE TABLE navigation_events (
    user_id    VARCHAR NOT NULL,
    event_time TIMESTAMP NOT NULL,
    page       VARCHAR NOT NULL
);

INSERT INTO navigation_events VALUES
    -- User A: Home -> Product -> Cart -> Checkout -> Confirmation
    ('user_a', '2024-01-15 10:00:00', 'Home'),
    ('user_a', '2024-01-15 10:02:00', 'Product'),
    ('user_a', '2024-01-15 10:05:00', 'Cart'),
    ('user_a', '2024-01-15 10:08:00', 'Checkout'),
    ('user_a', '2024-01-15 10:10:00', 'Confirmation'),

    -- User B: Home -> Product -> Product -> Home (browsing, no conversion)
    ('user_b', '2024-01-15 11:00:00', 'Home'),
    ('user_b', '2024-01-15 11:03:00', 'Product'),
    ('user_b', '2024-01-15 11:07:00', 'Product'),
    ('user_b', '2024-01-15 11:10:00', 'Home'),

    -- User C: Home -> Product -> Cart -> Home (cart abandonment)
    ('user_c', '2024-01-15 14:00:00', 'Home'),
    ('user_c', '2024-01-15 14:05:00', 'Product'),
    ('user_c', '2024-01-15 14:08:00', 'Cart'),
    ('user_c', '2024-01-15 14:15:00', 'Home'),

    -- User D: Home -> Product -> Checkout (skipped cart)
    ('user_d', '2024-01-15 15:00:00', 'Home'),
    ('user_d', '2024-01-15 15:02:00', 'Product'),
    ('user_d', '2024-01-15 15:05:00', 'Checkout');
```

### Analytical Query: Forward Flow

```sql
-- What page do users visit after Home -> Product?
SELECT
    user_id,
    sequence_next_node(
        'forward',
        'first_match',
        event_time,
        page,
        page = 'Home',      -- base_condition: start from Home
        page = 'Home',      -- event1: match Home
        page = 'Product'    -- event2: then match Product
    ) AS next_page_after_product
FROM navigation_events
GROUP BY user_id
ORDER BY user_id;
```

### Expected Results (Forward)

| user_id | next_page_after_product |
|---|---|
| user_a | Cart |
| user_b | Product |
| user_c | Cart |
| user_d | Checkout |

### Analytical Query: Backward Flow

```sql
-- What page leads users to arrive at Checkout?
SELECT
    user_id,
    sequence_next_node(
        'backward',
        'first_match',
        event_time,
        page,
        page = 'Checkout',     -- base_condition: anchor on Checkout
        page = 'Checkout'      -- event1: match Checkout
    ) AS page_before_checkout
FROM navigation_events
WHERE user_id IN ('user_a', 'user_d')  -- only users who reached Checkout
GROUP BY user_id
ORDER BY user_id;
```

### Expected Results (Backward)

| user_id | page_before_checkout |
|---|---|
| user_a | Cart |
| user_d | Product |

### Aggregate Flow Distribution

```sql
-- Distribution: what do users do after Home -> Product?
WITH next_pages AS (
    SELECT
        sequence_next_node(
            'forward',
            'first_match',
            event_time,
            page,
            page = 'Home',
            page = 'Home',
            page = 'Product'
        ) AS next_page
    FROM navigation_events
    GROUP BY user_id
)
SELECT
    COALESCE(next_page, '(end of session)') AS next_page,
    COUNT(*) AS user_count,
    ROUND(100.0 * COUNT(*) / SUM(COUNT(*)) OVER (), 1) AS pct
FROM next_pages
GROUP BY next_page
ORDER BY user_count DESC;
```

### Interpretation

The forward analysis reveals that after the Home-to-Product sequence:
- 50% of users proceed to Cart (users A and C) -- the intended happy path.
- 25% stay on Product pages (user B) -- likely comparing products.
- 25% go directly to Checkout (user D) -- possible express checkout behavior.

The backward analysis shows that the Cart page is the most common predecessor
to Checkout, while some users skip the Cart entirely. This information helps
the product team understand whether the Cart step adds friction or value to
the conversion flow.

---

## A/B Test Behavioral Analysis

### Problem

A growth team is running an A/B test on a new onboarding flow. They need to
compare behavioral patterns between the control and treatment groups. Beyond
simple conversion rates, they want to measure: funnel depth, event sequence
completion, and the number of times users repeat certain actions. This
multi-dimensional behavioral comparison reveals whether the new flow changes
user behavior patterns, not just outcomes.

### Sample Data

```sql
CREATE TABLE ab_test_events (
    user_id     VARCHAR NOT NULL,
    event_time  TIMESTAMP NOT NULL,
    event_type  VARCHAR NOT NULL,
    test_group  VARCHAR NOT NULL   -- 'control' or 'treatment'
);

INSERT INTO ab_test_events VALUES
    -- Control group: User A - completes onboarding slowly
    ('user_a', '2024-01-15 10:00:00', 'signup',          'control'),
    ('user_a', '2024-01-15 10:05:00', 'profile_setup',   'control'),
    ('user_a', '2024-01-15 10:30:00', 'tutorial_start',  'control'),
    ('user_a', '2024-01-15 10:45:00', 'tutorial_end',    'control'),
    ('user_a', '2024-01-15 11:00:00', 'first_action',    'control'),

    -- Control group: User B - drops off after profile
    ('user_b', '2024-01-15 11:00:00', 'signup',          'control'),
    ('user_b', '2024-01-15 11:10:00', 'profile_setup',   'control'),
    ('user_b', '2024-01-15 11:15:00', 'tutorial_start',  'control'),

    -- Control group: User C - signs up only
    ('user_c', '2024-01-15 12:00:00', 'signup',          'control'),

    -- Treatment group: User D - completes onboarding quickly
    ('user_d', '2024-01-15 10:00:00', 'signup',          'treatment'),
    ('user_d', '2024-01-15 10:02:00', 'profile_setup',   'treatment'),
    ('user_d', '2024-01-15 10:05:00', 'tutorial_start',  'treatment'),
    ('user_d', '2024-01-15 10:08:00', 'tutorial_end',    'treatment'),
    ('user_d', '2024-01-15 10:10:00', 'first_action',    'treatment'),

    -- Treatment group: User E - completes onboarding
    ('user_e', '2024-01-15 13:00:00', 'signup',          'treatment'),
    ('user_e', '2024-01-15 13:03:00', 'profile_setup',   'treatment'),
    ('user_e', '2024-01-15 13:06:00', 'tutorial_start',  'treatment'),
    ('user_e', '2024-01-15 13:10:00', 'tutorial_end',    'treatment'),
    ('user_e', '2024-01-15 13:12:00', 'first_action',    'treatment'),

    -- Treatment group: User F - drops off after tutorial
    ('user_f', '2024-01-15 14:00:00', 'signup',          'treatment'),
    ('user_f', '2024-01-15 14:02:00', 'profile_setup',   'treatment'),
    ('user_f', '2024-01-15 14:04:00', 'tutorial_start',  'treatment'),
    ('user_f', '2024-01-15 14:07:00', 'tutorial_end',    'treatment');
```

### Analytical Query: Multi-Dimensional Comparison

```sql
-- Dimension 1: Funnel depth (how far into onboarding?)
WITH funnel_analysis AS (
    SELECT
        user_id,
        test_group,
        window_funnel(
            INTERVAL '2 hours',
            event_time,
            event_type = 'signup',
            event_type = 'profile_setup',
            event_type = 'tutorial_start',
            event_type = 'tutorial_end',
            event_type = 'first_action'
        ) AS funnel_step
    FROM ab_test_events
    GROUP BY user_id, test_group
),
-- Dimension 2: Did the full sequence complete within 30 minutes?
sequence_analysis AS (
    SELECT
        user_id,
        test_group,
        sequence_match(
            '(?1).*(?t<=1800)(?2).*(?t<=1800)(?3).*(?t<=1800)(?4).*(?t<=1800)(?5)',
            event_time,
            event_type = 'signup',
            event_type = 'profile_setup',
            event_type = 'tutorial_start',
            event_type = 'tutorial_end',
            event_type = 'first_action'
        ) AS completed_within_30min
    FROM ab_test_events
    GROUP BY user_id, test_group
),
-- Dimension 3: Event timestamps for completed users
timing_analysis AS (
    SELECT
        user_id,
        test_group,
        sequence_match_events(
            '(?1).*(?2).*(?3).*(?4).*(?5)',
            event_time,
            event_type = 'signup',
            event_type = 'profile_setup',
            event_type = 'tutorial_start',
            event_type = 'tutorial_end',
            event_type = 'first_action'
        ) AS step_timestamps
    FROM ab_test_events
    GROUP BY user_id, test_group
)
-- Combined report per test group
SELECT
    f.test_group,
    COUNT(*) AS users,
    ROUND(AVG(f.funnel_step), 2) AS avg_funnel_depth,
    SUM(CASE WHEN f.funnel_step = 5 THEN 1 ELSE 0 END) AS completed_onboarding,
    ROUND(100.0 * SUM(CASE WHEN f.funnel_step = 5 THEN 1 ELSE 0 END) /
        COUNT(*), 1) AS completion_rate_pct,
    SUM(CASE WHEN s.completed_within_30min THEN 1 ELSE 0 END)
        AS completed_fast,
    ROUND(100.0 * SUM(CASE WHEN s.completed_within_30min THEN 1 ELSE 0 END) /
        NULLIF(SUM(CASE WHEN f.funnel_step = 5 THEN 1 ELSE 0 END), 0), 1)
        AS fast_completion_rate_pct
FROM funnel_analysis f
JOIN sequence_analysis s ON f.user_id = s.user_id
JOIN timing_analysis t ON f.user_id = t.user_id
GROUP BY f.test_group
ORDER BY f.test_group;
```

### Expected Results

| test_group | users | avg_funnel_depth | completed_onboarding | completion_rate_pct | completed_fast | fast_completion_rate_pct |
|---|---|---|---|---|---|---|
| control | 3 | 3.00 | 1 | 33.3 | 0 | 0.0 |
| treatment | 3 | 4.33 | 2 | 66.7 | 2 | 100.0 |

### Step-by-Step Funnel Comparison

```sql
-- Detailed funnel step distribution per group
WITH user_funnels AS (
    SELECT
        user_id,
        test_group,
        window_funnel(
            INTERVAL '2 hours',
            event_time,
            event_type = 'signup',
            event_type = 'profile_setup',
            event_type = 'tutorial_start',
            event_type = 'tutorial_end',
            event_type = 'first_action'
        ) AS step
    FROM ab_test_events
    GROUP BY user_id, test_group
)
SELECT
    test_group,
    step AS reached_step,
    COUNT(*) AS user_count
FROM user_funnels
GROUP BY test_group, step
ORDER BY test_group, step;
```

| test_group | reached_step | user_count |
|---|---|---|
| control | 1 | 1 |
| control | 3 | 1 |
| control | 5 | 1 |
| treatment | 4 | 1 |
| treatment | 5 | 2 |

### Interpretation

The treatment group shows stronger behavioral metrics across all dimensions:

- **Funnel depth**: Average 4.33 steps vs. 3.00 -- users progress further
  through onboarding.
- **Completion rate**: 66.7% vs. 33.3% -- twice as many users complete the
  full onboarding.
- **Speed**: All treatment completers finished within 30 minutes between each
  step. The single control completer (user A) took 60 minutes overall, with a
  25-minute gap between profile setup and tutorial start.
- **Drop-off pattern**: In the control group, one user (C) dropped off at step
  1 (signup only), suggesting the immediate post-signup experience needs work.
  In the treatment group, the only non-completing user (F) still reached step 4
  (tutorial_end), indicating better engagement even among non-completers.

The `sequence_match` with time constraints (`(?t<=1800)`) specifically identifies
users who moved through each step within 30 minutes, measuring momentum rather
than just completion. The `sequence_match_events` output can be used for
further analysis of time-between-steps to identify specific bottleneck transitions.
