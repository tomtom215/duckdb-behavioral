//! NFA-based pattern executor for sequence matching.
//!
//! Executes compiled patterns against sorted event streams using a
//! non-deterministic finite automaton (NFA) with backtracking for `.*` steps.

use crate::common::event::Event;
use crate::common::timestamp::MICROS_PER_SECOND;
use crate::pattern::parser::{CompiledPattern, PatternStep};

/// Maximum number of active NFA states before aborting execution.
/// Prevents pathological patterns (e.g., `.*.*.*.*`) from consuming
/// unbounded memory.
const MAX_NFA_STATES: usize = 10_000;

/// Result of executing a pattern against an event stream.
#[derive(Debug, Clone)]
pub struct MatchResult {
    /// Whether any full match was found.
    pub matched: bool,
    /// Number of non-overlapping full matches found.
    pub count: usize,
}

/// Executes a compiled pattern against a sorted event stream.
///
/// Events must be sorted by timestamp (ascending) before calling this function.
///
/// For `sequence_match` semantics: returns as soon as one match is found.
/// For `sequence_count` semantics: counts all non-overlapping matches.
///
/// # Algorithm
///
/// First attempts to classify the pattern into a fast-path shape:
///
/// - **Adjacent conditions only** (`(?1)(?2)(?3)`): O(n) scan with a sliding
///   window of `k` events. No NFA overhead.
/// - **Wildcard-separated conditions** (`(?1).*(?2).*(?3)`): O(n) single-pass
///   linear scan with a step counter. No NFA overhead.
/// - **Complex patterns**: Falls back to full NFA with backtracking.
///
/// The fast paths produce identical results to the NFA but eliminate per-position
/// stack management, function call overhead, and backtracking state.
pub fn execute_pattern(
    pattern: &CompiledPattern,
    events: &[Event],
    count_all: bool,
) -> MatchResult {
    if events.is_empty() || pattern.steps.is_empty() {
        return MatchResult {
            matched: false,
            count: 0,
        };
    }

    // Try fast paths for common pattern shapes before falling back to NFA.
    match classify_pattern(pattern) {
        PatternShape::AdjacentConditions(ref conds) => {
            return fast_adjacent(events, conds, count_all);
        }
        PatternShape::WildcardSeparated(ref conds) => {
            return fast_wildcard(events, conds, count_all);
        }
        PatternShape::Complex => {} // Fall through to NFA
    }

    execute_pattern_nfa(pattern, events, count_all)
}

/// Pattern shape classification for fast-path dispatch.
enum PatternShape {
    /// All steps are `Condition` — adjacent matching required.
    AdjacentConditions(Vec<usize>),
    /// Conditions separated by `.*` — greedy forward scan.
    WildcardSeparated(Vec<usize>),
    /// Requires full NFA (time constraints, `.`, mixed shapes).
    Complex,
}

/// Classifies a compiled pattern into a fast-path shape.
///
/// Returns `AdjacentConditions` if all steps are `Condition` (no wildcards).
/// Returns `WildcardSeparated` if the pattern alternates `Condition` and
/// `AnyEvents` steps (e.g., `(?1).*(?2).*(?3)`).
/// Returns `Complex` for patterns with time constraints, `.` (`OneEvent`),
/// or mixed structures.
fn classify_pattern(pattern: &CompiledPattern) -> PatternShape {
    let mut conditions = Vec::new();
    let mut has_any_events = false;
    let mut has_only_conditions = true;

    for step in &pattern.steps {
        match step {
            PatternStep::Condition(idx) => conditions.push(*idx),
            PatternStep::AnyEvents => {
                has_any_events = true;
                has_only_conditions = false;
            }
            PatternStep::OneEvent | PatternStep::TimeConstraint(_, _) => {
                return PatternShape::Complex;
            }
        }
    }

    if conditions.is_empty() {
        return PatternShape::Complex;
    }

    if has_only_conditions {
        return PatternShape::AdjacentConditions(conditions);
    }

    // Has AnyEvents — check if it's the standard wildcard-separated form.
    // Accept any mix of Condition and AnyEvents (consecutive AnyEvents is
    // just .*.* which matches any number of events, same as .*).
    if has_any_events {
        return PatternShape::WildcardSeparated(conditions);
    }

    PatternShape::Complex
}

