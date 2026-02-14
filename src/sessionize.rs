// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! `sessionize` — Window function that assigns monotonically increasing session IDs.
//!
//! A new session starts when the gap between consecutive rows (ordered by timestamp)
//! exceeds a configurable threshold.
//!
//! # SQL Usage
//!
//! ```sql
//! SELECT user_id, event_time,
//!   sessionize(event_time, INTERVAL '30 minutes') OVER (
//!     PARTITION BY user_id ORDER BY event_time
//!   ) as session_id
//! FROM events
//! ```
//!
//! # Implementation
//!
//! Implemented as a `DuckDB` aggregate function that works as a window function
//! via `DuckDB`'s segment tree machinery. The state tracks:
//!
//! - `first_ts`: Earliest timestamp in this segment
//! - `last_ts`: Latest timestamp in this segment
//! - `session_count`: Number of sessions (gap breaks + 1) in this segment
//! - `threshold_us`: Gap threshold in microseconds
//!
//! The `combine` operation is O(1) — it merges two adjacent segments by checking
//! if the gap between `left.last_ts` and `right.first_ts` exceeds the threshold.
//! This enables O(n log n) windowed evaluation via segment trees.

/// State for the sessionize aggregate.
///
/// Designed for O(1) `combine()` with `DuckDB`'s segment tree windowing.
/// Each state represents a contiguous range of ordered timestamps and
/// tracks the number of session boundaries within that range.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SessionizeState {
    /// Earliest timestamp in this segment (microseconds since epoch).
    pub first_ts: Option<i64>,
    /// Latest timestamp in this segment (microseconds since epoch).
    pub last_ts: Option<i64>,
    /// Number of sessions in this segment. A segment with one event has
    /// `session_count = 1`. Each gap exceeding the threshold adds 1.
    pub session_count: i64,
    /// Gap threshold in microseconds. Read from the INTERVAL parameter
    /// during the first `update()` call.
    pub threshold_us: i64,
}

impl SessionizeState {
    /// Creates a new empty state.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            first_ts: None,
            last_ts: None,
            session_count: 0,
            threshold_us: 0,
        }
    }

    /// Updates the state with a single timestamp.
    ///
    /// If this is the first timestamp, initializes the state.
    /// If the gap from the previous `last_ts` exceeds the threshold,
    /// increments the session count.
    pub fn update(&mut self, timestamp_us: i64) {
        match self.last_ts {
            None => {
                self.first_ts = Some(timestamp_us);
                self.last_ts = Some(timestamp_us);
                self.session_count = 1;
            }
            Some(prev) => {
                if timestamp_us - prev > self.threshold_us {
                    self.session_count += 1;
                }
                if timestamp_us > prev {
                    self.last_ts = Some(timestamp_us);
                }
                if let Some(first) = self.first_ts {
                    if timestamp_us < first {
                        self.first_ts = Some(timestamp_us);
                    }
                }
            }
        }
    }

    /// Combines two states representing adjacent segments.
    ///
    /// The key insight: when merging two ordered segments `[a, b]` and `[c, d]`,
    /// we only need to check if the gap between `b` (`left.last_ts`) and `c`
    /// (`right.first_ts`) creates a new session boundary.
    ///
    /// This is O(1) per combine, enabling O(n log n) segment tree evaluation.
    #[must_use]
    pub fn combine(&self, other: &Self) -> Self {
        match (self.first_ts, other.first_ts) {
            (None, _) => other.clone(),
            (_, None) => self.clone(),
            (Some(_), Some(other_first)) => {
                let boundary = self.last_ts.map_or(0, |self_last| {
                    i64::from(other_first - self_last > self.threshold_us)
                });

                Self {
                    first_ts: self.first_ts,
                    last_ts: other.last_ts.or(self.last_ts),
                    session_count: self.session_count + other.session_count + boundary,
                    threshold_us: self.threshold_us,
                }
            }
        }
    }

    /// Returns the session ID for the finalize step.
    ///
    /// The session count IS the session ID when evaluated over the frame
    /// `ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW`.
    #[must_use]
    pub const fn finalize(&self) -> i64 {
        self.session_count
    }
}

