-- =============================================================================
-- Example 01: Session Analysis
-- Assign session IDs based on 30-minute inactivity gaps, then compute
-- per-session and per-user metrics.
-- =============================================================================

-- Sample data: web page views with timestamps
CREATE OR REPLACE TABLE page_views AS SELECT * FROM (VALUES
  (1, TIMESTAMP '2024-01-15 10:00:00', '/home'),
  (1, TIMESTAMP '2024-01-15 10:05:00', '/products'),
  (1, TIMESTAMP '2024-01-15 10:08:00', '/product/shoes'),
  (1, TIMESTAMP '2024-01-15 10:25:00', '/cart'),
  -- 2-hour gap → new session
  (1, TIMESTAMP '2024-01-15 12:30:00', '/home'),
  (1, TIMESTAMP '2024-01-15 12:35:00', '/products'),
  -- User 2: single session
  (2, TIMESTAMP '2024-01-15 09:00:00', '/home'),
  (2, TIMESTAMP '2024-01-15 09:10:00', '/about'),
  (2, TIMESTAMP '2024-01-15 09:15:00', '/pricing'),
  (2, TIMESTAMP '2024-01-15 09:20:00', '/signup'),
  -- User 3: three bounce sessions
  (3, TIMESTAMP '2024-01-15 08:00:00', '/home'),
  (3, TIMESTAMP '2024-01-15 08:45:00', '/home'),
  (3, TIMESTAMP '2024-01-15 09:45:00', '/blog/1')
) AS t(user_id, event_time, page_url);

-- Step 1: Assign session IDs
SELECT '--- Session Assignment ---' as section;
SELECT user_id, event_time, page_url,
  sessionize(event_time, INTERVAL '30 minutes') OVER (
    PARTITION BY user_id ORDER BY event_time
  ) as session_id
FROM page_views
ORDER BY user_id, event_time;

-- Step 2: Session-level metrics
SELECT '--- Session Metrics ---' as section;
WITH sessionized AS (
  SELECT user_id, event_time, page_url,
    sessionize(event_time, INTERVAL '30 minutes') OVER (
      PARTITION BY user_id ORDER BY event_time
    ) as session_id
  FROM page_views
)
SELECT
  user_id,
  session_id,
  COUNT(*) as page_views,
  MIN(event_time) as started_at,
  MAX(event_time) as ended_at,
  EXTRACT(EPOCH FROM MAX(event_time) - MIN(event_time)) as duration_sec,
  CASE WHEN COUNT(*) = 1 THEN 'bounce' ELSE 'engaged' END as session_type
FROM sessionized
GROUP BY user_id, session_id
ORDER BY user_id, session_id;

-- Step 3: Per-user summary
SELECT '--- User Summary ---' as section;
WITH sessionized AS (
  SELECT user_id, event_time,
    sessionize(event_time, INTERVAL '30 minutes') OVER (
      PARTITION BY user_id ORDER BY event_time
    ) as session_id
  FROM page_views
),
session_stats AS (
  SELECT user_id, session_id,
    COUNT(*) as pages,
    EXTRACT(EPOCH FROM MAX(event_time) - MIN(event_time)) as duration_sec
  FROM sessionized
  GROUP BY user_id, session_id
)
SELECT
  user_id,
  COUNT(*) as total_sessions,
  ROUND(AVG(pages), 1) as avg_pages_per_session,
  ROUND(AVG(duration_sec), 0) as avg_duration_sec,
  SUM(CASE WHEN pages = 1 THEN 1 ELSE 0 END) as bounce_sessions
FROM session_stats
GROUP BY user_id
ORDER BY user_id;

DROP TABLE page_views;