/// Fast path for adjacent-condition patterns like `(?1)(?2)(?3)`.
///
/// Scans with a sliding window of `k` events, checking each window for a
/// consecutive match of all conditions. O(n) time, O(1) space.
fn fast_adjacent(events: &[Event], conditions: &[usize], count_all: bool) -> MatchResult {
    let k = conditions.len();
    if events.len() < k {
        return MatchResult {
            matched: false,
            count: 0,
        };
    }

    let mut total = 0;
    let mut i = 0;
    while i + k <= events.len() {
        let mut matched = true;
        for (j, &cond_idx) in conditions.iter().enumerate() {
            if !events[i + j].condition(cond_idx) {
                matched = false;
                i += 1;
                break;
            }
        }
        if matched {
            total += 1;
            if !count_all {
                return MatchResult {
                    matched: true,
                    count: 1,
                };
            }
            i += k; // Non-overlapping: advance past the match
        }
    }

    MatchResult {
        matched: total > 0,
        count: total,
    }
}

/// Fast path for wildcard-separated patterns like `(?1).*(?2).*(?3)`.
///
/// Single-pass linear scan: maintains a step counter and advances through
/// conditions as matching events are found. O(n) time, O(1) space.
/// Equivalent to lazy NFA matching for this pattern shape.
fn fast_wildcard(events: &[Event], conditions: &[usize], count_all: bool) -> MatchResult {
    let k = conditions.len();
    let mut total = 0;
    let mut step = 0;

    for event in events {
        if event.condition(conditions[step]) {
            step += 1;
            if step >= k {
                total += 1;
                if !count_all {
                    return MatchResult {
                        matched: true,
                        count: 1,
                    };
                }
                step = 0; // Reset for next non-overlapping match
            }
        }
    }

    MatchResult {
        matched: total > 0,
        count: total,
    }
}

/// Full NFA-based pattern execution for complex patterns.
///
/// Used when the pattern contains time constraints, `.` (`OneEvent`),
/// or other structures that cannot be handled by the fast paths.
fn execute_pattern_nfa(
    pattern: &CompiledPattern,
    events: &[Event],
    count_all: bool,
) -> MatchResult {
    let mut total_matches = 0;
    let mut search_start = 0;
    // Pre-allocate the NFA state stack once and reuse across all starting
    // positions. This eliminates per-position heap allocation: instead of
    // O(N) alloc/free pairs, we do O(1) total allocations. The Vec is
    // cleared (retaining capacity) at the start of each try_match_from call.
    let mut states = Vec::with_capacity(pattern.steps.len() * 2);

    while search_start < events.len() {
        if let Some(match_end) = try_match_from(pattern, events, search_start, &mut states) {
            total_matches += 1;
            if !count_all {
                return MatchResult {
                    matched: true,
                    count: 1,
                };
            }
            // For non-overlapping count, advance past this match
            search_start = match_end + 1;
        } else {
            search_start += 1;
        }
    }

    MatchResult {
        matched: total_matches > 0,
        count: total_matches,
    }
}

/// Tries to match the full pattern starting from the given event index.
///
/// Returns `Some(end_index)` if a full match is found (the index of the last
/// matched event), or `None` if no match is possible from this starting position.
///
/// The `states` Vec is pre-allocated by the caller and reused across calls
/// to avoid per-position heap allocation (see `execute_pattern` for rationale).
fn try_match_from(
    pattern: &CompiledPattern,
    events: &[Event],
    start: usize,
    states: &mut Vec<NfaState>,
) -> Option<usize> {
    states.clear();
    states.push(NfaState {
        event_idx: start,
        step_idx: 0,
        last_match_ts: None,
    });

    let mut iterations = 0;

    while let Some(state) = states.pop() {
        iterations += 1;
        if iterations > MAX_NFA_STATES {
            // Prevent runaway matching on pathological patterns
            return None;
        }

        // Successfully matched all steps
        if state.step_idx >= pattern.steps.len() {
            // Return the index of the last consumed event (one before current)
            return Some(if state.event_idx > 0 {
                state.event_idx - 1
            } else {
                0
            });
        }

        // No more events to consume
        if state.event_idx >= events.len() {
            // Time constraints and AnyEvents can still succeed at the end
            match &pattern.steps[state.step_idx] {
                PatternStep::AnyEvents => {
                    // .* can match zero events, advance to next step
                    states.push(NfaState {
                        step_idx: state.step_idx + 1,
                        ..state
                    });
                }
                _ => continue,
            }
            continue;
        }

        let event = &events[state.event_idx];

        match &pattern.steps[state.step_idx] {
            PatternStep::Condition(cond_idx) => {
                if event.condition(*cond_idx) {
                    // Condition matched, advance both event and step
                    states.push(NfaState {
                        event_idx: state.event_idx + 1,
                        step_idx: state.step_idx + 1,
                        last_match_ts: Some(event.timestamp_us),
                    });
                }
                // If condition doesn't match, this state dies (no push)
            }
            PatternStep::AnyEvents => {
                // .* can consume this event and stay in the same step
                // Pushed FIRST so it sits lower in the LIFO stack
                states.push(NfaState {
                    event_idx: state.event_idx + 1,
                    ..state
                });
                // .* can match zero events (skip to next step without consuming)
                // Pushed LAST so it's popped FIRST — prioritizes advancing the pattern
                // over consuming more events (lazy matching)
                states.push(NfaState {
                    step_idx: state.step_idx + 1,
                    ..state
                });
            }
            PatternStep::OneEvent => {
                // . matches exactly one event
                states.push(NfaState {
                    event_idx: state.event_idx + 1,
                    step_idx: state.step_idx + 1,
                    last_match_ts: Some(event.timestamp_us),
                });
            }
            PatternStep::TimeConstraint(op, threshold_seconds) => {
                // Time constraint doesn't consume an event, just checks timing
                if let Some(prev_ts) = state.last_match_ts {
                    let elapsed_us = event.timestamp_us - prev_ts;
                    let elapsed_seconds = elapsed_us / MICROS_PER_SECOND;
                    if op.evaluate(elapsed_seconds, *threshold_seconds) {
                        // Time constraint satisfied, advance step
                        states.push(NfaState {
                            step_idx: state.step_idx + 1,
                            ..state
                        });
                    }
                } else {
                    // No previous match timestamp; time constraint is vacuously true
                    states.push(NfaState {
                        step_idx: state.step_idx + 1,
                        ..state
                    });
                }
            }
        }
    }

    None
}