impl Default for SessionizeState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_state() {
        let state = SessionizeState::new();
        assert_eq!(state.finalize(), 0);
        assert!(state.first_ts.is_none());
    }

    #[test]
    fn test_single_event() {
        let mut state = SessionizeState::new();
        state.threshold_us = 1_800_000_000; // 30 minutes
        state.update(1_000_000);
        assert_eq!(state.finalize(), 1);
    }

    #[test]
    fn test_two_events_same_session() {
        let mut state = SessionizeState::new();
        state.threshold_us = 1_800_000_000; // 30 minutes
        state.update(1_000_000); // t=1s
        state.update(600_000_000); // t=600s (10 min gap, within 30 min)
        assert_eq!(state.finalize(), 1);
    }

    #[test]
    fn test_two_events_different_sessions() {
        let mut state = SessionizeState::new();
        state.threshold_us = 1_800_000_000; // 30 minutes
        state.update(0); // t=0
        state.update(2_000_000_000); // t=2000s (33+ min gap, exceeds 30 min)
        assert_eq!(state.finalize(), 2);
    }

    #[test]
    fn test_multiple_sessions() {
        let mut state = SessionizeState::new();
        state.threshold_us = 60_000_000; // 1 minute
        state.update(0); // t=0
        state.update(30_000_000); // t=30s (same session)
        state.update(120_000_000); // t=120s (new session, 90s gap > 60s)
        state.update(150_000_000); // t=150s (same session)
        state.update(300_000_000); // t=300s (new session, 150s gap > 60s)
        assert_eq!(state.finalize(), 3);
    }

    #[test]
    fn test_gap_exactly_at_threshold() {
        let mut state = SessionizeState::new();
        state.threshold_us = 60_000_000; // 1 minute
        state.update(0);
        state.update(60_000_000); // exactly 60s gap = threshold
                                  // Gap must EXCEED threshold, not equal it
        assert_eq!(state.finalize(), 1);
    }

    #[test]
    fn test_gap_just_over_threshold() {
        let mut state = SessionizeState::new();
        state.threshold_us = 60_000_000; // 1 minute
        state.update(0);
        state.update(60_000_001); // 1 microsecond over threshold
        assert_eq!(state.finalize(), 2);
    }

    #[test]
    fn test_combine_empty_states() {
        let a = SessionizeState::new();
        let b = SessionizeState::new();
        let combined = a.combine(&b);
        assert_eq!(combined.finalize(), 0);
    }

    #[test]
    fn test_combine_left_empty() {
        let a = SessionizeState::new();
        let mut b = SessionizeState::new();
        b.threshold_us = 1_000_000;
        b.update(100);
        let combined = a.combine(&b);
        assert_eq!(combined.finalize(), 1);
    }

    #[test]
    fn test_combine_right_empty() {
        let mut a = SessionizeState::new();
        a.threshold_us = 1_000_000;
        a.update(100);
        let b = SessionizeState::new();
        let combined = a.combine(&b);
        assert_eq!(combined.finalize(), 1);
    }

    #[test]
    fn test_combine_no_boundary() {
        let mut a = SessionizeState::new();
        a.threshold_us = 1_000_000; // 1 second
        a.update(0);

        let mut b = SessionizeState::new();
        b.threshold_us = 1_000_000;
        b.update(500_000); // 0.5s gap, within threshold

        let combined = a.combine(&b);
        assert_eq!(combined.finalize(), 2); // 1 + 1 + 0 boundary
                                            // Note: SessionizeState counts sessions per segment, so combining two
                                            // single-event segments yields 2 even though they're in the same session.
                                            // The revised SessionizeBoundaryState below fixes this.
    }
}

/// Revised sessionize state tracking session BOUNDARIES (gaps exceeding threshold).
///
/// `session_boundaries` counts the number of gaps within this segment that exceed
/// the threshold. The total number of sessions is `boundaries + 1` for non-empty data.
///
/// The `current_row_null` flag tracks whether the rightmost row in this segment
/// had a `NULL` timestamp. In `DuckDB`'s segment tree window evaluation, the rightmost
/// leaf in the frame is the current row. When this flag is set, `finalize` signals
/// that the output should be `NULL` (handled in the FFI layer).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SessionizeBoundaryState {
    /// Earliest timestamp in this segment (microseconds since epoch).
    pub first_ts: Option<i64>,
    /// Latest timestamp in this segment (microseconds since epoch).
    pub last_ts: Option<i64>,
    /// Number of session BOUNDARIES (gaps exceeding threshold) in this segment.
    pub boundaries: i64,
    /// Gap threshold in microseconds.
    pub threshold_us: i64,
    /// Whether the rightmost row in this segment had a `NULL` timestamp.
    /// Used by the FFI finalize to emit `NULL` for `NULL`-timestamp rows.
    pub current_row_null: bool,
}

