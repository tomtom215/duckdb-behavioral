// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! NFA-based pattern executor for sequence matching.
//!
//! Executes compiled patterns against sorted event streams using a
//! non-deterministic finite automaton (NFA) with backtracking for `.*` steps.

use crate::common::event::Event;
use crate::common::timestamp::MICROS_PER_SECOND;
use crate::pattern::parser::{CompiledPattern, PatternError, PatternStep, TimeOp};

/// Maximum number of active NFA states before aborting execution.
/// Prevents pathological patterns (e.g., `.*.*.*.*`) from consuming
/// unbounded memory.
/// Floor for the NFA exploration budget. The effective per-start budget
/// scales with input size (see [`nfa_budget`]); this floor covers tiny
/// inputs with adversarial patterns.
const MIN_NFA_BUDGET: usize = 10_000;

/// Per-start NFA exploration budget: `8 * events * steps`, floored at
/// [`MIN_NFA_BUDGET`].
///
/// Lazy exploration of real-world patterns visits O(events × steps) states
/// per starting position, so legitimate inputs stay far below `8 × n × k`.
/// Only adversarial stacks of wildcards (e.g. dozens of consecutive `.*`)
/// can exceed it — and instead of silently reporting "no match", exhaustion
/// surfaces as a [`PatternError`] so the query fails loudly.
fn nfa_budget(num_events: usize, num_steps: usize) -> usize {
    num_events
        .saturating_mul(num_steps.max(1))
        .saturating_mul(8)
        .max(MIN_NFA_BUDGET)
}

/// Builds the budget-exhaustion error.
fn budget_error(num_events: usize, num_steps: usize) -> PatternError {
    PatternError {
        message: format!(
            "pattern exploration budget exceeded ({num_events} events x {num_steps} steps): \
             the pattern is too complex for this input — simplify repeated wildcards \
             or reduce the group size"
        ),
        position: PatternError::NO_POSITION,
    }
}