/// Executes a compiled pattern and returns matched condition timestamps.
///
/// Returns timestamps for `(?N)` condition steps only (not `.`, `.*`, or
/// time constraints). Returns `Some(vec![ts1, ts2, ...])` if the pattern
/// matches, `None` if no match is found. Events must be sorted by
/// timestamp (ascending) before calling.
pub fn execute_pattern_events(pattern: &CompiledPattern, events: &[Event]) -> Option<Vec<i64>> {
    if events.is_empty() || pattern.steps.is_empty() {
        return None;
    }

    try_match_from_with_timestamps(pattern, events, 0, events.len())
}

/// Tries to match the full pattern starting from position range `[start, end)`,
/// collecting timestamps for each `(?N)` condition step.
fn try_match_from_with_timestamps(
    pattern: &CompiledPattern,
    events: &[Event],
    search_start: usize,
    search_end: usize,
) -> Option<Vec<i64>> {
    for start in search_start..search_end {
        if let Some(timestamps) = try_match_collecting(pattern, events, start) {
            return Some(timestamps);
        }
    }
    None
}

/// Tries to match from a specific start position, collecting condition timestamps.
fn try_match_collecting(
    pattern: &CompiledPattern,
    events: &[Event],
    start: usize,
) -> Option<Vec<i64>> {
    // Count how many Condition steps are in the pattern
    let num_conditions = pattern
        .steps
        .iter()
        .filter(|s| matches!(s, PatternStep::Condition(_)))
        .count();

    let mut states: Vec<NfaStateWithTimestamps> = vec![NfaStateWithTimestamps {
        event_idx: start,
        step_idx: 0,
        last_match_ts: None,
        collected: Vec::with_capacity(num_conditions),
    }];

    let mut iterations = 0;

    while let Some(state) = states.pop() {
        iterations += 1;
        if iterations > MAX_NFA_STATES {
            return None;
        }

        // Successfully matched all steps
        if state.step_idx >= pattern.steps.len() {
            return Some(state.collected);
        }

        // No more events to consume
        if state.event_idx >= events.len() {
            match &pattern.steps[state.step_idx] {
                PatternStep::AnyEvents => {
                    states.push(NfaStateWithTimestamps {
                        step_idx: state.step_idx + 1,
                        ..state
                    });
                }
                _ => continue,
            }
            continue;
        }

        let event = &events[state.event_idx];

        match &pattern.steps[state.step_idx] {
            PatternStep::Condition(cond_idx) => {
                if event.condition(*cond_idx) {
                    let mut new_collected = state.collected.clone();
                    new_collected.push(event.timestamp_us);
                    states.push(NfaStateWithTimestamps {
                        event_idx: state.event_idx + 1,
                        step_idx: state.step_idx + 1,
                        last_match_ts: Some(event.timestamp_us),
                        collected: new_collected,
                    });
                }
            }
            PatternStep::AnyEvents => {
                // Consume event (stay in same step) — pushed first (lower priority)
                states.push(NfaStateWithTimestamps {
                    event_idx: state.event_idx + 1,
                    ..state.clone()
                });
                // Advance step (lazy) — pushed last (higher priority)
                states.push(NfaStateWithTimestamps {
                    step_idx: state.step_idx + 1,
                    ..state
                });
            }
            PatternStep::OneEvent => {
                states.push(NfaStateWithTimestamps {
                    event_idx: state.event_idx + 1,
                    step_idx: state.step_idx + 1,
                    last_match_ts: Some(event.timestamp_us),
                    collected: state.collected,
                });
            }
            PatternStep::TimeConstraint(op, threshold_seconds) => {
                if let Some(prev_ts) = state.last_match_ts {
                    let elapsed_us = event.timestamp_us - prev_ts;
                    let elapsed_seconds = elapsed_us / MICROS_PER_SECOND;
                    if op.evaluate(elapsed_seconds, *threshold_seconds) {
                        states.push(NfaStateWithTimestamps {
                            step_idx: state.step_idx + 1,
                            ..state
                        });
                    }
                } else {
                    states.push(NfaStateWithTimestamps {
                        step_idx: state.step_idx + 1,
                        ..state
                    });
                }
            }
        }
    }

    None
}