impl SessionizeBoundaryState {
    /// Creates a new empty state.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            first_ts: None,
            last_ts: None,
            boundaries: 0,
            threshold_us: 0,
            current_row_null: false,
        }
    }

    /// Marks this state as representing a `NULL`-timestamp row.
    ///
    /// Called from the FFI layer when the current row's timestamp is `NULL`.
    /// The flag propagates through `combine` so that the rightmost segment's
    /// `NULL` status is preserved.
    #[inline]
    pub fn mark_null_row(&mut self) {
        self.current_row_null = true;
    }

    /// Updates the state with a single non-`NULL` timestamp.
    #[inline]
    pub fn update(&mut self, timestamp_us: i64) {
        self.current_row_null = false;
        match self.last_ts {
            None => {
                self.first_ts = Some(timestamp_us);
                self.last_ts = Some(timestamp_us);
            }
            Some(prev) => {
                if timestamp_us - prev > self.threshold_us {
                    self.boundaries += 1;
                }
                if timestamp_us > prev {
                    self.last_ts = Some(timestamp_us);
                }
                if let Some(first) = self.first_ts {
                    if timestamp_us < first {
                        self.first_ts = Some(timestamp_us);
                    }
                }
            }
        }
    }

    /// Combines two states representing adjacent ordered segments.
    ///
    /// O(1) operation: only checks the cross-segment boundary.
    ///
    /// The `current_row_null` flag always propagates from `other` (the right/later
    /// segment), because in `ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW` the
    /// current row is the rightmost element of the frame.
    #[must_use]
    #[inline]
    pub fn combine(&self, other: &Self) -> Self {
        match (self.first_ts, other.first_ts) {
            (None, _) => other.clone(),
            (_, None) => {
                // other has no timestamp data — propagate other's null flag
                let mut result = self.clone();
                result.current_row_null = other.current_row_null;
                result
            }
            (Some(_), Some(other_first)) => {
                let cross_boundary = self.last_ts.map_or(0, |self_last| {
                    i64::from(other_first - self_last > self.threshold_us)
                });

                Self {
                    first_ts: self.first_ts,
                    last_ts: other.last_ts.or(self.last_ts),
                    boundaries: self.boundaries + other.boundaries + cross_boundary,
                    threshold_us: self.threshold_us,
                    current_row_null: other.current_row_null,
                }
            }
        }
    }

    /// Returns the session ID: boundaries + 1 for non-empty data, 0 for empty.
    #[must_use]
    pub const fn finalize(&self) -> i64 {
        if self.first_ts.is_some() {
            self.boundaries + 1
        } else {
            0
        }
    }
}

impl Default for SessionizeBoundaryState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod boundary_tests {
    use super::*;

    #[test]
    fn test_empty_state() {
        let state = SessionizeBoundaryState::new();
        assert_eq!(state.finalize(), 0);
    }

    #[test]
    fn test_single_event() {
        let mut state = SessionizeBoundaryState::new();
        state.threshold_us = 1_800_000_000;
        state.update(1_000_000);
        assert_eq!(state.finalize(), 1);
    }

    #[test]
    fn test_two_events_same_session() {
        let mut state = SessionizeBoundaryState::new();
        state.threshold_us = 1_800_000_000; // 30 min
        state.update(0);
        state.update(600_000_000); // 10 min gap
        assert_eq!(state.finalize(), 1);
    }

    #[test]
    fn test_two_events_different_sessions() {
        let mut state = SessionizeBoundaryState::new();
        state.threshold_us = 1_800_000_000; // 30 min
        state.update(0);
        state.update(2_000_000_000); // 33+ min gap
        assert_eq!(state.finalize(), 2);
    }

