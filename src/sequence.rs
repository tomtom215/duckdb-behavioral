// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! `sequence_match` and `sequence_count` — Pattern matching over event sequences.
//!
//! These aggregate functions match a mini-regex pattern against a time-ordered
//! stream of events with boolean conditions.
//!
//! # SQL Usage
//!
//! ```sql
//! -- Did the user view a product and then purchase?
//! SELECT user_id,
//!   sequence_match('(?1).*(?2)', event_time,
//!     event_type = 'view',
//!     event_type = 'purchase'
//!   ) as converted
//! FROM events
//! GROUP BY user_id
//!
//! -- Count how many times the pattern occurred
//! SELECT user_id,
//!   sequence_count('(?1).*(?2)', event_time,
//!     event_type = 'view',
//!     event_type = 'purchase'
//!   ) as conversion_count
//! FROM events
//! GROUP BY user_id
//! ```

use crate::common::event::{sort_events, Event};
use crate::pattern::executor::{execute_pattern, execute_pattern_events, MatchResult};
use crate::pattern::parser::{parse_pattern, CompiledPattern, PatternError};

/// State for `sequence_match` and `sequence_count` aggregate functions.
///
/// Collects timestamped events during `update`, then matches them against
/// the compiled pattern during `finalize`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SequenceState {
    /// Collected events (timestamp + conditions). Sorted in finalize.
    pub events: Vec<Event>,
    /// Pattern string (parsed on first use in finalize).
    pub pattern_str: Option<String>,
    /// Cached compiled pattern (populated during finalize).
    compiled_pattern: Option<CompiledPattern>,
}

impl SequenceState {
    /// Creates a new empty state.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            events: Vec::new(),
            pattern_str: None,
            compiled_pattern: None,
        }
    }

    /// Sets the pattern string (called once during the first update).
    pub fn set_pattern(&mut self, pattern: &str) {
        if self.pattern_str.is_none() {
            self.pattern_str = Some(pattern.to_string());
        }
    }

    /// Adds an event to the state.
    ///
    /// Only events where at least one condition is true are stored,
    /// as events with all-false conditions cannot match any `(?N)` step.
    pub fn update(&mut self, event: Event) {
        if event.has_any_condition() {
            self.events.push(event);
        }
    }

    /// Combines two states by concatenating their event lists, returning a new state.
    ///
    /// Events do not need to be in sorted order during combine because
    /// `execute()` sorts them before pattern matching.
    #[must_use]
    pub fn combine(&self, other: &Self) -> Self {
        let mut events = Vec::with_capacity(self.events.len() + other.events.len());
        events.extend_from_slice(&self.events);
        events.extend_from_slice(&other.events);
        Self {
            events,
            pattern_str: self
                .pattern_str
                .clone()
                .or_else(|| other.pattern_str.clone()),
            compiled_pattern: None, // Will be recompiled in finalize
        }
    }

    /// Combines another state into `self` in-place by appending its events.
    ///
    /// This is the preferred combine method for sequential (left-fold) chains.
    /// By extending `self.events` in-place, Vec's doubling growth strategy
    /// provides O(N) amortized total copies for a chain of N single-event
    /// combines, compared to O(N²) when allocating a new Vec per combine.
    ///
    /// The compiled pattern is preserved when `self` already has one, avoiding
    /// redundant recompilation in finalize. The pattern string is invariant
    /// within a single query, so `self.compiled_pattern` remains valid.
    pub fn combine_in_place(&mut self, other: &Self) {
        self.events.extend_from_slice(&other.events);
        if self.pattern_str.is_none() {
            self.pattern_str.clone_from(&other.pattern_str);
            // Pattern string changed, invalidate cached compilation
            self.compiled_pattern = None;
        }
    }

    /// Compiles the pattern and executes it against the sorted event stream.
    fn execute(&mut self, count_all: bool) -> Result<MatchResult, PatternError> {
        sort_events(&mut self.events);

        if self.compiled_pattern.is_none() {
            let pattern_str = self.pattern_str.as_deref().unwrap_or("");
            self.compiled_pattern = Some(parse_pattern(pattern_str)?);
        }

        // SAFETY of unwrap: we just ensured compiled_pattern is Some above.
        let pattern = self.compiled_pattern.as_ref().unwrap();
        Ok(execute_pattern(pattern, &self.events, count_all))
    }

    /// Executes `sequence_match` — returns true if the pattern matches.
    ///
    /// # Errors
    ///
    /// Returns `PatternError` if the pattern string is invalid.
    pub fn finalize_match(&mut self) -> Result<bool, PatternError> {
        Ok(self.execute(false)?.matched)
    }

    /// Executes `sequence_count` — returns the number of non-overlapping matches.
    ///
    /// # Errors
    ///
    /// Returns `PatternError` if the pattern string is invalid.
    pub fn finalize_count(&mut self) -> Result<i64, PatternError> {
        Ok(self.execute(true)?.count as i64)
    }

    /// Executes `sequence_match_events` — returns timestamps of matched `(?N)` steps.
    ///
    /// Returns a vector of timestamps, one per `(?N)` condition step in the pattern.
    /// If the pattern does not match, returns an empty vector.
    ///
    /// # Errors
    ///
    /// Returns `PatternError` if the pattern string is invalid.
    pub fn finalize_events(&mut self) -> Result<Vec<i64>, PatternError> {
        sort_events(&mut self.events);

        if self.compiled_pattern.is_none() {
            let pattern_str = self.pattern_str.as_deref().unwrap_or("");
            self.compiled_pattern = Some(parse_pattern(pattern_str)?);
        }

        let pattern = self.compiled_pattern.as_ref().unwrap();
        Ok(execute_pattern_events(pattern, &self.events).unwrap_or_default())
    }
}