/// NFA state that also collects matched condition timestamps.
#[derive(Debug, Clone)]
struct NfaStateWithTimestamps {
    /// Current position in the event stream.
    event_idx: usize,
    /// Current position in the pattern steps.
    step_idx: usize,
    /// Timestamp of the last matched event (for time constraints).
    last_match_ts: Option<i64>,
    /// Collected timestamps for each matched `(?N)` condition step.
    collected: Vec<i64>,
}

/// State of a single NFA thread.
///
/// At 24 bytes with `Copy` semantics, NFA states are stack-allocated
/// and avoid heap cloning overhead during backtracking exploration.
#[derive(Debug, Clone, Copy)]
struct NfaState {
    /// Current position in the event stream.
    event_idx: usize,
    /// Current position in the pattern steps.
    step_idx: usize,
    /// Timestamp of the last matched event (for time constraints).
    last_match_ts: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pattern::parser::parse_pattern;

    fn make_events(data: &[(i64, &[bool])]) -> Vec<Event> {
        data.iter()
            .map(|(ts, conds)| Event::from_bools(*ts, conds))
            .collect()
    }

    #[test]
    fn test_simple_match() {
        let pattern = parse_pattern("(?1)(?2)").unwrap();
        let events = make_events(&[(100, &[true, false]), (200, &[false, true])]);
        let result = execute_pattern(&pattern, &events, false);
        assert!(result.matched);
    }

    #[test]
    fn test_simple_no_match() {
        let pattern = parse_pattern("(?1)(?2)").unwrap();
        let events = make_events(&[(100, &[false, true]), (200, &[true, false])]);
        let result = execute_pattern(&pattern, &events, false);
        assert!(!result.matched);
    }

    #[test]
    fn test_wildcard_match() {
        let pattern = parse_pattern("(?1).*(?2)").unwrap();
        let events = make_events(&[
            (100, &[true, false]),
            (200, &[false, false]), // gap event
            (300, &[false, false]), // gap event
            (400, &[false, true]),
        ]);
        let result = execute_pattern(&pattern, &events, false);
        assert!(result.matched);
    }

    #[test]
    fn test_one_event_gap() {
        let pattern = parse_pattern("(?1).(?2)").unwrap();
        let events = make_events(&[
            (100, &[true, false]),
            (200, &[false, false]), // exactly one event gap
            (300, &[false, true]),
        ]);
        let result = execute_pattern(&pattern, &events, false);
        assert!(result.matched);
    }

    #[test]
    fn test_one_event_gap_too_many() {
        let pattern = parse_pattern("(?1).(?2)").unwrap();
        let events = make_events(&[
            (100, &[true, false]),
            (200, &[false, false]),
            (300, &[false, false]), // two events gap, not one
            (400, &[false, true]),
        ]);
        // The pattern (?1).(?2) requires exactly ONE event between (?1) and (?2)
        // Event at 200 is the "." and event at 300 needs to be (?2) but it's false
        let result = execute_pattern(&pattern, &events, false);
        assert!(!result.matched);
    }

    #[test]
    fn test_time_constraint_satisfied() {
        let pattern = parse_pattern("(?1)(?t>=2)(?2)").unwrap();
        // Timestamps in microseconds, threshold in seconds
        let events = make_events(&[
            (0, &[true, false]),
            (3_000_000, &[false, true]), // 3 seconds later >= 2
        ]);
        let result = execute_pattern(&pattern, &events, false);
        assert!(result.matched);
    }

    #[test]
    fn test_time_constraint_not_satisfied() {
        let pattern = parse_pattern("(?1)(?t>=5)(?2)").unwrap();
        let events = make_events(&[
            (0, &[true, false]),
            (3_000_000, &[false, true]), // 3 seconds < 5
        ]);
        let result = execute_pattern(&pattern, &events, false);
        assert!(!result.matched);
    }