    #[test]
    fn test_three_sessions() {
        let mut state = SessionizeBoundaryState::new();
        state.threshold_us = 60_000_000; // 1 min
        state.update(0);
        state.update(30_000_000); // same session
        state.update(120_000_000); // new session (90s > 60s)
        state.update(150_000_000); // same session
        state.update(300_000_000); // new session (150s > 60s)
        assert_eq!(state.finalize(), 3);
    }

    #[test]
    fn test_gap_exactly_at_threshold() {
        let mut state = SessionizeBoundaryState::new();
        state.threshold_us = 60_000_000;
        state.update(0);
        state.update(60_000_000); // exactly 60s = threshold, NOT exceeded
        assert_eq!(state.finalize(), 1);
    }

    #[test]
    fn test_gap_just_over_threshold() {
        let mut state = SessionizeBoundaryState::new();
        state.threshold_us = 60_000_000;
        state.update(0);
        state.update(60_000_001); // 1us over threshold
        assert_eq!(state.finalize(), 2);
    }

    #[test]
    fn test_combine_both_empty() {
        let a = SessionizeBoundaryState::new();
        let b = SessionizeBoundaryState::new();
        assert_eq!(a.combine(&b).finalize(), 0);
    }

    #[test]
    fn test_combine_left_empty() {
        let a = SessionizeBoundaryState::new();
        let mut b = SessionizeBoundaryState::new();
        b.threshold_us = 1_000_000;
        b.update(100);
        assert_eq!(a.combine(&b).finalize(), 1);
    }

    #[test]
    fn test_combine_no_boundary() {
        let mut a = SessionizeBoundaryState::new();
        a.threshold_us = 1_000_000; // 1 second
        a.update(0);

        let mut b = SessionizeBoundaryState::new();
        b.threshold_us = 1_000_000;
        b.update(500_000); // 0.5s gap, within threshold

        let combined = a.combine(&b);
        // boundaries: 0 + 0 + 0 (no cross boundary) = 0
        // sessions: 0 + 1 = 1
        assert_eq!(combined.finalize(), 1);
    }

    #[test]
    fn test_combine_with_boundary() {
        let mut a = SessionizeBoundaryState::new();
        a.threshold_us = 1_000_000; // 1 second
        a.update(0);

        let mut b = SessionizeBoundaryState::new();
        b.threshold_us = 1_000_000;
        b.update(2_000_000); // 2s gap, exceeds threshold

        let combined = a.combine(&b);
        // boundaries: 0 + 0 + 1 (cross boundary) = 1
        // sessions: 1 + 1 = 2
        assert_eq!(combined.finalize(), 2);
    }

    #[test]
    fn test_combine_complex() {
        // Segment A: events at t=0, t=30s (same session, threshold=60s)
        let mut a = SessionizeBoundaryState::new();
        a.threshold_us = 60_000_000; // 60s
        a.update(0);
        a.update(30_000_000);
        assert_eq!(a.boundaries, 0);

        // Segment B: events at t=120s, t=150s (same session within B)
        let mut b = SessionizeBoundaryState::new();
        b.threshold_us = 60_000_000;
        b.update(120_000_000);
        b.update(150_000_000);
        assert_eq!(b.boundaries, 0);

        // Cross boundary: 120s - 30s = 90s > 60s → new session
        let combined = a.combine(&b);
        assert_eq!(combined.boundaries, 1);
        assert_eq!(combined.finalize(), 2);
        assert_eq!(combined.first_ts, Some(0));
        assert_eq!(combined.last_ts, Some(150_000_000));
    }

