// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! FFI registration for `sequence_match` and `sequence_count` aggregate functions.
//!
//! Uses [`quack_rs::aggregate::AggregateFunctionSetBuilder`] for function set
//! registration, [`quack_rs::aggregate::FfiState`] for safe state management,
//! and [`quack_rs::vector::VectorReader`] for safe vector reading.

use super::view::{BoolKind, Combine, Finalize, I64Kind, States, Update};
use crate::common::config::{conflict_message, out_of_memory_message};
use crate::common::event::Event;
use crate::pattern::parser::{CompiledPattern, parse_pattern};
use crate::sequence::SequenceState;
use quack_rs::aggregate::AggregateFunctionSetBuilder;
use quack_rs::types::TypeId;
use std::sync::Arc;

/// Minimum number of boolean condition parameters for sequence functions.
const MIN_CONDITIONS: usize = 2;
/// Maximum number of boolean condition parameters for sequence functions.
const MAX_CONDITIONS: usize = 32;

impl quack_rs::aggregate::AggregateState for SequenceState {}

/// Registers the `sequence_match` function with `DuckDB`.
///
/// Signature: `sequence_match(VARCHAR, TIMESTAMP, BOOLEAN, BOOLEAN [, ...]) -> BOOLEAN`
///
/// # Errors
///
/// Returns an error if function registration fails.
pub fn register_sequence_match(
    con: &impl quack_rs::connection::Registrar,
) -> Result<(), quack_rs::error::ExtensionError> {
    let mut builder = AggregateFunctionSetBuilder::new("sequence_match").returns(TypeId::Boolean);
    // The same overloads for TIMESTAMP and TIMESTAMPTZ (both int64
    // microseconds since the epoch, read identically).
    for ts in super::TIMESTAMP_TYPES {
        builder = builder.overloads(MIN_CONDITIONS..=MAX_CONDITIONS, move |n, builder| {
            let mut b = builder.param(TypeId::Varchar).param(ts);
            for _ in 0..n {
                b = b.param(TypeId::Boolean);
            }
            b.ffi_state::<SequenceState>()
                .update(sequence_match_update)
                .combine(sequence_match_combine)
                .finalize(match_state_finalize)
        });
    }
    // SAFETY: `con` is the connection the entry point registers on (this
    // function's contract), and every overload installs `FfiState<T>`'s size,
    // init and destroy callbacks together via `ffi_state::<T>()`, the pairing
    // `Registrar::register_*` requires.
    unsafe { con.register_aggregate_set(builder) }
}

/// Registers the `sequence_count` function with `DuckDB`.
///
/// Signature: `sequence_count(VARCHAR, TIMESTAMP, BOOLEAN, BOOLEAN [, ...]) -> BIGINT`
///
/// # Errors
///
/// Returns an error if function registration fails.
pub fn register_sequence_count(
    con: &impl quack_rs::connection::Registrar,
) -> Result<(), quack_rs::error::ExtensionError> {
    let mut builder = AggregateFunctionSetBuilder::new("sequence_count").returns(TypeId::BigInt);
    // The same overloads for TIMESTAMP and TIMESTAMPTZ (both int64
    // microseconds since the epoch, read identically).
    for ts in super::TIMESTAMP_TYPES {
        builder = builder.overloads(MIN_CONDITIONS..=MAX_CONDITIONS, move |n, builder| {
            let mut b = builder.param(TypeId::Varchar).param(ts);
            for _ in 0..n {
                b = b.param(TypeId::Boolean);
            }
            b.ffi_state::<SequenceState>()
                .update(sequence_count_update)
                .combine(sequence_count_combine)
                .finalize(count_state_finalize)
        });
    }
    // SAFETY: `con` is the connection the entry point registers on (this
    // function's contract), and every overload installs `FfiState<T>`'s size,
    // init and destroy callbacks together via `ffi_state::<T>()`, the pairing
    // `Registrar::register_*` requires.
    unsafe { con.register_aggregate_set(builder) }
}