    #[test]
    fn test_count_non_overlapping() {
        let pattern = parse_pattern("(?1)(?2)").unwrap();
        let events = make_events(&[
            (100, &[true, false]),
            (200, &[false, true]),
            (300, &[true, false]),
            (400, &[false, true]),
        ]);
        let result = execute_pattern(&pattern, &events, true);
        assert!(result.matched);
        assert_eq!(result.count, 2);
    }

    #[test]
    fn test_empty_events() {
        let pattern = parse_pattern("(?1)").unwrap();
        let result = execute_pattern(&pattern, &[], false);
        assert!(!result.matched);
        assert_eq!(result.count, 0);
    }

    #[test]
    fn test_no_matching_condition() {
        let pattern = parse_pattern("(?1)").unwrap();
        let events = make_events(&[(100, &[false]), (200, &[false])]);
        let result = execute_pattern(&pattern, &events, false);
        assert!(!result.matched);
    }

    #[test]
    fn test_wildcard_zero_events() {
        // .* can match zero events
        let pattern = parse_pattern("(?1).*(?2)").unwrap();
        let events = make_events(&[
            (100, &[true, true]), // both conditions true on same event
        ]);
        // (?1) matches event[0], .* matches 0 events, (?2) needs event[1] which doesn't exist
        // Actually, (?1) consumes event[0] and advances. .* matches 0 events.
        // (?2) tries event[1] which doesn't exist. So this should NOT match.
        // Unless event[0] has cond[1] = true and we can reuse it...
        // No - each step consumes events. (?1) consumed event[0], so (?2) needs another event.
        let result = execute_pattern(&pattern, &events, false);
        assert!(!result.matched);
    }

    #[test]
    fn test_adjacent_match() {
        let pattern = parse_pattern("(?1).*(?2)").unwrap();
        let events = make_events(&[(100, &[true, false]), (200, &[false, true])]);
        let result = execute_pattern(&pattern, &events, false);
        assert!(result.matched);
    }

    #[test]
    fn test_three_step_with_wildcards() {
        let pattern = parse_pattern("(?1).*(?2).*(?3)").unwrap();
        let events = make_events(&[
            (100, &[true, false, false]),
            (200, &[false, false, false]),
            (300, &[false, true, false]),
            (400, &[false, false, false]),
            (500, &[false, false, true]),
        ]);
        let result = execute_pattern(&pattern, &events, false);
        assert!(result.matched);
    }

    #[test]
    fn test_time_lte_constraint() {
        let pattern = parse_pattern("(?1)(?t<=1)(?2)").unwrap();
        let events = make_events(&[
            (0, &[true, false]),
            (500_000, &[false, true]), // 0.5 seconds <= 1
        ]);
        let result = execute_pattern(&pattern, &events, false);
        assert!(result.matched);
    }

    #[test]
    fn test_max_nfa_states_limit() {
        // A pathological pattern with multiple .* can cause state explosion.
        // The executor should abort after MAX_NFA_STATES iterations and return no match.
        let pattern = parse_pattern("(?1).*.*.*.*(?2)").unwrap();
        // Many events that don't match (?2) force extensive backtracking
        let mut event_data: Vec<(i64, &[bool])> = Vec::new();
        let conds_start: [bool; 2] = [true, false];
        let conds_mid: [bool; 2] = [false, false];
        event_data.push((0, &conds_start));
        for i in 1..100 {
            event_data.push((i, &conds_mid));
        }
        let events = make_events(&event_data);
        // Should not hang; returns no match after hitting the state limit
        let result = execute_pattern(&pattern, &events, false);
        assert!(!result.matched);
    }

    #[test]
    fn test_empty_pattern_steps() {
        // A pattern with no steps should not match anything
        let pattern = CompiledPattern { steps: vec![] };
        let events = make_events(&[(100, &[true])]);
        let result = execute_pattern(&pattern, &events, false);
        assert!(!result.matched);
        assert_eq!(result.count, 0);
    }

    #[test]
    fn test_count_all_no_matches() {
        let pattern = parse_pattern("(?1)(?2)").unwrap();
        let events = make_events(&[(100, &[false, true]), (200, &[false, true])]);
        let result = execute_pattern(&pattern, &events, true);
        assert!(!result.matched);
        assert_eq!(result.count, 0);
    }

    #[test]
    fn test_time_eq_constraint() {
        let pattern = parse_pattern("(?1)(?t==2)(?2)").unwrap();
        let events = make_events(&[
            (0, &[true, false]),
            (2_000_000, &[false, true]), // exactly 2 seconds
        ]);
        let result = execute_pattern(&pattern, &events, false);
        assert!(result.matched);
    }

    #[test]
    fn test_time_ne_constraint() {
        let pattern = parse_pattern("(?1)(?t!=2)(?2)").unwrap();
        let events = make_events(&[
            (0, &[true, false]),
            (3_000_000, &[false, true]), // 3 seconds != 2
        ]);
        let result = execute_pattern(&pattern, &events, false);
        assert!(result.matched);
    }

