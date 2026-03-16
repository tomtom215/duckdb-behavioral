-- =============================================================================
-- Example 05: User Flow Analysis
-- Discover navigation patterns using sequence_next_node: what users do
-- after/before specific page sequences.
-- =============================================================================

CREATE OR REPLACE TABLE navigation AS SELECT * FROM (VALUES
  -- User A: Home → Product → Cart → Checkout → Confirmation
  ('alice', TIMESTAMP '2024-01-15 10:00:00', 'Home'),
  ('alice', TIMESTAMP '2024-01-15 10:02:00', 'Product'),
  ('alice', TIMESTAMP '2024-01-15 10:05:00', 'Cart'),
  ('alice', TIMESTAMP '2024-01-15 10:08:00', 'Checkout'),
  ('alice', TIMESTAMP '2024-01-15 10:10:00', 'Confirmation'),
  -- User B: Home → Product → Product → Home (browsing, no conversion)
  ('bob',   TIMESTAMP '2024-01-15 11:00:00', 'Home'),
  ('bob',   TIMESTAMP '2024-01-15 11:03:00', 'Product'),
  ('bob',   TIMESTAMP '2024-01-15 11:07:00', 'Product'),
  ('bob',   TIMESTAMP '2024-01-15 11:10:00', 'Home'),
  -- User C: Home → Product → Cart → Home (cart abandonment)
  ('carol', TIMESTAMP '2024-01-15 14:00:00', 'Home'),
  ('carol', TIMESTAMP '2024-01-15 14:05:00', 'Product'),
  ('carol', TIMESTAMP '2024-01-15 14:08:00', 'Cart'),
  ('carol', TIMESTAMP '2024-01-15 14:15:00', 'Home'),
  -- User D: Home → Product → Checkout (skipped cart)
  ('dave',  TIMESTAMP '2024-01-15 15:00:00', 'Home'),
  ('dave',  TIMESTAMP '2024-01-15 15:02:00', 'Product'),
  ('dave',  TIMESTAMP '2024-01-15 15:05:00', 'Checkout')
) AS t(user_id, event_time, page);

-- Forward: What page after Home → Product?
SELECT '--- Forward Flow: After Home → Product ---' as section;
SELECT user_id,
  sequence_next_node('forward', 'first_match', event_time, page,
    page = 'Home', page = 'Home', page = 'Product'
  ) as next_page
FROM navigation GROUP BY user_id ORDER BY user_id;

-- Aggregate forward flow distribution
SELECT '--- Forward Flow Distribution ---' as section;
WITH flows AS (
  SELECT
    sequence_next_node('forward', 'first_match', event_time, page,
      page = 'Home', page = 'Home', page = 'Product'
    ) as next_page
  FROM navigation GROUP BY user_id
)
SELECT
  COALESCE(next_page, '(end)') as next_page,
  COUNT(*) as users,
  ROUND(100.0 * COUNT(*) / SUM(COUNT(*)) OVER (), 1) as pct
FROM flows
GROUP BY next_page
ORDER BY users DESC;

-- Backward: What page before Checkout?
SELECT '--- Backward Flow: Before Checkout ---' as section;
SELECT user_id,
  sequence_next_node('backward', 'first_match', event_time, page,
    page = 'Checkout', page = 'Checkout'
  ) as page_before_checkout
FROM navigation
WHERE user_id IN ('alice', 'dave')
GROUP BY user_id
ORDER BY user_id;

DROP TABLE navigation;
