-- =============================================================================
-- Example 02: Conversion Funnel Analysis
-- Track user progress through a purchase funnel and generate a drop-off report.
-- =============================================================================

CREATE OR REPLACE TABLE events AS SELECT * FROM (VALUES
  -- User 1: completes full funnel
  (1, TIMESTAMP '2024-01-15 10:00:00', 'page_view'),
  (1, TIMESTAMP '2024-01-15 10:05:00', 'add_to_cart'),
  (1, TIMESTAMP '2024-01-15 10:12:00', 'checkout'),
  (1, TIMESTAMP '2024-01-15 10:15:00', 'purchase'),
  -- User 2: adds to cart but abandons
  (2, TIMESTAMP '2024-01-15 11:00:00', 'page_view'),
  (2, TIMESTAMP '2024-01-15 11:03:00', 'add_to_cart'),
  (2, TIMESTAMP '2024-01-15 11:30:00', 'page_view'),
  -- User 3: views only
  (3, TIMESTAMP '2024-01-15 14:00:00', 'page_view'),
  (3, TIMESTAMP '2024-01-15 14:10:00', 'page_view'),
  -- User 4: funnel outside time window (>1 hour from first step)
  (4, TIMESTAMP '2024-01-15 09:00:00', 'page_view'),
  (4, TIMESTAMP '2024-01-15 09:10:00', 'add_to_cart'),
  (4, TIMESTAMP '2024-01-15 10:30:00', 'checkout'),
  (4, TIMESTAMP '2024-01-15 10:35:00', 'purchase'),
  -- User 5: completes quickly
  (5, TIMESTAMP '2024-01-15 16:00:00', 'page_view'),
  (5, TIMESTAMP '2024-01-15 16:01:00', 'add_to_cart'),
  (5, TIMESTAMP '2024-01-15 16:02:00', 'checkout'),
  (5, TIMESTAMP '2024-01-15 16:03:00', 'purchase')
) AS t(user_id, event_time, event_type);

-- Step 1: Per-user funnel progress
SELECT '--- Per-User Funnel ---' as section;
SELECT user_id,
  window_funnel(INTERVAL '1 hour', event_time,
    event_type = 'page_view',
    event_type = 'add_to_cart',
    event_type = 'checkout',
    event_type = 'purchase'
  ) as furthest_step
FROM events
GROUP BY user_id
ORDER BY user_id;

-- Step 2: Drop-off report
SELECT '--- Drop-Off Report ---' as section;
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
  ROUND(100.0 * COUNT(*) / SUM(COUNT(*)) OVER (), 1) as pct_of_total
FROM funnels
GROUP BY step
ORDER BY step;

-- Step 3: With strict_increase mode
SELECT '--- Strict Increase Mode ---' as section;
SELECT user_id,
  window_funnel(INTERVAL '1 hour', 'strict_increase', event_time,
    event_type = 'page_view',
    event_type = 'add_to_cart',
    event_type = 'checkout',
    event_type = 'purchase'
  ) as step_strict
FROM events
GROUP BY user_id
ORDER BY user_id;

DROP TABLE events;