    #[test]
    fn test_time_gt_constraint() {
        let pattern = parse_pattern("(?1)(?t>5)(?2)").unwrap();
        let events = make_events(&[
            (0, &[true, false]),
            (6_000_000, &[false, true]), // 6 > 5
        ]);
        let result = execute_pattern(&pattern, &events, false);
        assert!(result.matched);
    }

    #[test]
    fn test_time_lt_constraint() {
        let pattern = parse_pattern("(?1)(?t<5)(?2)").unwrap();
        let events = make_events(&[
            (0, &[true, false]),
            (4_000_000, &[false, true]), // 4 < 5
        ]);
        let result = execute_pattern(&pattern, &events, false);
        assert!(result.matched);
    }

    #[test]
    fn test_single_event_single_condition() {
        let pattern = parse_pattern("(?1)").unwrap();
        let events = make_events(&[(100, &[true])]);
        let result = execute_pattern(&pattern, &events, false);
        assert!(result.matched);
    }

    #[test]
    fn test_wildcard_at_end() {
        // .* at the end of pattern should still match
        let pattern = parse_pattern("(?1).*").unwrap();
        let events = make_events(&[(100, &[true]), (200, &[false])]);
        let result = execute_pattern(&pattern, &events, false);
        assert!(result.matched);
    }

    #[test]
    fn test_count_three_non_overlapping() {
        let pattern = parse_pattern("(?1)(?2)").unwrap();
        let events = make_events(&[
            (100, &[true, false]),
            (200, &[false, true]),
            (300, &[true, false]),
            (400, &[false, true]),
            (500, &[true, false]),
            (600, &[false, true]),
        ]);
        let result = execute_pattern(&pattern, &events, true);
        assert_eq!(result.count, 3);
    }

    // --- Session 4: Mutation-killing tests for identified gaps ---

    #[test]
    fn test_one_event_dot_with_time_constraint() {
        // Kills mutant: removing last_match_ts update in OneEvent handler.
        // If `.` doesn't set last_match_ts, the following time constraint
        // would use the wrong baseline timestamp (or None).
        let pattern = parse_pattern("(?1).(?t<=3)(?2)").unwrap();
        let events = make_events(&[
            (0, &[true, false]),
            (1_000_000, &[false, false]), // matched by `.`
            (3_000_000, &[false, true]),  // 2s after the `.` event, <= 3
        ]);
        let result = execute_pattern(&pattern, &events, false);
        assert!(result.matched);

        // Now verify the time constraint uses the `.` event's timestamp, not (?1)'s
        let pattern2 = parse_pattern("(?1).(?t<=1)(?2)").unwrap();
        let events2 = make_events(&[
            (0, &[true, false]),
            (1_000_000, &[false, false]), // matched by `.` at 1s
            (3_000_000, &[false, true]),  // 2s after `.`, > 1s limit
        ]);
        let result2 = execute_pattern(&pattern2, &events2, false);
        assert!(!result2.matched);
    }

    #[test]
    fn test_time_constraint_vacuous_truth_at_pattern_start() {
        // Kills mutant: removing the else branch for time constraints
        // when last_match_ts is None. A time constraint at the start
        // of a pattern has no previous match to compare against and
        // should be vacuously true.
        let pattern = parse_pattern("(?t<=5)(?1)").unwrap();
        let events = make_events(&[(100, &[true])]);
        let result = execute_pattern(&pattern, &events, false);
        assert!(result.matched);
    }

    #[test]
    fn test_time_constraint_microsecond_to_second_conversion() {
        // Kills mutant: replacing `/` with `*` in elapsed_us / MICROS_PER_SECOND.
        // Uses non-trivial values where the division matters.
        // 1_500_000 µs = 1.5s, truncated to 1s.
        // With (?t>=2), 1s < 2s → should NOT match.
        let pattern = parse_pattern("(?1)(?t>=2)(?2)").unwrap();
        let events = make_events(&[
            (0, &[true, false]),
            (1_500_000, &[false, true]), // 1.5s → 1s (integer division) < 2
        ]);
        let result = execute_pattern(&pattern, &events, false);
        assert!(!result.matched);

        // 2_500_000 µs = 2.5s, truncated to 2s. With (?t>=2), 2s >= 2 → match.
        let events2 = make_events(&[(0, &[true, false]), (2_500_000, &[false, true])]);
        let result2 = execute_pattern(&pattern, &events2, false);
        assert!(result2.matched);
    }