    #[test]
    fn test_combine_is_associative() {
        // Events: 0, 30s, 120s, 150s, 300s (threshold 60s)
        // Expected: 3 sessions: [0,30s], [120s,150s], [300s]

        let mut s1 = SessionizeBoundaryState::new();
        s1.threshold_us = 60_000_000;
        s1.update(0);

        let mut s2 = SessionizeBoundaryState::new();
        s2.threshold_us = 60_000_000;
        s2.update(30_000_000);

        let mut s3 = SessionizeBoundaryState::new();
        s3.threshold_us = 60_000_000;
        s3.update(120_000_000);

        let mut s4 = SessionizeBoundaryState::new();
        s4.threshold_us = 60_000_000;
        s4.update(150_000_000);

        let mut s5 = SessionizeBoundaryState::new();
        s5.threshold_us = 60_000_000;
        s5.update(300_000_000);

        // Left-to-right: ((((s1 + s2) + s3) + s4) + s5)
        let lr = s1.combine(&s2).combine(&s3).combine(&s4).combine(&s5);
        assert_eq!(lr.finalize(), 3);

        // Right-to-left: (s1 + (s2 + (s3 + (s4 + s5))))
        let rl = s1.combine(&s2.combine(&s3.combine(&s4.combine(&s5))));
        assert_eq!(rl.finalize(), 3);

        // Tree-style: ((s1 + s2) + (s3 + (s4 + s5)))
        let tree = s1.combine(&s2).combine(&s3.combine(&s4.combine(&s5)));
        assert_eq!(tree.finalize(), 3);
    }

    #[test]
    fn test_threshold_zero() {
        // With threshold=0, every event with ANY gap starts a new session
        let mut state = SessionizeBoundaryState::new();
        state.threshold_us = 0;
        state.update(0);
        state.update(1); // 1us > 0 → new session
        state.update(2); // 1us > 0 → new session
        assert_eq!(state.finalize(), 3);
    }

    #[test]
    fn test_threshold_zero_same_timestamp() {
        // With threshold=0, same timestamps do NOT create new sessions (0 is not > 0)
        let mut state = SessionizeBoundaryState::new();
        state.threshold_us = 0;
        state.update(100);
        state.update(100);
        state.update(100);
        assert_eq!(state.finalize(), 1);
    }

    #[test]
    fn test_duplicate_timestamps() {
        // Multiple events at the same timestamp should not create new sessions
        let mut state = SessionizeBoundaryState::new();
        state.threshold_us = 60_000_000; // 1 minute
        state.update(1_000_000);
        state.update(1_000_000);
        state.update(1_000_000);
        assert_eq!(state.finalize(), 1);
    }

    #[test]
    fn test_duplicate_timestamps_then_gap() {
        let mut state = SessionizeBoundaryState::new();
        state.threshold_us = 60_000_000; // 1 minute
        state.update(0);
        state.update(0);
        state.update(0);
        state.update(120_000_000); // 2 minutes later → new session
        assert_eq!(state.finalize(), 2);
    }

    #[test]
    fn test_combine_threshold_zero_boundary() {
        let mut a = SessionizeBoundaryState::new();
        a.threshold_us = 0;
        a.update(0);

        let mut b = SessionizeBoundaryState::new();
        b.threshold_us = 0;
        b.update(0); // same timestamp, no boundary

        let combined = a.combine(&b);
        assert_eq!(combined.finalize(), 1);
    }

    #[test]
    fn test_combine_threshold_zero_with_gap() {
        let mut a = SessionizeBoundaryState::new();
        a.threshold_us = 0;
        a.update(0);

        let mut b = SessionizeBoundaryState::new();
        b.threshold_us = 0;
        b.update(1); // 1us > 0 → boundary

        let combined = a.combine(&b);
        assert_eq!(combined.finalize(), 2);
    }

    #[test]
    fn test_large_timestamps() {
        // Test with timestamps near i64::MAX to ensure no overflow in comparisons
        let mut state = SessionizeBoundaryState::new();
        state.threshold_us = 1_000;
        state.update(i64::MAX - 2000);
        state.update(i64::MAX - 1000);
        assert_eq!(state.finalize(), 1); // 1000 gap, not > threshold
    }

    // --- Mutation testing coverage ---

    #[test]
    fn test_first_ts_tracks_minimum() {
        // Covers: replace < with == / > / <= in first_ts tracking
        let mut state = SessionizeBoundaryState::new();
        state.threshold_us = 1_000_000_000; // large threshold, no boundaries
        state.update(500);
        state.update(100); // earlier timestamp
        state.update(300);
        assert_eq!(state.first_ts, Some(100)); // must be the minimum
        assert_eq!(state.last_ts, Some(500)); // must be the maximum
    }

    #[test]
    fn test_last_ts_tracks_maximum() {
        // Covers: replace > with >= in last_ts tracking
        let mut state = SessionizeBoundaryState::new();
        state.threshold_us = 1_000_000_000;
        state.update(100);
        state.update(100); // same timestamp — last_ts should not change to "equal"
        assert_eq!(state.last_ts, Some(100));
        state.update(200);
        assert_eq!(state.last_ts, Some(200));
    }

