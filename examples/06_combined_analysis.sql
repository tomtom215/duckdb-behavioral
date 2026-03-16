-- =============================================================================
-- Example 06: Combined Multi-Function Analysis
-- Demonstrates using multiple behavioral functions together for comprehensive
-- user behavior understanding.
-- =============================================================================

CREATE OR REPLACE TABLE events AS SELECT * FROM (VALUES
  -- User 1: Power user - multiple sessions, completes funnel
  (1, TIMESTAMP '2024-01-15 09:00:00', 'page_view',   'Home'),
  (1, TIMESTAMP '2024-01-15 09:05:00', 'page_view',   'Product'),
  (1, TIMESTAMP '2024-01-15 09:10:00', 'add_to_cart',  'Product'),
  (1, TIMESTAMP '2024-01-15 09:15:00', 'checkout',     'Cart'),
  (1, TIMESTAMP '2024-01-15 09:18:00', 'purchase',     'Checkout'),
  -- Same user, second session
  (1, TIMESTAMP '2024-01-15 14:00:00', 'page_view',   'Home'),
  (1, TIMESTAMP '2024-01-15 14:10:00', 'page_view',   'Product'),
  (1, TIMESTAMP '2024-01-15 14:15:00', 'add_to_cart',  'Product'),
  (1, TIMESTAMP '2024-01-15 14:20:00', 'purchase',     'Checkout'),

  -- User 2: Casual browser, no purchase
  (2, TIMESTAMP '2024-01-15 10:00:00', 'page_view',   'Home'),
  (2, TIMESTAMP '2024-01-15 10:05:00', 'page_view',   'Product'),
  (2, TIMESTAMP '2024-01-15 10:08:00', 'page_view',   'Product'),
  (2, TIMESTAMP '2024-01-15 10:12:00', 'page_view',   'Home'),

  -- User 3: One session, slow checkout
  (3, TIMESTAMP '2024-01-15 11:00:00', 'page_view',   'Home'),
  (3, TIMESTAMP '2024-01-15 11:30:00', 'page_view',   'Product'),
  (3, TIMESTAMP '2024-01-15 11:45:00', 'add_to_cart',  'Product'),
  (3, TIMESTAMP '2024-01-15 12:30:00', 'checkout',     'Cart'),
  (3, TIMESTAMP '2024-01-15 12:35:00', 'purchase',     'Checkout')
) AS t(user_id, event_time, event_type, page);

-- Analysis 1: Session assignment
SELECT '--- Sessions ---' as section;
WITH sessionized AS (
  SELECT user_id, event_time, event_type, page,
    sessionize(event_time, INTERVAL '30 minutes') OVER (
      PARTITION BY user_id ORDER BY event_time
    ) as session_id
  FROM events
)
SELECT user_id,
  COUNT(DISTINCT session_id) as total_sessions,
  COUNT(*) as total_events
FROM sessionized
GROUP BY user_id
ORDER BY user_id;

-- Analysis 2: Funnel progress
SELECT '--- Funnel Progress ---' as section;
SELECT user_id,
  window_funnel(INTERVAL '2 hours', event_time,
    event_type = 'page_view',
    event_type = 'add_to_cart',
    event_type = 'checkout',
    event_type = 'purchase'
  ) as furthest_step
FROM events
GROUP BY user_id
ORDER BY user_id;

-- Analysis 3: Pattern detection
SELECT '--- Behavioral Patterns ---' as section;
SELECT user_id,
  sequence_match('(?1).*(?2)', event_time,
    event_type = 'page_view', event_type = 'purchase'
  ) as viewed_then_bought,
  sequence_match('(?1).*(?t<=3600)(?2)', event_time,
    event_type = 'page_view', event_type = 'purchase'
  ) as bought_within_hour,
  sequence_count('(?1).*(?2)', event_time,
    event_type = 'page_view', event_type = 'add_to_cart'
  ) as browse_cart_cycles
FROM events
GROUP BY user_id
ORDER BY user_id;

-- Analysis 4: User flow
SELECT '--- Next Page After Home → Product ---' as section;
SELECT user_id,
  sequence_next_node('forward', 'first_match', event_time, page,
    page = 'Home', page = 'Home', page = 'Product'
  ) as next_page
FROM events
GROUP BY user_id
ORDER BY user_id;

-- Analysis 5: Combined user scorecard
SELECT '--- User Behavioral Scorecard ---' as section;
SELECT
  user_id,
  window_funnel(INTERVAL '2 hours', event_time,
    event_type = 'page_view', event_type = 'add_to_cart',
    event_type = 'checkout', event_type = 'purchase'
  ) as funnel_depth,
  sequence_count('(?1).*(?2)', event_time,
    event_type = 'page_view', event_type = 'add_to_cart'
  ) as engagement_cycles,
  sequence_match('(?1).*(?t<=1800)(?2)', event_time,
    event_type = 'page_view', event_type = 'purchase'
  ) as fast_converter,
  CASE
    WHEN window_funnel(INTERVAL '2 hours', event_time,
      event_type = 'page_view', event_type = 'add_to_cart',
      event_type = 'checkout', event_type = 'purchase') = 4
      AND sequence_count('(?1).*(?2)', event_time,
        event_type = 'page_view', event_type = 'add_to_cart') >= 2
    THEN 'power_user'
    WHEN window_funnel(INTERVAL '2 hours', event_time,
      event_type = 'page_view', event_type = 'add_to_cart',
      event_type = 'checkout', event_type = 'purchase') >= 3
    THEN 'converter'
    WHEN window_funnel(INTERVAL '2 hours', event_time,
      event_type = 'page_view', event_type = 'add_to_cart',
      event_type = 'checkout', event_type = 'purchase') >= 2
    THEN 'engaged'
    ELSE 'browser'
  END as user_segment
FROM events
GROUP BY user_id
ORDER BY user_id;

DROP TABLE events;