    #[test]
    fn test_lazy_matching_prefers_advance_over_consume() {
        // Kills mutant: swapping AnyEvents push order (lazy → greedy).
        // With lazy matching, .* matches as few events as possible,
        // enabling more non-overlapping matches when count_all=true.
        let pattern = parse_pattern("(?1).*(?2)").unwrap();
        let events = make_events(&[
            (100, &[true, false]),
            (200, &[false, true]), // lazy: (?2) matches here immediately
            (300, &[true, false]), // start of second match
            (400, &[false, true]), // lazy: (?2) matches here immediately
        ]);
        let result = execute_pattern(&pattern, &events, true);
        // Lazy: match (0→1), then (2→3) = 2 non-overlapping matches
        assert!(result.matched);
        assert_eq!(result.count, 2);
    }

    #[test]
    fn test_step_completion_boundary() {
        // Kills mutant: replacing `>=` with `>` in step completion check.
        // A pattern with 2 steps should complete when step_idx == 2 == steps.len().
        let pattern = parse_pattern("(?1)(?2)").unwrap();
        assert_eq!(pattern.steps.len(), 2);
        let events = make_events(&[(100, &[true, false]), (200, &[false, true])]);
        let result = execute_pattern(&pattern, &events, false);
        assert!(result.matched);
    }

    #[test]
    fn test_match_end_index_for_non_overlapping_count() {
        // Kills mutant: altering match_end return value logic.
        // Verifies that non-overlapping count correctly advances past the match.
        let pattern = parse_pattern("(?1)(?2)").unwrap();
        // Events: c1, c2, c1, c2, c1, c2
        // Matches: (0,1), (2,3), (4,5) = 3 non-overlapping
        let events = make_events(&[
            (100, &[true, false]),
            (200, &[false, true]),
            (300, &[true, false]),
            (400, &[false, true]),
            (500, &[true, false]),
            (600, &[false, true]),
        ]);
        let result = execute_pattern(&pattern, &events, true);
        assert_eq!(result.count, 3);

        // Adjacent events that share: c1, c1c2, c2
        // First match: event 0 (c1) → event 1 (c2). match_end = 1.
        // search_start = 2. Event 2 has c2 only, no c1. No second match.
        let events2 = make_events(&[
            (100, &[true, false]),
            (200, &[true, true]), // both conditions
            (300, &[false, true]),
        ]);
        let result2 = execute_pattern(&pattern, &events2, true);
        assert_eq!(result2.count, 1);
    }

    #[test]
    fn test_any_events_at_end_of_stream() {
        // Kills mutant: not handling .* at end of stream when events exhausted.
        // .* should match zero remaining events at the end.
        let pattern = parse_pattern("(?1).*").unwrap();
        let events = make_events(&[(100, &[true])]);
        let result = execute_pattern(&pattern, &events, false);
        assert!(result.matched);
    }

    // --- execute_pattern_events tests ---

    #[test]
    fn test_events_simple_match() {
        let pattern = parse_pattern("(?1)(?2)").unwrap();
        let events = make_events(&[(100, &[true, false]), (200, &[false, true])]);
        let result = execute_pattern_events(&pattern, &events);
        assert_eq!(result, Some(vec![100, 200]));
    }

    #[test]
    fn test_events_no_match() {
        let pattern = parse_pattern("(?1)(?2)").unwrap();
        let events = make_events(&[(100, &[false, true]), (200, &[true, false])]);
        let result = execute_pattern_events(&pattern, &events);
        assert_eq!(result, None);
    }

    #[test]
    fn test_events_with_wildcard() {
        let pattern = parse_pattern("(?1).*(?2)").unwrap();
        let events = make_events(&[
            (100, &[true, false]),
            (200, &[false, false]),
            (300, &[false, true]),
        ]);
        let result = execute_pattern_events(&pattern, &events);
        // Only condition timestamps, not wildcard
        assert_eq!(result, Some(vec![100, 300]));
    }

    #[test]
    fn test_events_empty_input() {
        let pattern = parse_pattern("(?1)").unwrap();
        let result = execute_pattern_events(&pattern, &[]);
        assert_eq!(result, None);
    }

    #[test]
    fn test_events_three_conditions() {
        let pattern = parse_pattern("(?1).*(?2).*(?3)").unwrap();
        let events = make_events(&[
            (10, &[true, false, false]),
            (20, &[false, true, false]),
            (30, &[false, false, true]),
        ]);
        let result = execute_pattern_events(&pattern, &events);
        assert_eq!(result, Some(vec![10, 20, 30]));
    }

    #[test]
    fn test_events_with_time_constraint() {
        let pattern = parse_pattern("(?1)(?t>=2)(?2)").unwrap();
        let events = make_events(&[(0, &[true, false]), (3_000_000, &[false, true])]);
        let result = execute_pattern_events(&pattern, &events);
        assert_eq!(result, Some(vec![0, 3_000_000]));
    }