    #[test]
    fn test_boundary_strictly_greater_than_threshold() {
        // Covers: replace > with >= in boundary detection
        // Gap exactly at threshold should NOT create boundary
        let mut state = SessionizeBoundaryState::new();
        state.threshold_us = 100;
        state.update(0);
        state.update(100); // gap = 100, NOT > 100
        assert_eq!(state.boundaries, 0);
        assert_eq!(state.finalize(), 1);

        state.update(201); // gap = 101, > 100
        assert_eq!(state.boundaries, 1);
        assert_eq!(state.finalize(), 2);
    }

    // --- Session 4: Mutation-killing tests ---

    #[test]
    fn test_combine_map_or_zero_when_self_has_no_last_ts() {
        // Kills mutant: replacing map_or(0, ...) with map_or(1, ...) in combine.
        // When self.last_ts is None, no boundary should be counted from the gap.
        let a = SessionizeBoundaryState::new(); // no timestamps at all
        let mut b = SessionizeBoundaryState::new();
        b.threshold_us = 100;
        b.update(200);

        let combined = a.combine(&b);
        // a has no data, b has one event → no boundary from the join
        assert_eq!(combined.boundaries, 0);
        assert_eq!(combined.finalize(), 1);
    }

    #[test]
    fn test_combine_last_ts_takes_other_over_self() {
        // Kills mutant: reversing or(self, other) to or(other, self) in last_ts merge.
        // When both have values, other.last_ts (later segment) should be preferred.
        let mut a = SessionizeBoundaryState::new();
        a.threshold_us = 1_000_000;
        a.update(100);
        a.update(200); // a.last_ts = 200

        let mut b = SessionizeBoundaryState::new();
        b.threshold_us = 1_000_000;
        b.update(500); // b.last_ts = 500

        let combined = a.combine(&b);
        assert_eq!(combined.last_ts, Some(500)); // must be b's, not a's
    }

    #[test]
    fn test_combine_first_ts_takes_self_over_other() {
        // Kills mutant: reversing or(self, other) for first_ts.
        // When both have values, the minimum should be selected.
        let mut a = SessionizeBoundaryState::new();
        a.threshold_us = 1_000_000;
        a.update(100); // a.first_ts = 100

        let mut b = SessionizeBoundaryState::new();
        b.threshold_us = 1_000_000;
        b.update(500); // b.first_ts = 500

        let combined = a.combine(&b);
        assert_eq!(combined.first_ts, Some(100)); // must be a's (earlier)
    }

    #[test]
    fn test_finalize_with_no_timestamps_returns_zero() {
        // Kills mutant: changing finalize return value for empty state.
        let state = SessionizeBoundaryState::new();
        assert_eq!(state.finalize(), 0);
    }

    // --- Session 11: DuckDB zero-initialized target combine tests ---
    // DuckDB's segment tree creates fresh zero-initialized target states
    // and combines source states into them. These tests verify that the
    // combine() method correctly propagates threshold_us and all fields
    // when the target (self) is zero-initialized.

    #[test]
    fn test_combine_zero_target_propagates_threshold() {
        // Simulate DuckDB's combine: zero-initialized target + configured source
        let target = SessionizeBoundaryState::new(); // zero-initialized
        let mut source = SessionizeBoundaryState::new();
        source.threshold_us = 60_000_000; // 1 minute
        source.update(0);
        source.update(30_000_000); // same session
        source.update(120_000_000); // new session

        let combined = target.combine(&source);
        // Must propagate threshold from source (via clone in None branch)
        assert_eq!(combined.threshold_us, 60_000_000);
        assert_eq!(combined.finalize(), 2); // 2 sessions
    }

    #[test]
    fn test_combine_zero_target_then_chain() {
        // Zero target combined with source, then combined with another source
        let target = SessionizeBoundaryState::new();
        let mut s1 = SessionizeBoundaryState::new();
        s1.threshold_us = 100_000;
        s1.update(0);
        s1.update(50_000); // same session

        let mut s2 = SessionizeBoundaryState::new();
        s2.threshold_us = 100_000;
        s2.update(200_000); // 150k gap from s1.last_ts, > threshold

        let combined = target.combine(&s1).combine(&s2);
        assert_eq!(combined.threshold_us, 100_000);
        assert_eq!(combined.finalize(), 2);
    }