/// Result of executing a pattern against an event stream.
#[derive(Debug, Clone)]
#[non_exhaustive]
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
) -> Result<MatchResult, PatternError> {
    if events.is_empty() || pattern.steps.is_empty() {
        return Ok(MatchResult {
            matched: false,
            count: 0,
        });
    }

    // Try fast paths for common pattern shapes before falling back to NFA.
    match classify_pattern(pattern) {
        PatternShape::AdjacentConditions(ref conds) => {
            return Ok(fast_adjacent(events, conds, count_all));
        }
        PatternShape::WildcardSeparated(ref conds) => {
            return Ok(fast_wildcard(events, conds, count_all));
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

/// Evaluates a time-constraint gate, mirroring `ClickHouse`'s semantics
/// (see the `TimeConstraint` arms): returns `(advance_pattern, skip_event)`.
///
/// Events are sorted, so the true elapsed time is non-negative and (even
/// spanning ±infinity timestamps) fits in u64; `wrapping_sub` reinterpreted
/// as u64 IS that gap. Dividing in u64 keeps the i64 conversion exact, and
/// flooring to whole seconds generalizes `ClickHouse`'s whole-second
/// `DateTime` comparisons to microsecond timestamps.
fn time_gate(
    op: TimeOp,
    threshold_seconds: i64,
    last_match_ts: Option<i64>,
    event_ts: i64,
) -> (bool, bool) {
    let Some(prev_ts) = last_match_ts else {
        // No previous match timestamp; vacuously satisfied.
        return (true, false);
    };
    let elapsed_us = event_ts.wrapping_sub(prev_ts) as u64;
    let elapsed_seconds = (elapsed_us / MICROS_PER_SECOND as u64) as i64;
    let satisfied = op.evaluate(elapsed_seconds, threshold_seconds);
    // Elapsed time is non-decreasing over later events, so skipping is only
    // useful while the gate can still (or again) hold: always for >=, >, != ;
    // only while still satisfied for <=, < ; until the threshold is passed
    // for ==.
    let skip = match op {
        TimeOp::Gte | TimeOp::Gt | TimeOp::Ne => true,
        TimeOp::Lte | TimeOp::Lt => satisfied,
        TimeOp::Eq => elapsed_seconds <= threshold_seconds,
    };
    (satisfied, skip)
}

/// Full NFA-based pattern execution for complex patterns.
///
/// Used when the pattern contains time constraints, `.` (`OneEvent`),
/// or other structures that cannot be handled by the fast paths.
fn execute_pattern_nfa(
    pattern: &CompiledPattern,
    events: &[Event],
    count_all: bool,
) -> Result<MatchResult, PatternError> {
    let mut total_matches = 0;
    let mut search_start = 0;
    let budget = nfa_budget(events.len(), pattern.steps.len());
    // Pre-allocate the NFA state stack once and reuse across all starting
    // positions. This eliminates per-position heap allocation: instead of
    // O(N) alloc/free pairs, we do O(1) total allocations. The Vec is
    // cleared (retaining capacity) at the start of each try_match_from call.
    let mut states = Vec::with_capacity(pattern.steps.len() * 2);

    while search_start < events.len() {
        if let Some(match_end) = try_match_from(pattern, events, search_start, budget, &mut states)?
        {
            total_matches += 1;
            if !count_all {
                return Ok(MatchResult {
                    matched: true,
                    count: 1,
                });
            }
            // For non-overlapping count, advance past this match
            search_start = match_end + 1;
        } else {
            search_start += 1;
        }
    }

    Ok(MatchResult {
        matched: total_matches > 0,
        count: total_matches,
    })
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
    budget: usize,
    states: &mut Vec<NfaState>,
) -> Result<Option<usize>, PatternError> {
    states.clear();
    states.push(NfaState {
        event_idx: start,
        step_idx: 0,
        last_match_ts: None,
    });

    let mut iterations = 0;

    while let Some(state) = states.pop() {
        iterations += 1;
        if iterations > budget {
            // Adversarial pattern shapes (stacked wildcards) can explode the
            // search space; fail loudly instead of reporting a false "no match".
            return Err(budget_error(events.len(), pattern.steps.len()));
        }

        // Successfully matched all steps
        if state.step_idx >= pattern.steps.len() {
            // Return the index of the last consumed event (one before current)
            return Ok(Some(if state.event_idx > 0 {
                state.event_idx - 1
            } else {
                0
            }));
        }

        // No more events to consume
        if state.event_idx >= events.len() {
            // ClickHouse treats trailing `.*`, `(?t<=N)`, `(?t<N)` and
            // `(?t>=0)` as matching the empty remainder.
            match &pattern.steps[state.step_idx] {
                PatternStep::AnyEvents => {
                    // .* can match zero events, advance to next step
                    states.push(NfaState {
                        step_idx: state.step_idx + 1,
                        ..state
                    });
                }
                PatternStep::TimeConstraint(op, threshold) => {
                    let vacuous_at_end = matches!(op, TimeOp::Lte | TimeOp::Lt)
                        || (matches!(op, TimeOp::Gte) && *threshold == 0);
                    if vacuous_at_end {
                        states.push(NfaState {
                            step_idx: state.step_idx + 1,
                            ..state
                        });
                    }
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
                // ClickHouse semantics (verified against
                // AggregateFunctionSequenceMatch.cpp): the constraint gates
                // the next pattern step without consuming the event, and
                // non-matching events may be skipped — the gate is re-tested
                // against later events whenever it could still be satisfied.
                // Anchored at the last consumed event (`last_match_ts`).
                let (advance_pattern, skip_event) = time_gate(
                    *op,
                    *threshold_seconds,
                    state.last_match_ts,
                    event.timestamp_us,
                );
                // Lazy order: advancing the pattern is explored first
                // (pushed last), mirroring `.*`.
                if skip_event {
                    states.push(NfaState {
                        event_idx: state.event_idx + 1,
                        ..state
                    });
                }
                if advance_pattern {
                    states.push(NfaState {
                        step_idx: state.step_idx + 1,
                        ..state
                    });
                }
            }
        }
    }

    Ok(None)
}

/// Executes a compiled pattern and returns matched condition timestamps.
///
/// Returns timestamps for `(?N)` condition steps only (not `.`, `.*`, or
/// time constraints). Returns `Some(vec![ts1, ts2, ...])` if the pattern
/// matches, `None` if no match is found. Events must be sorted by
/// timestamp (ascending) before calling.
pub fn execute_pattern_events(
    pattern: &CompiledPattern,
    events: &[Event],
) -> Result<Vec<i64>, PatternError> {
    if events.is_empty() || pattern.steps.is_empty() {
        return Ok(Vec::new());
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
) -> Result<Vec<i64>, PatternError> {
    let budget = nfa_budget(events.len(), pattern.steps.len());
    let mut best_partial = Vec::new();
    for start in search_start..search_end {
        if let Some(timestamps) =
            try_match_collecting(pattern, events, start, budget, &mut best_partial)?
        {
            // A complete match: every complete match has the same length, so
            // the first one found (in lazy exploration order, ascending
            // starts) is the result — mirroring ClickHouse.
            return Ok(timestamps);
        }
    }
    // No complete match: ClickHouse's sequenceMatchEvents returns the
    // timestamps of the longest chain matched anywhere (empty when no
    // condition ever fired).
    Ok(best_partial)
}

/// Tries to match from a specific start position, collecting condition timestamps.
fn try_match_collecting(
    pattern: &CompiledPattern,
    events: &[Event],
    start: usize,
    budget: usize,
    best_partial: &mut Vec<i64>,
) -> Result<Option<Vec<i64>>, PatternError> {
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
        if iterations > budget {
            // Fail loudly instead of reporting a false "no match" (see
            // try_match_from).
            return Err(budget_error(events.len(), pattern.steps.len()));
        }

        // ClickHouse's sequenceMatchEvents returns the timestamps of the
        // LONGEST chain matched when the full pattern never matches; track
        // the best partial across the whole exploration.
        if state.collected.len() > best_partial.len() {
            best_partial.clone_from(&state.collected);
        }

        // Successfully matched all steps
        if state.step_idx >= pattern.steps.len() {
            return Ok(Some(state.collected));
        }

        // No more events to consume. ClickHouse treats trailing `.*`,
        // `(?t<=N)`, `(?t<N)` and `(?t>=0)` as matching the empty remainder.
        if state.event_idx >= events.len() {
            match &pattern.steps[state.step_idx] {
                PatternStep::AnyEvents => {
                    states.push(NfaStateWithTimestamps {
                        step_idx: state.step_idx + 1,
                        ..state
                    });
                }
                PatternStep::TimeConstraint(op, threshold) => {
                    let vacuous_at_end = matches!(op, TimeOp::Lte | TimeOp::Lt)
                        || (matches!(op, TimeOp::Gte) && *threshold == 0);
                    if vacuous_at_end {
                        states.push(NfaStateWithTimestamps {
                            step_idx: state.step_idx + 1,
                            ..state
                        });
                    }
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
                // ClickHouse gap-skip semantics — see time_gate.
                let (advance_pattern, skip_event) = time_gate(
                    *op,
                    *threshold_seconds,
                    state.last_match_ts,
                    event.timestamp_us,
                );
                if skip_event {
                    states.push(NfaStateWithTimestamps {
                        event_idx: state.event_idx + 1,
                        ..state.clone()
                    });
                }
                if advance_pattern {
                    states.push(NfaStateWithTimestamps {
                        step_idx: state.step_idx + 1,
                        ..state
                    });
                }
            }
        }
    }

    Ok(None)
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
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(result.matched);
    }

    #[test]
    fn test_simple_no_match() {
        let pattern = parse_pattern("(?1)(?2)").unwrap();
        let events = make_events(&[(100, &[false, true]), (200, &[true, false])]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
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
        let result = execute_pattern(&pattern, &events, false).unwrap();
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
        let result = execute_pattern(&pattern, &events, false).unwrap();
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
        let result = execute_pattern(&pattern, &events, false).unwrap();
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
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(result.matched);
    }

    #[test]
    fn test_time_constraint_not_satisfied() {
        let pattern = parse_pattern("(?1)(?t>=5)(?2)").unwrap();
        let events = make_events(&[
            (0, &[true, false]),
            (3_000_000, &[false, true]), // 3 seconds < 5
        ]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
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
        let result = execute_pattern(&pattern, &events, true).unwrap();
        assert!(result.matched);
        assert_eq!(result.count, 2);
    }

    #[test]
    fn test_empty_events() {
        let pattern = parse_pattern("(?1)").unwrap();
        let result = execute_pattern(&pattern, &[], false).unwrap();
        assert!(!result.matched);
        assert_eq!(result.count, 0);
    }

    #[test]
    fn test_no_matching_condition() {
        let pattern = parse_pattern("(?1)").unwrap();
        let events = make_events(&[(100, &[false]), (200, &[false])]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
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
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(!result.matched);
    }

    #[test]
    fn test_adjacent_match() {
        let pattern = parse_pattern("(?1).*(?2)").unwrap();
        let events = make_events(&[(100, &[true, false]), (200, &[false, true])]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
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
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(result.matched);
    }

    #[test]
    fn test_time_lte_constraint() {
        let pattern = parse_pattern("(?1)(?t<=1)(?2)").unwrap();
        let events = make_events(&[
            (0, &[true, false]),
            (500_000, &[false, true]), // 0.5 seconds <= 1
        ]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
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
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(!result.matched);
    }

    #[test]
    fn test_empty_pattern_steps() {
        // A pattern with no steps should not match anything
        let pattern = CompiledPattern { steps: vec![] };
        let events = make_events(&[(100, &[true])]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(!result.matched);
        assert_eq!(result.count, 0);
    }

    #[test]
    fn test_count_all_no_matches() {
        let pattern = parse_pattern("(?1)(?2)").unwrap();
        let events = make_events(&[(100, &[false, true]), (200, &[false, true])]);
        let result = execute_pattern(&pattern, &events, true).unwrap();
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
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(result.matched);
    }

    #[test]
    fn test_time_ne_constraint() {
        let pattern = parse_pattern("(?1)(?t!=2)(?2)").unwrap();
        let events = make_events(&[
            (0, &[true, false]),
            (3_000_000, &[false, true]), // 3 seconds != 2
        ]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(result.matched);
    }

    #[test]
    fn test_time_gt_constraint() {
        let pattern = parse_pattern("(?1)(?t>5)(?2)").unwrap();
        let events = make_events(&[
            (0, &[true, false]),
            (6_000_000, &[false, true]), // 6 > 5
        ]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(result.matched);
    }

    #[test]
    fn test_time_lt_constraint() {
        let pattern = parse_pattern("(?1)(?t<5)(?2)").unwrap();
        let events = make_events(&[
            (0, &[true, false]),
            (4_000_000, &[false, true]), // 4 < 5
        ]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(result.matched);
    }

    #[test]
    fn test_single_event_single_condition() {
        let pattern = parse_pattern("(?1)").unwrap();
        let events = make_events(&[(100, &[true])]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(result.matched);
    }

    #[test]
    fn test_wildcard_at_end() {
        // .* at the end of pattern should still match
        let pattern = parse_pattern("(?1).*").unwrap();
        let events = make_events(&[(100, &[true]), (200, &[false])]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
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
        let result = execute_pattern(&pattern, &events, true).unwrap();
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
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(result.matched);

        // Now verify the time constraint uses the `.` event's timestamp, not (?1)'s
        let pattern2 = parse_pattern("(?1).(?t<=1)(?2)").unwrap();
        let events2 = make_events(&[
            (0, &[true, false]),
            (1_000_000, &[false, false]), // matched by `.` at 1s
            (3_000_000, &[false, true]),  // 2s after `.`, > 1s limit
        ]);
        let result2 = execute_pattern(&pattern2, &events2, false).unwrap();
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
        let result = execute_pattern(&pattern, &events, false).unwrap();
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
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(!result.matched);

        // 2_500_000 µs = 2.5s, truncated to 2s. With (?t>=2), 2s >= 2 → match.
        let events2 = make_events(&[(0, &[true, false]), (2_500_000, &[false, true])]);
        let result2 = execute_pattern(&pattern, &events2, false).unwrap();
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
        let result = execute_pattern(&pattern, &events, true).unwrap();
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
        let result = execute_pattern(&pattern, &events, false).unwrap();
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
        let result = execute_pattern(&pattern, &events, true).unwrap();
        assert_eq!(result.count, 3);

        // Adjacent events that share: c1, c1c2, c2
        // First match: event 0 (c1) → event 1 (c2). match_end = 1.
        // search_start = 2. Event 2 has c2 only, no c1. No second match.
        let events2 = make_events(&[
            (100, &[true, false]),
            (200, &[true, true]), // both conditions
            (300, &[false, true]),
        ]);
        let result2 = execute_pattern(&pattern, &events2, true).unwrap();
        assert_eq!(result2.count, 1);
    }

    #[test]
    fn test_any_events_at_end_of_stream() {
        // Kills mutant: not handling .* at end of stream when events exhausted.
        // .* should match zero remaining events at the end.
        let pattern = parse_pattern("(?1).*").unwrap();
        let events = make_events(&[(100, &[true])]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(result.matched);
    }

    // --- execute_pattern_events tests ---

    #[test]
    fn test_events_simple_match() {
        let pattern = parse_pattern("(?1)(?2)").unwrap();
        let events = make_events(&[(100, &[true, false]), (200, &[false, true])]);
        let result = execute_pattern_events(&pattern, &events).unwrap();
        assert_eq!(result, vec![100, 200]);
    }

    #[test]
    fn test_events_no_match() {
        // No complete match: ClickHouse's sequenceMatchEvents returns the
        // longest partial chain — here (?1) matched at 200.
        let pattern = parse_pattern("(?1)(?2)").unwrap();
        let events = make_events(&[(100, &[false, true]), (200, &[true, false])]);
        let result = execute_pattern_events(&pattern, &events).unwrap();
        assert_eq!(result, vec![200]);
    }

    #[test]
    fn test_events_with_wildcard() {
        let pattern = parse_pattern("(?1).*(?2)").unwrap();
        let events = make_events(&[
            (100, &[true, false]),
            (200, &[false, false]),
            (300, &[false, true]),
        ]);
        let result = execute_pattern_events(&pattern, &events).unwrap();
        // Only condition timestamps, not wildcard
        assert_eq!(result, vec![100, 300]);
    }

    #[test]
    fn test_events_empty_input() {
        let pattern = parse_pattern("(?1)").unwrap();
        let result = execute_pattern_events(&pattern, &[]).unwrap();
        assert_eq!(result, Vec::<i64>::new());
    }

    #[test]
    fn test_events_three_conditions() {
        let pattern = parse_pattern("(?1).*(?2).*(?3)").unwrap();
        let events = make_events(&[
            (10, &[true, false, false]),
            (20, &[false, true, false]),
            (30, &[false, false, true]),
        ]);
        let result = execute_pattern_events(&pattern, &events).unwrap();
        assert_eq!(result, vec![10, 20, 30]);
    }

    #[test]
    fn test_events_with_time_constraint() {
        let pattern = parse_pattern("(?1)(?t>=2)(?2)").unwrap();
        let events = make_events(&[(0, &[true, false]), (3_000_000, &[false, true])]);
        let result = execute_pattern_events(&pattern, &events).unwrap();
        assert_eq!(result, vec![0, 3_000_000]);
    }

    #[test]
    fn test_events_with_one_event() {
        let pattern = parse_pattern("(?1).(?2)").unwrap();
        let events = make_events(&[
            (100, &[true, false]),
            (200, &[false, false]),
            (300, &[false, true]),
        ]);
        let result = execute_pattern_events(&pattern, &events).unwrap();
        assert_eq!(result, vec![100, 300]);
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
        let result = execute_pattern(&pattern, &events, true).unwrap();
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
        let result = execute_pattern(&pattern, &events, false).unwrap();
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
        let result = execute_pattern(&pattern, &events, true).unwrap();
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
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(!result.matched);
    }

    #[test]
    fn test_fast_adjacent_insufficient_events() {
        // Fewer events than pattern steps
        let pattern = parse_pattern("(?1)(?2)(?3)").unwrap();
        let events = make_events(&[(100, &[true, false, false]), (200, &[false, true, false])]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(!result.matched);
    }

    #[test]
    fn test_classify_time_constraint_is_complex() {
        // Patterns with time constraints must use the NFA, not fast paths.
        let pattern = parse_pattern("(?1)(?t<=5)(?2)").unwrap();
        let events = make_events(&[(0, &[true, false]), (3_000_000, &[false, true])]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
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
        let result = execute_pattern(&pattern, &events, false).unwrap();
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
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(result.matched);

        // Time constraint too tight for the gap
        let pattern2 = parse_pattern("(?1).*(?t<=1)(?2)").unwrap();
        let events2 = make_events(&[
            (0, &[true, false]),
            (1_000_000, &[false, false]),
            (5_000_000, &[false, true]), // 5s from (?1), > 1
        ]);
        let result2 = execute_pattern(&pattern2, &events2, false).unwrap();
        assert!(!result2.matched);
    }

    // --- execute_pattern_events: additional edge case coverage ---

    #[test]
    fn test_events_wildcard_plus_time_constraint() {
        // Verifies timestamp collection when pattern contains both .* and (?t<=N).
        // Only condition timestamps should appear in the result.
        let pattern = parse_pattern("(?1).*(?t<=5)(?2)").unwrap();
        let events = make_events(&[
            (0, &[true, false]),
            (1_000_000, &[false, false]), // consumed by .*
            (3_000_000, &[false, true]),  // 3s from (?1), <= 5
        ]);
        let result = execute_pattern_events(&pattern, &events).unwrap();
        assert_eq!(result, vec![0, 3_000_000]);
    }

    #[test]
    fn test_events_wildcard_time_constraint_fails() {
        // Time constraint not satisfiable: no complete match, so the longest
        // partial chain — just (?1) at t=0 — is returned.
        let pattern = parse_pattern("(?1).*(?t<=1)(?2)").unwrap();
        let events = make_events(&[
            (0, &[true, false]),
            (1_000_000, &[false, false]),
            (5_000_000, &[false, true]), // 5s from (?1), > 1
        ]);
        let result = execute_pattern_events(&pattern, &events).unwrap();
        assert_eq!(result, vec![0]);
    }

    #[test]
    fn test_events_nfa_state_limit() {
        // Consecutive `.*` runs are collapsed by the parser, so the classic
        // pathological shape stays on the fast path and simply reports
        // no-match.
        let pattern = parse_pattern("(?1).*.*.*.*(?2)").unwrap();
        let mut event_data: Vec<(i64, &[bool])> = Vec::new();
        let conds_start: [bool; 2] = [true, false];
        let conds_mid: [bool; 2] = [false, false];
        event_data.push((0, &conds_start));
        for i in 1..100 {
            event_data.push((i, &conds_mid));
        }
        let events = make_events(&event_data);
        let result = execute_pattern_events(&pattern, &events).unwrap();
        // No (?2) anywhere: the longest partial chain is (?1) at t=0.
        assert_eq!(result, vec![0]);

        // A non-normalizable adversarial pattern (wildcards interleaved with
        // time constraints) that exceeds the exploration budget fails LOUDLY
        // instead of silently reporting no-match.
        let adversarial =
            parse_pattern("(?1).*(?t>=0).*(?t>=0).*(?t>=0).*(?t>=0).*(?t>=0).*(?2)").unwrap();
        let mut big: Vec<Event> = vec![Event::from_bools(0, &[true, false])];
        for i in 1..3_000i64 {
            big.push(Event::new(i, 0b100));
        }
        let err = execute_pattern_events(&adversarial, &big).unwrap_err();
        assert!(
            err.message.contains("exploration budget exceeded"),
            "actual: {}",
            err.message
        );
    }

    #[test]
    fn test_events_empty_pattern() {
        // Empty pattern steps match nothing: empty result.
        let pattern = CompiledPattern { steps: vec![] };
        let events = make_events(&[(100, &[true])]);
        let result = execute_pattern_events(&pattern, &events).unwrap();
        assert_eq!(result, Vec::<i64>::new());
    }

    #[test]
    fn test_events_wildcard_zero_events_between_conditions() {
        // .* matching zero events between conditions.
        // (?1) consumes event[0], .* matches zero, (?2) needs event[1].
        let pattern = parse_pattern("(?1).*(?2)").unwrap();
        let events = make_events(&[(100, &[true, false]), (200, &[false, true])]);
        let result = execute_pattern_events(&pattern, &events).unwrap();
        assert_eq!(result, vec![100, 200]);
    }

    #[test]
    fn test_events_one_event_gap_fails() {
        // (?1).(?2) with two gap events — no complete match because `.`
        // matches exactly one event; longest partial is (?1) at 100.
        let pattern = parse_pattern("(?1).(?2)").unwrap();
        let events = make_events(&[
            (100, &[true, false]),
            (200, &[false, false]),
            (300, &[false, false]),
            (400, &[false, true]),
        ]);
        let result = execute_pattern_events(&pattern, &events).unwrap();
        assert_eq!(result, vec![100]);
    }

    #[test]
    fn test_events_wildcard_at_end_of_stream() {
        // .* at end of pattern when events run out. The .* should match
        // zero remaining events and the pattern should succeed.
        let pattern = parse_pattern("(?1).*").unwrap();
        let events = make_events(&[(100, &[true])]);
        let result = execute_pattern_events(&pattern, &events).unwrap();
        // Only one condition timestamp collected
        assert_eq!(result, vec![100]);
    }

    #[test]
    fn test_events_time_constraint_vacuous_truth() {
        // Time constraint at pattern start with no prior timestamp.
        // Should be vacuously true for event collection too.
        let pattern = parse_pattern("(?t<=5)(?1)").unwrap();
        let events = make_events(&[(100, &[true])]);
        let result = execute_pattern_events(&pattern, &events).unwrap();
        assert_eq!(result, vec![100]);
    }

    #[test]
    fn test_events_lazy_matching_collects_earliest() {
        // Verifies lazy matching during event collection: .* prefers
        // advancing the pattern over consuming events, so the earliest
        // matching (?2) timestamp is collected.
        let pattern = parse_pattern("(?1).*(?2)").unwrap();
        let events = make_events(&[
            (100, &[true, false]),
            (200, &[false, true]), // earliest (?2) — lazy match picks this
            (300, &[false, false]),
            (400, &[false, true]), // later (?2) — greedy would pick this
        ]);
        let result = execute_pattern_events(&pattern, &events).unwrap();
        assert_eq!(result, vec![100, 200]);
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;
    use crate::pattern::parser::parse_pattern;

    /// A complex pattern (time constraint forces the NFA path) over a large
    /// gap span must still find the match — the exploration budget must not
    /// produce silent false negatives on legitimate inputs.
    #[test]
    fn test_large_gap_span_still_matches_complex_pattern() {
        let pattern = parse_pattern("(?1).*(?t>=1)(?2)").unwrap();
        let mut events = Vec::new();
        events.push(Event::from_bools(0, &[true, false]));
        for i in 0..50_000i64 {
            // Gap events: condition 3 fires so they pass update() filters,
            // but they match neither pattern condition.
            events.push(Event::new(1_000_000 + i, 0b100));
        }
        events.push(Event::from_bools(60_000_000, &[false, true]));

        let result = execute_pattern(&pattern, &events, false).expect("within budget");
        assert!(
            result.matched,
            "the match exists; budget exhaustion must not hide it"
        );
    }

    /// The events-collecting variant has the same requirement.
    #[test]
    fn test_large_gap_span_events_variant() {
        let pattern = parse_pattern("(?1).(?t>=0)(?2)").unwrap();
        let mut events = Vec::new();
        events.push(Event::from_bools(0, &[true, false]));
        events.push(Event::new(500_000, 0b100));
        events.push(Event::from_bools(1_000_000, &[false, true]));
        // Long tail after the match must not matter.
        for i in 0..20_000i64 {
            events.push(Event::new(2_000_000 + i, 0b100));
        }
        let timestamps = execute_pattern_events(&pattern, &events).expect("within budget");
        assert_eq!(timestamps, vec![0, 1_000_000]);
    }
}

#[cfg(test)]
mod time_semantics_tests {
    use super::*;
    use crate::pattern::parser::parse_pattern;

    /// Elapsed time is floored to whole seconds before comparison — the
    /// faithful generalization of `ClickHouse`'s `DateTime` (whole-second)
    /// behavior to microsecond timestamps. 2.5s elapsed counts as 2.
    #[test]
    fn test_elapsed_seconds_floor_for_lte() {
        let pattern = parse_pattern("(?1)(?t<=2)(?2)").unwrap();
        let events = vec![
            Event::from_bools(0, &[true, false]),
            Event::from_bools(2_500_000, &[false, true]), // 2.5s -> floor 2
        ];
        assert!(execute_pattern(&pattern, &events, false).unwrap().matched);
    }

    /// (?t==N) therefore means "elapsed within [N, N+1) seconds".
    #[test]
    fn test_elapsed_seconds_floor_for_eq() {
        let pattern = parse_pattern("(?1)(?t==2)(?2)").unwrap();
        let events = vec![
            Event::from_bools(0, &[true, false]),
            Event::from_bools(2_900_000, &[false, true]), // 2.9s -> floor 2
        ];
        assert!(execute_pattern(&pattern, &events, false).unwrap().matched);

        let events = vec![
            Event::from_bools(0, &[true, false]),
            Event::from_bools(3_000_000, &[false, true]), // 3.0s -> floor 3
        ];
        assert!(!execute_pattern(&pattern, &events, false).unwrap().matched);
    }

    /// `ClickHouse` gap-skip semantics: `(?1)(?t<=N)(?2)` tolerates
    /// non-matching events between the two conditions.
    #[test]
    fn test_time_constraint_skips_gap_events() {
        let pattern = parse_pattern("(?1)(?t<=10)(?2)").unwrap();
        let events = vec![
            Event::from_bools(0, &[true, false]),
            Event::new(2_000_000, 0b100), // gap event (other condition)
            Event::new(3_000_000, 0b100), // gap event
            Event::from_bools(5_000_000, &[false, true]),
        ];
        assert!(execute_pattern(&pattern, &events, false).unwrap().matched);
    }

    /// The gate still rejects matches outside the window even when skipping.
    #[test]
    fn test_time_constraint_gate_still_enforced_with_gaps() {
        let pattern = parse_pattern("(?1)(?t<=2)(?2)").unwrap();
        let events = vec![
            Event::from_bools(0, &[true, false]),
            Event::new(1_000_000, 0b100), // gap inside window
            Event::from_bools(5_000_000, &[false, true]), // outside window
        ];
        assert!(!execute_pattern(&pattern, &events, false).unwrap().matched);
    }

    /// `ClickHouse` end-of-events rule: trailing `(?t<=N)` / `(?t<N)` /
    /// `(?t>=0)` match the empty remainder.
    #[test]
    fn test_trailing_constraints_vacuous_at_end() {
        let events = vec![Event::from_bools(0, &[true])];
        for pattern_str in ["(?1)(?t<=5)", "(?1)(?t<5)", "(?1)(?t>=0)"] {
            let pattern = parse_pattern(pattern_str).unwrap();
            assert!(
                execute_pattern(&pattern, &events, false).unwrap().matched,
                "{pattern_str} must match at end of events"
            );
        }
        // Trailing constraints that need a future event do not match.
        for pattern_str in ["(?1)(?t>=1)", "(?1)(?t>0)", "(?1)(?t==0)"] {
            let pattern = parse_pattern(pattern_str).unwrap();
            assert!(
                !execute_pattern(&pattern, &events, false).unwrap().matched,
                "{pattern_str} must not match at end of events"
            );
        }
    }

    /// `(?t>=N)` waits past arbitrarily many early events.
    #[test]
    fn test_gte_waits_for_later_events() {
        let pattern = parse_pattern("(?1)(?t>=4)(?2)").unwrap();
        let events = vec![
            Event::from_bools(0, &[true, false]),
            Event::from_bools(1_000_000, &[false, true]), // too early
            Event::from_bools(2_000_000, &[false, true]), // too early
            Event::from_bools(5_000_000, &[false, true]), // 5s >= 4 ✓
        ];
        assert!(execute_pattern(&pattern, &events, false).unwrap().matched);
        let timestamps = execute_pattern_events(&pattern, &events).unwrap();
        assert_eq!(timestamps, vec![0, 5_000_000]);
    }
}