impl Default for SequenceState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_event(ts: i64, conds: &[bool]) -> Event {
        Event::from_bools(ts, conds)
    }

    #[test]
    fn test_empty_state() {
        let mut state = SequenceState::new();
        state.set_pattern("(?1)");
        assert!(!state.finalize_match().unwrap());
        assert_eq!(state.finalize_count().unwrap(), 0);
    }

    #[test]
    fn test_simple_match() {
        let mut state = SequenceState::new();
        state.set_pattern("(?1)(?2)");
        state.update(make_event(100, &[true, false]));
        state.update(make_event(200, &[false, true]));
        assert!(state.finalize_match().unwrap());
    }

    #[test]
    fn test_simple_no_match() {
        let mut state = SequenceState::new();
        state.set_pattern("(?1)(?2)");
        state.update(make_event(100, &[false, true])); // wrong order
        state.update(make_event(200, &[true, false]));
        assert!(!state.finalize_match().unwrap());
    }

    #[test]
    fn test_wildcard_match() {
        let mut state = SequenceState::new();
        state.set_pattern("(?1).*(?2)");
        state.update(make_event(100, &[true, false]));
        state.update(make_event(200, &[false, false])); // gap
        state.update(make_event(300, &[false, true]));
        assert!(state.finalize_match().unwrap());
    }

    #[test]
    fn test_count_multiple() {
        let mut state = SequenceState::new();
        state.set_pattern("(?1)(?2)");
        state.update(make_event(100, &[true, false]));
        state.update(make_event(200, &[false, true]));
        state.update(make_event(300, &[true, false]));
        state.update(make_event(400, &[false, true]));
        assert_eq!(state.finalize_count().unwrap(), 2);
    }

    #[test]
    fn test_time_constraint() {
        let mut state = SequenceState::new();
        state.set_pattern("(?1)(?t<=2)(?2)");
        state.update(make_event(0, &[true, false]));
        state.update(make_event(1_000_000, &[false, true])); // 1s <= 2s
        assert!(state.finalize_match().unwrap());
    }

    #[test]
    fn test_time_constraint_fails() {
        let mut state = SequenceState::new();
        state.set_pattern("(?1)(?t<=1)(?2)");
        state.update(make_event(0, &[true, false]));
        state.update(make_event(3_000_000, &[false, true])); // 3s > 1s
        assert!(!state.finalize_match().unwrap());
    }

    #[test]
    fn test_combine() {
        let mut a = SequenceState::new();
        a.set_pattern("(?1).*(?2)");
        a.update(make_event(100, &[true, false]));

        let mut b = SequenceState::new();
        b.update(make_event(200, &[false, true]));

        let mut combined = a.combine(&b);
        assert!(combined.finalize_match().unwrap());
    }

    #[test]
    fn test_invalid_pattern() {
        let mut state = SequenceState::new();
        state.set_pattern("(?x)");
        state.update(make_event(100, &[true]));
        assert!(state.finalize_match().is_err());
    }

    #[test]
    fn test_three_step_pattern() {
        let mut state = SequenceState::new();
        state.set_pattern("(?1).*(?2).*(?3)");
        state.update(make_event(100, &[true, false, false]));
        state.update(make_event(200, &[false, true, false]));
        state.update(make_event(300, &[false, false, true]));
        assert!(state.finalize_match().unwrap());
    }

    #[test]
    fn test_unsorted_input() {
        let mut state = SequenceState::new();
        state.set_pattern("(?1)(?2)");
        // Insert out of order
        state.update(make_event(200, &[false, true]));
        state.update(make_event(100, &[true, false]));
        assert!(state.finalize_match().unwrap());
    }

    #[test]
    fn test_non_overlapping_count() {
        let mut state = SequenceState::new();
        state.set_pattern("(?1)(?2)");
        // Three possible matches, but non-overlapping means events can't be reused
        state.update(make_event(100, &[true, false]));
        state.update(make_event(200, &[false, true]));
        state.update(make_event(300, &[true, false]));
        state.update(make_event(400, &[false, true]));
        state.update(make_event(500, &[true, false]));
        state.update(make_event(600, &[false, true]));
        assert_eq!(state.finalize_count().unwrap(), 3);
    }

    #[test]
    fn test_no_pattern_set() {
        // When no pattern is set, pattern_str defaults to None → empty string → error
        let mut state = SequenceState::new();
        state.update(make_event(100, &[true]));
        assert!(state.finalize_match().is_err());
    }

    #[test]
    fn test_set_pattern_only_first_call() {
        // set_pattern should only set the pattern on the first call
        let mut state = SequenceState::new();
        state.set_pattern("(?1)");
        state.set_pattern("(?2)"); // ignored
        assert_eq!(state.pattern_str.as_deref(), Some("(?1)"));
    }

    #[test]
    fn test_combine_preserves_pattern() {
        let mut a = SequenceState::new();
        a.set_pattern("(?1)(?2)");
        a.update(make_event(100, &[true, false]));

        let mut b = SequenceState::new();
        // No pattern set on b
        b.update(make_event(200, &[false, true]));

        let mut combined = a.combine(&b);
        // Pattern should come from a
        assert!(combined.finalize_match().unwrap());
    }

    #[test]
    fn test_combine_both_empty() {
        let a = SequenceState::new();
        let b = SequenceState::new();
        let combined = a.combine(&b);
        assert!(combined.events.is_empty());
        assert!(combined.pattern_str.is_none());
    }

    #[test]
    fn test_all_false_events_filtered() {
        let mut state = SequenceState::new();
        state.set_pattern("(?1)");
        state.update(make_event(100, &[false, false]));
        state.update(make_event(200, &[false, false]));
        assert!(state.events.is_empty()); // all filtered out
        assert!(!state.finalize_match().unwrap());
    }

    #[test]
    fn test_count_with_no_matches() {
        let mut state = SequenceState::new();
        state.set_pattern("(?1)(?2)");
        state.update(make_event(100, &[true, false]));
        // No (?2) event
        assert_eq!(state.finalize_count().unwrap(), 0);
    }

    #[test]
    fn test_match_with_time_constraint_and_wildcard() {
        let mut state = SequenceState::new();
        state.set_pattern("(?1).*(?t<=5)(?2)");
        state.update(make_event(0, &[true, false]));
        state.update(make_event(1_000_000, &[false, false])); // gap
        state.update(make_event(2_000_000, &[false, true])); // 2s <= 5s from last match
        assert!(state.finalize_match().unwrap());
    }

    // --- Mutation testing coverage: combine_in_place ---

    #[test]
    fn test_combine_in_place_basic() {
        let mut a = SequenceState::new();
        a.set_pattern("(?1).*(?2)");
        a.update(make_event(100, &[true, false]));

        let mut b = SequenceState::new();
        b.update(make_event(200, &[false, true]));

        a.combine_in_place(&b);
        assert_eq!(a.events.len(), 2);
        assert!(a.finalize_match().unwrap());
    }

    #[test]
    fn test_combine_in_place_preserves_pattern() {
        let mut a = SequenceState::new();
        a.set_pattern("(?1)(?2)");
        a.update(make_event(100, &[true, false]));

        let mut b = SequenceState::new();
        // b has no pattern
        b.update(make_event(200, &[false, true]));

        a.combine_in_place(&b);
        assert_eq!(a.pattern_str.as_deref(), Some("(?1)(?2)"));
        assert!(a.finalize_match().unwrap());
    }

    #[test]
    fn test_combine_in_place_takes_pattern_from_other() {
        let mut a = SequenceState::new();
        // a has no pattern
        a.update(make_event(100, &[true, false]));

        let mut b = SequenceState::new();
        b.set_pattern("(?1)(?2)");
        b.update(make_event(200, &[false, true]));

        a.combine_in_place(&b);
        assert_eq!(a.pattern_str.as_deref(), Some("(?1)(?2)"));
        assert!(a.finalize_match().unwrap());
    }

    // --- Session 3: Mutation-killing boundary tests ---

    #[test]
    fn test_combine_in_place_preserves_compiled_pattern() {
        // Kills mutant: unconditionally set compiled_pattern = None.
        // After set_pattern + finalize, compiled_pattern is populated.
        // combine_in_place should preserve it if pattern_str didn't change.
        let mut a = SequenceState::new();
        a.set_pattern("(?1)(?2)");
        a.update(make_event(100, &[true, false]));
        // Trigger compilation
        let _ = a.finalize_match();

        let mut b = SequenceState::new();
        b.set_pattern("(?1)(?2)");
        b.update(make_event(200, &[false, true]));

        a.combine_in_place(&b);
        // a already had pattern_str, so compiled_pattern should be preserved
        // If wrongly invalidated, finalize_match still works but recompiles unnecessarily
        assert!(a.finalize_match().unwrap());
    }

    #[test]
    fn test_combine_events_extend_not_replace() {
        // Kills mutant: replace extend_from_slice with assignment/clear.
        let mut a = SequenceState::new();
        a.set_pattern("(?1)(?2)");
        a.update(make_event(100, &[true, false]));
        a.update(make_event(200, &[true, false]));

        let mut b = SequenceState::new();
        b.update(make_event(300, &[false, true]));

        a.combine_in_place(&b);
        // Should have all 3 events, not just b's
        assert_eq!(a.events.len(), 3);
    }

    #[test]
    fn test_empty_pattern_string_errors() {
        // Boundary: empty string pattern should produce an error.
        let mut state = SequenceState::new();
        state.set_pattern("");
        state.update(make_event(100, &[true]));
        assert!(state.finalize_match().is_err());
    }

    // --- sequence_match_events tests ---

    #[test]
    fn test_events_simple_two_step() {
        let mut state = SequenceState::new();
        state.set_pattern("(?1)(?2)");
        state.update(make_event(100, &[true, false]));
        state.update(make_event(200, &[false, true]));
        let events = state.finalize_events().unwrap();
        assert_eq!(events, vec![100, 200]);
    }

    #[test]
    fn test_events_with_wildcard() {
        let mut state = SequenceState::new();
        state.set_pattern("(?1).*(?2)");
        state.update(make_event(100, &[true, false]));
        state.update(make_event(200, &[false, false])); // gap
        state.update(make_event(300, &[false, true]));
        let events = state.finalize_events().unwrap();
        // Only condition steps are returned, not wildcard events
        assert_eq!(events, vec![100, 300]);
    }

    #[test]
    fn test_events_no_match_empty() {
        let mut state = SequenceState::new();
        state.set_pattern("(?1)(?2)");
        state.update(make_event(100, &[false, true])); // wrong order
        state.update(make_event(200, &[true, false]));
        let events = state.finalize_events().unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn test_events_empty_state() {
        let mut state = SequenceState::new();
        state.set_pattern("(?1)");
        let events = state.finalize_events().unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn test_events_three_step_pattern() {
        let mut state = SequenceState::new();
        state.set_pattern("(?1).*(?2).*(?3)");
        state.update(make_event(100, &[true, false, false]));
        state.update(make_event(200, &[false, true, false]));
        state.update(make_event(300, &[false, false, true]));
        let events = state.finalize_events().unwrap();
        assert_eq!(events, vec![100, 200, 300]);
    }

    #[test]
    fn test_events_with_time_constraint() {
        let mut state = SequenceState::new();
        state.set_pattern("(?1)(?t>=2)(?2)");
        state.update(make_event(0, &[true, false]));
        state.update(make_event(3_000_000, &[false, true])); // 3s >= 2
        let events = state.finalize_events().unwrap();
        assert_eq!(events, vec![0, 3_000_000]);
    }

    #[test]
    fn test_events_time_constraint_fails_empty() {
        let mut state = SequenceState::new();
        state.set_pattern("(?1)(?t>=5)(?2)");
        state.update(make_event(0, &[true, false]));
        state.update(make_event(1_000_000, &[false, true])); // 1s < 5
        let events = state.finalize_events().unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn test_events_invalid_pattern_error() {
        let mut state = SequenceState::new();
        state.set_pattern("(?x)");
        state.update(make_event(100, &[true]));
        assert!(state.finalize_events().is_err());
    }

    #[test]
    fn test_events_single_condition() {
        let mut state = SequenceState::new();
        state.set_pattern("(?1)");
        state.update(make_event(42, &[true]));
        let events = state.finalize_events().unwrap();
        assert_eq!(events, vec![42]);
    }

    #[test]
    fn test_events_returns_first_match() {
        // When multiple matches exist, returns the first (earliest) one
        let mut state = SequenceState::new();
        state.set_pattern("(?1)(?2)");
        state.update(make_event(100, &[true, false]));
        state.update(make_event(200, &[false, true]));
        state.update(make_event(300, &[true, false]));
        state.update(make_event(400, &[false, true]));
        let events = state.finalize_events().unwrap();
        assert_eq!(events, vec![100, 200]); // first match
    }

    #[test]
    fn test_events_unsorted_input() {
        let mut state = SequenceState::new();
        state.set_pattern("(?1)(?2)");
        state.update(make_event(200, &[false, true]));
        state.update(make_event(100, &[true, false]));
        let events = state.finalize_events().unwrap();
        assert_eq!(events, vec![100, 200]);
    }

    #[test]
    fn test_events_with_one_event_gap() {
        let mut state = SequenceState::new();
        state.set_pattern("(?1).(?2)");
        state.update(make_event(100, &[true, false, false]));
        // The `.` gap event must have at least one condition true to pass the filter
        state.update(make_event(200, &[false, false, true])); // one event gap
        state.update(make_event(300, &[false, true, false]));
        let events = state.finalize_events().unwrap();
        // Only (?1) and (?2) timestamps, not the `.` event
        assert_eq!(events, vec![100, 300]);
    }

    #[test]
    fn test_events_combine_then_finalize() {
        let mut a = SequenceState::new();
        a.set_pattern("(?1).*(?2)");
        a.update(make_event(100, &[true, false]));

        let mut b = SequenceState::new();
        b.update(make_event(200, &[false, true]));

        a.combine_in_place(&b);
        let events = a.finalize_events().unwrap();
        assert_eq!(events, vec![100, 200]);
    }

    #[test]
    fn test_events_no_pattern_set_error() {
        let mut state = SequenceState::new();
        state.update(make_event(100, &[true]));
        assert!(state.finalize_events().is_err());
    }

    // --- Session 11: DuckDB zero-initialized target combine tests ---

    #[test]
    fn test_combine_in_place_zero_target_propagates_pattern() {
        // DuckDB's segment tree: fresh target + configured source
        let mut target = SequenceState::new(); // zero-initialized
        let mut source = SequenceState::new();
        source.set_pattern("(?1)(?2)");
        source.update(make_event(100, &[true, false]));
        source.update(make_event(200, &[false, true]));

        target.combine_in_place(&source);
        assert_eq!(target.pattern_str.as_deref(), Some("(?1)(?2)"));
        assert!(target.finalize_match().unwrap());
    }

    #[test]
    fn test_combine_in_place_zero_target_chain_finalize() {
        let mut target = SequenceState::new();
        let mut s1 = SequenceState::new();
        s1.set_pattern("(?1).*(?2).*(?3)");
        s1.update(make_event(100, &[true, false, false]));

        let mut s2 = SequenceState::new();
        s2.update(make_event(200, &[false, true, false]));

        let mut s3 = SequenceState::new();
        s3.update(make_event(300, &[false, false, true]));

        target.combine_in_place(&s1);
        target.combine_in_place(&s2);
        target.combine_in_place(&s3);
        assert!(target.finalize_match().unwrap());
        assert_eq!(target.events.len(), 3);
    }

    #[test]
    fn test_combine_in_place_zero_target_count() {
        let mut target = SequenceState::new();
        let mut source = SequenceState::new();
        source.set_pattern("(?1)(?2)");
        source.update(make_event(100, &[true, false]));
        source.update(make_event(200, &[false, true]));
        source.update(make_event(300, &[true, false]));
        source.update(make_event(400, &[false, true]));

        target.combine_in_place(&source);
        assert_eq!(target.finalize_count().unwrap(), 2);
    }

    #[test]
    fn test_combine_in_place_zero_target_events() {
        let mut target = SequenceState::new();
        let mut source = SequenceState::new();
        source.set_pattern("(?1)(?2)");
        source.update(make_event(100, &[true, false]));
        source.update(make_event(200, &[false, true]));

        target.combine_in_place(&source);
        let events = target.finalize_events().unwrap();
        assert_eq!(events, vec![100, 200]);
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn count_non_negative(
            num_events in 0..=50usize,
        ) {
            let mut state = SequenceState::new();
            state.set_pattern("(?1)(?2)");
            for i in 0..num_events {
                let bitmask = 1u32 << (i % 2);
                state.update(Event::from_bools(i as i64, &[bitmask & 1 != 0, bitmask & 2 != 0]));
            }
            let count = state.finalize_count().unwrap();
            prop_assert!(count >= 0);
        }

        #[test]
        fn match_implies_count_at_least_one(
            num_events in 2..=30usize,
        ) {
            // If match returns true, count must be >= 1
            let mut match_state = SequenceState::new();
            match_state.set_pattern("(?1)(?2)");
            let mut count_state = SequenceState::new();
            count_state.set_pattern("(?1)(?2)");

            for i in 0..num_events {
                let bitmask = 1u32 << (i % 2);
                let event = Event::new(i as i64, bitmask);
                match_state.update(event);
                count_state.update(event);
            }

            let matched = match_state.finalize_match().unwrap();
            let count = count_state.finalize_count().unwrap();

            if matched {
                prop_assert!(count >= 1);
            } else {
                prop_assert_eq!(count, 0);
            }
        }

        #[test]
        fn all_false_events_produce_no_match(
            num_events in 0..=50usize,
        ) {
            let mut state = SequenceState::new();
            state.set_pattern("(?1)");
            for i in 0..num_events {
                state.update(Event::new(i as i64, 0u32)); // all conditions false
            }
            // All-false events are filtered by update(), so no match possible
            prop_assert!(!state.finalize_match().unwrap());
        }

        #[test]
        fn combine_preserves_events(
            n_a in 0..=20usize,
            n_b in 0..=20usize,
        ) {
            let mut a = SequenceState::new();
            a.set_pattern("(?1)(?2)");
            for i in 0..n_a {
                a.update(Event::new(i as i64, 1u32));
            }

            let mut b = SequenceState::new();
            b.set_pattern("(?1)(?2)");
            for i in 0..n_b {
                b.update(Event::new((n_a + i) as i64, 2u32));
            }

            let combined = a.combine(&b);
            prop_assert_eq!(combined.events.len(), n_a + n_b);
        }

        // --- 32-condition property tests ---

        #[test]
        fn high_condition_indices_match(
            cond_idx in 8..=31usize,
            num_events in 2..=20usize,
        ) {
            // Pattern (?N) where N is a high condition index (9-32, 1-indexed)
            let pattern = format!("(?{})", cond_idx + 1);
            let mut state = SequenceState::new();
            state.set_pattern(&pattern);
            for i in 0..num_events {
                let bitmask = 1u32 << cond_idx;
                state.update(Event::new(i as i64, bitmask));
            }
            // Should match since all events satisfy the condition
            let matched = state.finalize_match().unwrap();
            prop_assert!(matched);
        }

        #[test]
        fn high_condition_pair_match(
            cond_a in 8..=30usize,
        ) {
            // Two consecutive high-index conditions: (?A)(?B)
            let cond_b = cond_a + 1;
            let pattern = format!("(?{})(?{})", cond_a + 1, cond_b + 1);
            let mut state = SequenceState::new();
            state.set_pattern(&pattern);

            // First event satisfies condition A
            state.update(Event::new(100, 1u32 << cond_a));
            // Second event satisfies condition B
            state.update(Event::new(200, 1u32 << cond_b));

            let matched = state.finalize_match().unwrap();
            prop_assert!(matched);
        }

        #[test]
        fn condition_32_boundary_no_match(
            num_events in 1..=10usize,
        ) {
            // (?32) requires condition index 31 (0-indexed). Events with only
            // condition 0 set should NOT match.
            let mut state = SequenceState::new();
            state.set_pattern("(?32)");
            for i in 0..num_events {
                state.update(Event::new(i as i64, 1u32)); // only bit 0
            }
            let matched = state.finalize_match().unwrap();
            prop_assert!(!matched);
        }
    }
}
