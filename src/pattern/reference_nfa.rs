// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! The original backtracking NFA matcher, kept verbatim as a test oracle.
//!
//! Production matching lives in [`super::executor`]; it replaced this search
//! because the lazy depth-first exploration re-visits the same suffixes from
//! every start, which is quadratic for patterns mixing `.*` with a time
//! constraint. The semantics (lazy `.*`, gap-skipping time gates, vacuous
//! trailing gates, first-match and longest-partial rules) are defined by this
//! code, and the differential tests in `executor` check the new matcher
//! against it on randomized inputs.

use crate::common::event::Event;
use crate::common::timestamp::MICROS_PER_SECOND;
use crate::pattern::executor::MatchResult;
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
pub fn execute_pattern_nfa(
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
        if let Some(next) = try_match_from(pattern, events, search_start, budget, &mut states)? {
            total_matches += 1;
            if !count_all {
                return Ok(MatchResult {
                    matched: true,
                    count: 1,
                });
            }
            // Non-overlapping count: continue after the match. A match that
            // consumed no events (e.g. `.*`) still advances one event, as
            // ClickHouse does; otherwise the loop would never end.
            search_start = next.max(search_start + 1);
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
/// Returns `Some(next)` if a full match is found, where `next` is the index of
/// the first event after the match (equal to `start` for a match that consumed
/// no events), or `None` if no match is possible from this starting position.
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
            return Ok(Some(state.event_idx));
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