// SAFETY (the callbacks below): DuckDB calls them with the handles
// `Update::from_raw`, `Combine::from_raw` and `Finalize::from_raw` require, for
// the aggregates registered in `register_sequence_match` and
// `register_sequence_count`: `FfiState<SequenceState>` states, arguments
// (VARCHAR pattern, TIMESTAMP | TIMESTAMPTZ, BOOLEAN...), and a BOOLEAN or
// BIGINT result.

// -- sequence_match finalize --

// A NULL pattern finalizes as NULL; any other error aborts the query.
quack_rs::aggregate_finalize_callback!(
    match_state_finalize,
    |info, source, result, count, offset| {
        // SAFETY: see above.
        let Finalize {
            info,
            mut states,
            mut out,
        } = unsafe {
            Finalize::<SequenceState, BoolKind>::from_raw(info, source, result, count, offset)
        };
        for i in 0..out.count() {
            let Some(state) = states.get_mut(i) else {
                out.set_null(i);
                continue;
            };
            match state.finalize_match() {
                Ok(matched) => out.write(i, matched),
                // A NULL pattern finalizes as NULL; any other error (an
                // invalid pattern) aborts the query.
                Err(_) if state.pattern_str.is_none() => out.set_null(i),
                Err(e) => {
                    info.set_error(&format!("sequence_match: {e}"));
                    out.set_null(i);
                }
            }
        }
    }
);

// -- sequence_count finalize --

quack_rs::aggregate_finalize_callback!(
    count_state_finalize,
    |info, source, result, count, offset| {
        // SAFETY: see above.
        let Finalize {
            info,
            mut states,
            mut out,
        } = unsafe {
            Finalize::<SequenceState, I64Kind>::from_raw(info, source, result, count, offset)
        };
        for i in 0..out.count() {
            let Some(state) = states.get_mut(i) else {
                out.set_null(i);
                continue;
            };
            match state.finalize_count() {
                Ok(n) => out.write(i, n),
                // A NULL pattern finalizes as NULL; any other error (an
                // invalid pattern) aborts the query.
                Err(_) if state.pattern_str.is_none() => out.set_null(i),
                Err(e) => {
                    info.set_error(&format!("sequence_count: {e}"));
                    out.set_null(i);
                }
            }
        }
    }
);

// -- Shared update/combine callbacks --

quack_rs::aggregate_update_callback!(sequence_match_update, |info, input, states| {
    // SAFETY: see above.
    let call = unsafe { Update::<SequenceState>::from_raw(info, input, states) };
    update_impl(call, "sequence_match");
});

quack_rs::aggregate_update_callback!(sequence_count_update, |info, input, states| {
    // SAFETY: see above.
    let call = unsafe { Update::<SequenceState>::from_raw(info, input, states) };
    update_impl(call, "sequence_count");
});

quack_rs::aggregate_combine_callback!(sequence_match_combine, |info, source, target, count| {
    // SAFETY: see above.
    let call = unsafe { Combine::<SequenceState>::from_raw(info, source, target, count) };
    combine_impl(call, "sequence_match");
});

quack_rs::aggregate_combine_callback!(sequence_count_combine, |info, source, target, count| {
    // SAFETY: see above.
    let call = unsafe { Combine::<SequenceState>::from_raw(info, source, target, count) };
    combine_impl(call, "sequence_count");
});

/// Quotes a pattern for an error message, shortening a long one so the
/// message stays readable.
fn quote_pattern(pattern: &str) -> String {
    const SHOWN: usize = 60;
    match pattern.char_indices().nth(SHOWN) {
        Some((cut, _)) => format!("'{}...' ({} bytes)", &pattern[..cut], pattern.len()),
        None => format!("'{pattern}'"),
    }
}