    #[test]
    fn test_combine_both_zero_targets() {
        // Two zero-initialized states combined should remain empty
        let a = SessionizeBoundaryState::new();
        let b = SessionizeBoundaryState::new();
        let combined = a.combine(&b);
        assert_eq!(combined.threshold_us, 0);
        assert_eq!(combined.finalize(), 0);
    }

    #[test]
    fn test_combine_source_right_propagates() {
        // Source on the right (other) should propagate when target (self) is empty
        let target = SessionizeBoundaryState::new();
        let mut source = SessionizeBoundaryState::new();
        source.threshold_us = 500_000;
        source.update(1_000);

        let combined = target.combine(&source);
        assert_eq!(combined.threshold_us, 500_000);
        assert_eq!(combined.first_ts, Some(1_000));
        assert_eq!(combined.last_ts, Some(1_000));
        assert_eq!(combined.finalize(), 1);
    }

    // --- NULL timestamp tracking tests ---
    // DuckDB window function evaluation: for ROWS BETWEEN UNBOUNDED PRECEDING AND
    // CURRENT ROW, the current row is the rightmost leaf. When the current row has
    // a NULL timestamp, the output should be NULL. The current_row_null flag tracks
    // this through the segment tree combine chain.

    #[test]
    fn test_mark_null_row() {
        let mut state = SessionizeBoundaryState::new();
        assert!(!state.current_row_null);
        state.mark_null_row();
        assert!(state.current_row_null);
        // Timestamps remain unset
        assert!(state.first_ts.is_none());
        assert!(state.last_ts.is_none());
    }

    #[test]
    fn test_update_clears_null_flag() {
        let mut state = SessionizeBoundaryState::new();
        state.threshold_us = 1_000_000;
        state.mark_null_row();
        assert!(state.current_row_null);
        state.update(100);
        assert!(!state.current_row_null);
    }

    #[test]
    fn test_combine_null_right_propagates_flag() {
        // Simulates: [ts1, ts2, NULL] — frame for NULL row
        // Left segment: two non-null timestamps
        let mut left = SessionizeBoundaryState::new();
        left.threshold_us = 1_800_000_000;
        left.update(0);
        left.update(300_000_000);
        assert!(!left.current_row_null);

        // Right segment: NULL timestamp leaf
        let mut right = SessionizeBoundaryState::new();
        right.mark_null_row();
        assert!(right.current_row_null);

        let combined = left.combine(&right);
        // Data from left is preserved
        assert_eq!(combined.first_ts, Some(0));
        assert_eq!(combined.last_ts, Some(300_000_000));
        assert_eq!(combined.boundaries, 0);
        // But null flag from right propagates
        assert!(combined.current_row_null);
    }

    #[test]
    fn test_combine_non_null_right_clears_flag() {
        // Simulates: [NULL, ts1] — frame for ts1 row
        let mut left = SessionizeBoundaryState::new();
        left.mark_null_row();

        let mut right = SessionizeBoundaryState::new();
        right.threshold_us = 1_000_000;
        right.update(100);
        assert!(!right.current_row_null);

        let combined = left.combine(&right);
        // Left is empty (no timestamps), so result is right's clone
        assert_eq!(combined.first_ts, Some(100));
        assert!(!combined.current_row_null);
    }

    #[test]
    fn test_combine_three_segments_null_middle() {
        // Simulates: [ts1, NULL, ts2] — segment tree evaluation
        let mut s1 = SessionizeBoundaryState::new();
        s1.threshold_us = 1_800_000_000;
        s1.update(0);

        let mut s2 = SessionizeBoundaryState::new();
        s2.mark_null_row();

        let mut s3 = SessionizeBoundaryState::new();
        s3.threshold_us = 1_800_000_000;
        s3.update(300_000_000);

        // For row 0 (ts1): frame is [s1] → not null
        assert!(!s1.current_row_null);
        assert_eq!(s1.finalize(), 1);

        // For row 1 (NULL): frame is [s1, s2] → null flag from s2
        let frame_1 = s1.combine(&s2);
        assert!(frame_1.current_row_null);

        // For row 2 (ts2): frame is [s1, s2, s3] → not null (s3 clears it)
        let frame_2 = s1.combine(&s2).combine(&s3);
        assert!(!frame_2.current_row_null);
        assert_eq!(frame_2.finalize(), 1); // 300ms < 30min, same session
    }

