-- =============================================================================
-- Example 03: Weekly Cohort Retention
-- Measure week-over-week retention for signup cohorts.
-- =============================================================================

CREATE OR REPLACE TABLE activity AS SELECT * FROM (VALUES
  -- User A: active weeks 0, 1, 3
  ('alice', DATE '2024-01-01', DATE '2024-01-01'),
  ('alice', DATE '2024-01-09', DATE '2024-01-01'),
  ('alice', DATE '2024-01-22', DATE '2024-01-01'),
  -- User B: active weeks 0, 1
  ('bob',   DATE '2024-01-02', DATE '2024-01-01'),
  ('bob',   DATE '2024-01-08', DATE '2024-01-01'),
  -- User C: active week 0 only
  ('carol', DATE '2024-01-03', DATE '2024-01-01'),
  -- User D: active weeks 0, 1, 2, 3
  ('dave',  DATE '2024-01-01', DATE '2024-01-01'),
  ('dave',  DATE '2024-01-10', DATE '2024-01-01'),
  ('dave',  DATE '2024-01-15', DATE '2024-01-01'),
  ('dave',  DATE '2024-01-23', DATE '2024-01-01'),
  -- User E: different cohort (Jan 8), active weeks 0, 2
  ('eve',   DATE '2024-01-08', DATE '2024-01-08'),
  ('eve',   DATE '2024-01-22', DATE '2024-01-08')
) AS t(user_id, activity_date, cohort_week);

-- Per-user retention arrays
SELECT '--- Per-User Retention ---' as section;
SELECT user_id, cohort_week,
  retention(
    activity_date >= cohort_week AND activity_date < cohort_week + INTERVAL '7 days',
    activity_date >= cohort_week + INTERVAL '7 days' AND activity_date < cohort_week + INTERVAL '14 days',
    activity_date >= cohort_week + INTERVAL '14 days' AND activity_date < cohort_week + INTERVAL '21 days',
    activity_date >= cohort_week + INTERVAL '21 days' AND activity_date < cohort_week + INTERVAL '28 days'
  ) as retained
FROM activity
GROUP BY user_id, cohort_week
ORDER BY cohort_week, user_id;

-- Cohort retention report
SELECT '--- Cohort Retention Report ---' as section;
WITH user_retention AS (
  SELECT user_id, cohort_week,
    retention(
      activity_date >= cohort_week AND activity_date < cohort_week + INTERVAL '7 days',
      activity_date >= cohort_week + INTERVAL '7 days' AND activity_date < cohort_week + INTERVAL '14 days',
      activity_date >= cohort_week + INTERVAL '14 days' AND activity_date < cohort_week + INTERVAL '21 days',
      activity_date >= cohort_week + INTERVAL '21 days' AND activity_date < cohort_week + INTERVAL '28 days'
    ) as r
  FROM activity GROUP BY user_id, cohort_week
)
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
FROM user_retention
GROUP BY cohort_week
ORDER BY cohort_week;

DROP TABLE activity;