/// Update shared by `sequence_match`, `sequence_count` and
/// `sequence_match_events`: columns (VARCHAR pattern, TIMESTAMP, BOOLEAN...).
///
/// A row with a `NULL` timestamp is skipped entirely, pattern included, so a
/// group whose timestamps are all `NULL` finalizes like an empty group. The
/// first pattern a state sees is parsed and validated (a malformed pattern
/// aborts the query with the parser's position-annotated message); a
/// different pattern later in the group is an error. `func` names the SQL
/// function in error messages.
pub(super) fn update_impl(call: Update<SequenceState>, func: &str) {
    let Update {
        info,
        chunk,
        mut states,
    } = call;
    // SAFETY: column 0 is registered as VARCHAR (the pattern).
    let patterns = unsafe { chunk.varchars(0) };
    // SAFETY: column 1 is registered as TIMESTAMP or TIMESTAMPTZ.
    let timestamps = unsafe { chunk.timestamps(1) };
    // SAFETY: every column after the timestamp is registered as BOOLEAN.
    let conditions = unsafe { chunk.bool_range(2..chunk.cols()) };
    // The last distinct pattern in this chunk: its raw slot, text and
    // compiled form.
    let mut last_pattern: Option<([u8; 16], Arc<str>, Arc<CompiledPattern>)> = None;

    // DuckDB hands consecutive rows of one group the same state, so work per
    // run of equal state pointers: with one large group the whole chunk is
    // one run, and the per-row cost is just building the event.
    let mut start = 0;
    while start < chunk.rows() {
        let end = states.run_end(start);
        let run = start..end;
        start = end;
        let Some(state) = states.get_mut(run.start) else {
            continue;
        };

        // 1. Patterns. Rows of the run that repeat the previous row's raw
        //    slot carry the same string, already checked for this state.
        let mut checked_slot: Option<[u8; 16]> = None;
        for i in run.clone() {
            if !timestamps.is_valid(i) || !patterns.is_valid(i) {
                continue;
            }
            let slot = patterns.slot(i);
            if checked_slot == Some(slot) {
                continue;
            }
            checked_slot = Some(slot);
            // Parse each distinct value once per chunk and share it, so a
            // state takes it without allocating.
            if !matches!(&last_pattern, Some((cached, _, _)) if *cached == slot) {
                let Some(s) = patterns.get(i) else {
                    continue;
                };
                match &mut last_pattern {
                    Some((cached, text, _)) if &**text == s => *cached = slot,
                    _ => match parse_pattern(s) {
                        Err(e) => {
                            info.set_error(&format!(
                                "invalid sequence pattern {}: {e}",
                                quote_pattern(s)
                            ));
                            return;
                        }
                        // As in ClickHouse, a condition number beyond those
                        // passed is an error, not a step that never matches.
                        Ok(p) if p.max_condition().is_some_and(|n| n > conditions.len()) => {
                            info.set_error(&format!(
                                "invalid sequence pattern {}: condition (?{}) is out of \
                                 range; {} conditions were passed",
                                quote_pattern(s),
                                p.max_condition().unwrap_or(0),
                                conditions.len()
                            ));
                            return;
                        }
                        Ok(p) => last_pattern = Some((slot, Arc::from(s), Arc::new(p))),
                    },
                }
            }
            let Some((_, text, compiled)) = last_pattern.as_ref() else {
                continue;
            };
            match &state.pattern_str {
                None => state.set_compiled_pattern(text, compiled),
                Some(existing) if Arc::ptr_eq(existing, text) => {}
                // The same pattern from an earlier chunk: adopt this chunk's
                // copy so later checks compare pointers.
                Some(existing) if existing == text => state.replace_pattern(text, compiled),
                Some(existing) => {
                    info.set_error(&conflict_message(
                        func,
                        "pattern",
                        &quote_pattern(existing),
                        &quote_pattern(text),
                    ));
                    return;
                }
            }
        }

        // 2. Events. Reserve the run once, fallibly (an allocation failure
        //    becomes a SQL error instead of aborting the host process);
        //    `extend` then writes into the reserved space.
        let needed = run.len();
        if state.events.capacity() - state.events.len() < needed
            && state.events.try_reserve(needed).is_err()
        {
            info.set_error(&out_of_memory_message(func, state.events.len() + needed));
            return;
        }
        state.events.extend(run.filter_map(|i| {
            let timestamp_us = timestamps.get(i)?;
            let mut bitmask: u32 = 0;
            for (c, condition) in conditions.iter().enumerate() {
                if condition.is_true(i) {
                    bitmask |= 1 << c;
                }
            }
            // Only events with a true condition can match a `(?N)` step (as
            // in `SequenceState::update`).
            (bitmask != 0).then(|| Event::new(timestamp_us, bitmask))
        }));
    }
}

