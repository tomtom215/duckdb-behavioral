-- =============================================================================
-- Example 04: Event Sequence Pattern Matching
-- Detect behavioral patterns using sequence_match, sequence_count, and
-- sequence_match_events.
-- =============================================================================

CREATE OR REPLACE TABLE events AS SELECT * FROM (VALUES
  -- User 1: view → cart → purchase (happy path)
  (1, TIMESTAMP '2024-01-15 10:00:00', 'page_view'),
  (1, TIMESTAMP '2024-01-15 10:05:00', 'add_to_cart'),
  (1, TIMESTAMP '2024-01-15 10:15:00', 'purchase'),
  -- User 2: view → cart → view → cart → purchase (browsing cycle)
  (2, TIMESTAMP '2024-01-15 11:00:00', 'page_view'),
  (2, TIMESTAMP '2024-01-15 11:05:00', 'add_to_cart'),
  (2, TIMESTAMP '2024-01-15 11:10:00', 'page_view'),
  (2, TIMESTAMP '2024-01-15 11:15:00', 'add_to_cart'),
  (2, TIMESTAMP '2024-01-15 11:20:00', 'purchase'),
  -- User 3: view → view → view (no conversion)
  (3, TIMESTAMP '2024-01-15 14:00:00', 'page_view'),
  (3, TIMESTAMP '2024-01-15 14:10:00', 'page_view'),
  (3, TIMESTAMP '2024-01-15 14:20:00', 'page_view'),
  -- User 4: view → purchase after 2 hours (outside time constraint)
  (4, TIMESTAMP '2024-01-15 09:00:00', 'page_view'),
  (4, TIMESTAMP '2024-01-15 11:30:00', 'purchase')
) AS t(user_id, event_time, event_type);

-- Pattern 1: Did user view then purchase (any gap)?
SELECT '--- View Then Purchase ---' as section;
SELECT user_id,
  sequence_match('(?1).*(?2)', event_time,
    event_type = 'page_view',
    event_type = 'purchase'
  ) as viewed_then_purchased
FROM events GROUP BY user_id ORDER BY user_id;

-- Pattern 2: View then purchase within 1 hour
SELECT '--- Within 1 Hour ---' as section;
SELECT user_id,
  sequence_match('(?1).*(?t<=3600)(?2)', event_time,
    event_type = 'page_view',
    event_type = 'purchase'
  ) as converted_within_hour
FROM events GROUP BY user_id ORDER BY user_id;

-- Pattern 3: Count view→cart cycles
SELECT '--- Browse-Cart Cycles ---' as section;
SELECT user_id,
  sequence_count('(?1).*(?2)', event_time,
    event_type = 'page_view',
    event_type = 'add_to_cart'
  ) as browse_cart_cycles
FROM events GROUP BY user_id ORDER BY user_id;

-- Pattern 4: Get timestamps of matched 3-step sequence
SELECT '--- Matched Timestamps ---' as section;
SELECT user_id,
  sequence_match_events('(?1).*(?2).*(?3)', event_time,
    event_type = 'page_view',
    event_type = 'add_to_cart',
    event_type = 'purchase'
  ) as step_timestamps
FROM events GROUP BY user_id ORDER BY user_id;

DROP TABLE events;