    #[test]
    fn test_events_with_one_event() {
        let pattern = parse_pattern("(?1).(?2)").unwrap();
        let events = make_events(&[
            (100, &[true, false]),
            (200, &[false, false]),
            (300, &[false, true]),
        ]);
        let result = execute_pattern_events(&pattern, &events);
        assert_eq!(result, Some(vec![100, 300]));
    }

    // --- Fast path tests ---

    #[test]
    fn test_fast_adjacent_skip_correctness() {
        // Regression test: the fast_adjacent path must not skip valid starting
        // positions when an intermediate condition check fails.
        // Events: c1c2, c1, c2. Pattern (?1)(?2).
        // Position 0: events[0]=c1c2, events[1]=c1. c1 doesn't have condition 1 → fail.
        // Position 1: events[1]=c1, events[2]=c2. Match!
        let pattern = parse_pattern("(?1)(?2)").unwrap();
        let events = make_events(&[
            (100, &[true, true]),  // c1c2
            (200, &[true, false]), // c1
            (300, &[false, true]), // c2
        ]);
        let result = execute_pattern(&pattern, &events, true);
        assert_eq!(result.count, 1);
    }

    #[test]
    fn test_fast_adjacent_three_step() {
        // Three adjacent conditions: (?1)(?2)(?3)
        let pattern = parse_pattern("(?1)(?2)(?3)").unwrap();
        let events = make_events(&[
            (100, &[true, false, false]),
            (200, &[false, true, false]),
            (300, &[false, false, true]),
        ]);
        let result = execute_pattern(&pattern, &events, false);
        assert!(result.matched);
    }

    #[test]
    fn test_fast_wildcard_count() {
        // Wildcard-separated pattern counting: (?1).*(?2)
        let pattern = parse_pattern("(?1).*(?2)").unwrap();
        let events = make_events(&[
            (100, &[true, false]),
            (200, &[false, false]), // gap
            (300, &[false, true]),
            (400, &[true, false]),
            (500, &[false, true]),
        ]);
        let result = execute_pattern(&pattern, &events, true);
        assert_eq!(result.count, 2);
    }

    #[test]
    fn test_fast_wildcard_no_match() {
        // Wildcard pattern where condition 2 never fires
        let pattern = parse_pattern("(?1).*(?2)").unwrap();
        let events = make_events(&[
            (100, &[true, false]),
            (200, &[true, false]),
            (300, &[true, false]),
        ]);
        let result = execute_pattern(&pattern, &events, false);
        assert!(!result.matched);
    }

    #[test]
    fn test_fast_adjacent_insufficient_events() {
        // Fewer events than pattern steps
        let pattern = parse_pattern("(?1)(?2)(?3)").unwrap();
        let events = make_events(&[(100, &[true, false, false]), (200, &[false, true, false])]);
        let result = execute_pattern(&pattern, &events, false);
        assert!(!result.matched);
    }

    #[test]
    fn test_classify_time_constraint_is_complex() {
        // Patterns with time constraints must use the NFA, not fast paths.
        let pattern = parse_pattern("(?1)(?t<=5)(?2)").unwrap();
        let events = make_events(&[(0, &[true, false]), (3_000_000, &[false, true])]);
        let result = execute_pattern(&pattern, &events, false);
        assert!(result.matched);
    }

    #[test]
    fn test_classify_one_event_is_complex() {
        // Patterns with `.` (OneEvent) must use the NFA.
        let pattern = parse_pattern("(?1).(?2)").unwrap();
        let events = make_events(&[
            (100, &[true, false]),
            (200, &[false, false]),
            (300, &[false, true]),
        ]);
        let result = execute_pattern(&pattern, &events, false);
        assert!(result.matched);
    }

    #[test]
    fn test_time_constraint_after_wildcard() {
        // Kills mutant: incorrect last_match_ts propagation through .*.
        // After .* matches, the time constraint should use the last
        // matched event's timestamp (from before .*), not the current event.
        let pattern = parse_pattern("(?1).*(?t<=3)(?2)").unwrap();
        let events = make_events(&[
            (0, &[true, false]),
            (1_000_000, &[false, false]), // consumed by .*
            (2_000_000, &[false, true]),  // 2s from (?1) match, <= 3
        ]);
        let result = execute_pattern(&pattern, &events, false);
        assert!(result.matched);

        // Time constraint too tight for the gap
        let pattern2 = parse_pattern("(?1).*(?t<=1)(?2)").unwrap();
        let events2 = make_events(&[
            (0, &[true, false]),
            (1_000_000, &[false, false]),
            (5_000_000, &[false, true]), // 5s from (?1), > 1
        ]);
        let result2 = execute_pattern(&pattern2, &events2, false);
        assert!(!result2.matched);
    }
}