    #[test]
    fn test_combine_null_with_null() {
        // Two consecutive NULL rows
        let mut a = SessionizeBoundaryState::new();
        a.mark_null_row();
        let mut b = SessionizeBoundaryState::new();
        b.mark_null_row();

        let combined = a.combine(&b);
        assert!(combined.current_row_null);
        assert!(combined.first_ts.is_none());
    }

    #[test]
    fn test_combine_data_then_null_then_data() {
        // Verifies: [data, null, data] with segment tree combine
        let mut d1 = SessionizeBoundaryState::new();
        d1.threshold_us = 100;
        d1.update(10);

        let mut null_leaf = SessionizeBoundaryState::new();
        null_leaf.mark_null_row();

        let mut d2 = SessionizeBoundaryState::new();
        d2.threshold_us = 100;
        d2.update(20);

        // Left-to-right: ((d1 + null) + d2)
        let lr = d1.combine(&null_leaf).combine(&d2);
        assert!(!lr.current_row_null); // d2 is rightmost, not null
        assert_eq!(lr.first_ts, Some(10));
        assert_eq!(lr.last_ts, Some(20));

        // Right-to-left: (d1 + (null + d2))
        let rl = d1.combine(&null_leaf.combine(&d2));
        assert!(!rl.current_row_null);
        assert_eq!(rl.first_ts, Some(10));
        assert_eq!(rl.last_ts, Some(20));
    }

    #[test]
    fn test_new_state_not_null() {
        // A freshly initialized state is NOT a null row — it's "no data yet"
        let state = SessionizeBoundaryState::new();
        assert!(!state.current_row_null);
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    fn make_boundary_state(threshold_us: i64, ts: i64) -> SessionizeBoundaryState {
        let mut s = SessionizeBoundaryState::new();
        s.threshold_us = threshold_us;
        s.update(ts);
        s
    }

    proptest! {
        #[test]
        fn combine_is_associative(
            t1 in 0i64..1_000_000,
            t2 in 1_000_000i64..2_000_000,
            t3 in 2_000_000i64..3_000_000,
            threshold in 1i64..2_000_000,
        ) {
            let s1 = make_boundary_state(threshold, t1);
            let s2 = make_boundary_state(threshold, t2);
            let s3 = make_boundary_state(threshold, t3);

            let ab_c = s1.combine(&s2).combine(&s3);
            let a_bc = s1.combine(&s2.combine(&s3));
            prop_assert_eq!(ab_c.finalize(), a_bc.finalize());
        }

        #[test]
        fn combine_with_empty_is_identity(
            ts in 0i64..1_000_000_000,
            threshold in 1i64..1_000_000_000,
        ) {
            let s = make_boundary_state(threshold, ts);
            let empty = SessionizeBoundaryState::new();

            let se = s.combine(&empty);
            let es = empty.combine(&s);
            prop_assert_eq!(se.finalize(), s.finalize());
            prop_assert_eq!(es.finalize(), s.finalize());
        }

        #[test]
        fn single_event_always_one_session(
            ts in 0i64..i64::MAX / 2,
            threshold in 0i64..i64::MAX / 2,
        ) {
            let s = make_boundary_state(threshold, ts);
            prop_assert_eq!(s.finalize(), 1);
        }

        #[test]
        fn monotonic_sessions(
            gap1 in 0i64..2_000_000,
            gap2 in 0i64..2_000_000,
            threshold in 1i64..1_000_000,
        ) {
            // Adding more events should never decrease session count
            let mut s = SessionizeBoundaryState::new();
            s.threshold_us = threshold;
            s.update(0);
            let sessions_1 = s.finalize();

            s.update(gap1);
            let sessions_2 = s.finalize();

            s.update(gap1 + gap2);
            let sessions_3 = s.finalize();

            prop_assert!(sessions_2 >= sessions_1);
            prop_assert!(sessions_3 >= sessions_2);
        }
    }
}