/// Combine shared by the `sequence_*` functions: appends the source's events
/// and rejects a target and source holding different patterns.
pub(super) fn combine_impl(call: Combine<SequenceState>, func: &str) {
    let Combine {
        info,
        source,
        mut target,
    } = call;
    for i in 0..source.count() {
        let Some((src, tgt)) = States::pair(&source, &mut target, i) else {
            continue;
        };
        if let (Some(a), Some(b)) = (tgt.pattern_str.as_deref(), src.pattern_str.as_deref())
            && a != b
        {
            info.set_error(&conflict_message(
                func,
                "pattern",
                &quote_pattern(a),
                &quote_pattern(b),
            ));
            return;
        }
        if tgt.events.try_reserve(src.events.len()).is_err() {
            info.set_error(&out_of_memory_message(
                func,
                tgt.events.len() + src.events.len(),
            ));
            return;
        }
        tgt.combine_in_place(src);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_patterns_are_shortened_in_errors() {
        assert_eq!(quote_pattern("(?1)(?2)"), "'(?1)(?2)'");
        let long = "é".repeat(100);
        let quoted = quote_pattern(&long);
        assert!(
            quoted.starts_with(&format!("'{}...'", "é".repeat(60))),
            "{quoted}"
        );
        assert!(quoted.ends_with("(200 bytes)"), "{quoted}");
    }
    use quack_rs::testing::AggregateTestHarness;

    #[test]
    fn test_sequence_combine_preserves_events() {
        let mut a = AggregateTestHarness::<SequenceState>::new();
        a.update(|s| {
            s.set_pattern("(?1).*(?2)");
            s.update(Event::new(1_000_000, 0b01));
        });

        let mut b = AggregateTestHarness::<SequenceState>::new();
        b.update(|s| {
            s.set_pattern("(?1).*(?2)");
            s.update(Event::new(2_000_000, 0b10));
        });

        b.combine(&a, |src, tgt| tgt.combine_in_place(src));

        let mut state = b.finalize();
        assert!(state.finalize_match().unwrap());
    }

    #[test]
    fn test_sequence_combine_config_from_zero() {
        // Simulate DuckDB's zero-initialized target combine pattern (Session 10 bug).
        let mut source = AggregateTestHarness::<SequenceState>::new();
        source.update(|s| {
            s.set_pattern("(?1).*(?2)");
            s.update(Event::new(1_000_000, 0b01));
            s.update(Event::new(2_000_000, 0b10));
        });

        let mut target = AggregateTestHarness::<SequenceState>::new();
        // Target is default — no pattern, no events.

        target.combine(&source, |src, tgt| tgt.combine_in_place(src));

        let mut state = target.finalize();
        // Pattern propagates through combine, events are merged.
        assert!(state.pattern_str.is_some());
        assert!(state.finalize_match().unwrap());
    }

    #[test]
    fn test_sequence_match_and_count_consistency() {
        // Same events, same pattern → match iff count > 0.
        let mut state = AggregateTestHarness::<SequenceState>::aggregate(
            vec![
                Event::new(1_000_000, 0b01),
                Event::new(2_000_000, 0b10),
                Event::new(3_000_000, 0b01),
                Event::new(4_000_000, 0b10),
            ],
            |s, event| {
                if s.pattern_str.is_none() {
                    s.set_pattern("(?1).*(?2)");
                }
                s.update(event);
            },
        );

        let matched = state.finalize_match().unwrap();
        let count = state.finalize_count().unwrap();

        assert_eq!(matched, count > 0);
        assert_eq!(count, 2);
    }
}
