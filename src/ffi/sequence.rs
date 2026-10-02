// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! FFI registration for `sequence_match` and `sequence_count` aggregate functions.
//!
//! Uses [`quack_rs::aggregate::AggregateFunctionSetBuilder`] for function set
//! registration, [`quack_rs::aggregate::FfiState`] for safe state management,
//! and [`quack_rs::vector::VectorReader`] for safe vector reading.

use crate::common::config::{conflict_message, out_of_memory_message};
use crate::common::event::Event;
use crate::pattern::parser::parse_pattern;
use crate::sequence::SequenceState;
use libduckdb_sys::*;
use quack_rs::aggregate::{AggregateFunctionInfo, AggregateFunctionSetBuilder, FfiState};
use quack_rs::types::TypeId;
use quack_rs::vector::{VectorReader, VectorWriter};

/// Minimum number of boolean condition parameters for sequence functions.
const MIN_CONDITIONS: usize = 2;
/// Maximum number of boolean condition parameters for sequence functions.
const MAX_CONDITIONS: usize = 32;

impl quack_rs::aggregate::AggregateState for SequenceState {}

/// Registers the `sequence_match` function with `DuckDB`.
///
/// Signature: `sequence_match(VARCHAR, TIMESTAMP, BOOLEAN, BOOLEAN [, ...]) -> BOOLEAN`
///
/// # Safety
///
/// Requires a valid connection implementing the [`Registrar`](quack_rs::connection::Registrar) trait.
///
/// # Errors
///
/// Returns an error if function registration fails.
pub unsafe fn register_sequence_match(
    con: &impl quack_rs::connection::Registrar,
) -> Result<(), quack_rs::error::ExtensionError> {
    let builder = AggregateFunctionSetBuilder::new("sequence_match")
        .returns(TypeId::Boolean)
        .overloads(MIN_CONDITIONS..=MAX_CONDITIONS, |n, builder| {
            let mut b = builder.param(TypeId::Varchar).param(TypeId::Timestamp);
            for _ in 0..n {
                b = b.param(TypeId::Boolean);
            }
            b.ffi_state::<SequenceState>()
                .update(sequence_match_update)
                .combine(sequence_match_combine)
                .finalize(match_state_finalize)
        });
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
/// # Safety
///
/// Requires a valid connection implementing the [`Registrar`](quack_rs::connection::Registrar) trait.
///
/// # Errors
///
/// Returns an error if function registration fails.
pub unsafe fn register_sequence_count(
    con: &impl quack_rs::connection::Registrar,
) -> Result<(), quack_rs::error::ExtensionError> {
    let builder = AggregateFunctionSetBuilder::new("sequence_count")
        .returns(TypeId::BigInt)
        .overloads(MIN_CONDITIONS..=MAX_CONDITIONS, |n, builder| {
            let mut b = builder.param(TypeId::Varchar).param(TypeId::Timestamp);
            for _ in 0..n {
                b = b.param(TypeId::Boolean);
            }
            b.ffi_state::<SequenceState>()
                .update(sequence_count_update)
                .combine(sequence_count_combine)
                .finalize(count_state_finalize)
        });
    // SAFETY: `con` is the connection the entry point registers on (this
    // function's contract), and every overload installs `FfiState<T>`'s size,
    // init and destroy callbacks together via `ffi_state::<T>()`, the pairing
    // `Registrar::register_*` requires.
    unsafe { con.register_aggregate_set(builder) }
}

// -- sequence_match finalize --

// SAFETY: `source` points to `count` aggregate state pointers. `result` is a
// valid DuckDB BOOLEAN vector. A NULL pattern finalizes as NULL; any other error aborts the query.
quack_rs::aggregate_finalize_callback!(
    match_state_finalize,
    |info, source, result, count, offset| {
        unsafe {
            let info = AggregateFunctionInfo::new(info);
            let mut writer = VectorWriter::new(result);

            for i in 0..count as usize {
                let idx = offset as usize + i;

                let Some(state) = FfiState::<SequenceState>::with_state_mut(*source.add(i)) else {
                    writer.set_null(idx);
                    continue;
                };

                match state.finalize_match() {
                    Ok(matched) => writer.write_bool(idx, matched),
                    // A NULL pattern finalizes as NULL; any other error (an
                    // invalid pattern) aborts the query.
                    Err(_) if state.pattern_str.is_none() => writer.set_null(idx),
                    Err(e) => {
                        info.set_error(&format!("sequence_match: {e}"));
                        writer.set_null(idx);
                    }
                }
            }
        }
    }
);

// -- sequence_count finalize --

// SAFETY: `source` points to `count` aggregate state pointers. `result` is a
// valid DuckDB BIGINT vector. A NULL pattern finalizes as NULL; any other error aborts the query.
quack_rs::aggregate_finalize_callback!(
    count_state_finalize,
    |info, source, result, count, offset| {
        unsafe {
            let info = AggregateFunctionInfo::new(info);
            let mut writer = VectorWriter::new(result);

            for i in 0..count as usize {
                let idx = offset as usize + i;

                let Some(state) = FfiState::<SequenceState>::with_state_mut(*source.add(i)) else {
                    writer.set_null(idx);
                    continue;
                };

                match state.finalize_count() {
                    Ok(n) => writer.write_i64(idx, n),
                    // A NULL pattern finalizes as NULL; any other error (an
                    // invalid pattern) aborts the query.
                    Err(_) if state.pattern_str.is_none() => writer.set_null(idx),
                    Err(e) => {
                        info.set_error(&format!("sequence_count: {e}"));
                        writer.set_null(idx);
                    }
                }
            }
        }
    }
);

// -- Shared update/combine callbacks --

// SAFETY: `input` is a valid DuckDB data chunk with columns (VARCHAR, TIMESTAMP,
// BOOLEAN...) as registered. `states` points to `row_count` aggregate state pointers.
quack_rs::aggregate_update_callback!(sequence_match_update, |info, input, states| {
    unsafe { update_impl(info, input, states, "sequence_match") }
});

// SAFETY: as for `sequence_match_update`.
quack_rs::aggregate_update_callback!(sequence_count_update, |info, input, states| {
    unsafe { update_impl(info, input, states, "sequence_count") }
});

// SAFETY: `source` and `target` point to `count` aggregate state pointers.
quack_rs::aggregate_combine_callback!(sequence_match_combine, |info, source, target, count| {
    unsafe { combine_impl(info, source, target, count, "sequence_match") }
});

// SAFETY: as for `sequence_match_combine`.
quack_rs::aggregate_combine_callback!(sequence_count_combine, |info, source, target, count| {
    unsafe { combine_impl(info, source, target, count, "sequence_count") }
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
///
/// # Safety
///
/// Requires a valid `info` handle plus valid `input` data chunk and `states`
/// aggregate state pointers, one initialised state per input row.
pub(super) unsafe fn update_impl(
    info: duckdb_function_info,
    input: duckdb_data_chunk,
    states: *mut duckdb_aggregate_state,
    func: &str,
) {
    // SAFETY: forwarded from this function's contract.
    unsafe {
        let info = AggregateFunctionInfo::new(info);
        let row_count = duckdb_data_chunk_get_size(input) as usize;
        let col_count = duckdb_data_chunk_get_column_count(input) as usize;
        let pattern_reader = VectorReader::new(input, 0);
        let ts_reader = VectorReader::new(input, 1);
        let cond_readers: Vec<VectorReader> = (2..col_count)
            .map(|c| VectorReader::new(input, c))
            .collect();

        for i in 0..row_count {
            let Some(state) = FfiState::<SequenceState>::with_state_mut(*states.add(i)) else {
                continue;
            };
            if !ts_reader.is_valid(i) {
                continue;
            }

            if pattern_reader.is_valid(i) {
                // Compare raw bytes first: the pattern is normally the same on
                // every row, and an equal byte string needs no validation.
                let raw = pattern_reader.read_blob(i);
                if state
                    .pattern_str
                    .as_deref()
                    .is_some_and(|existing| existing.as_bytes() == raw)
                {
                    // Same pattern as the state already holds.
                } else {
                    let s = pattern_reader.read_str(i);
                    match state.pattern_str.as_deref() {
                        Some(existing) if existing == s => {}
                        Some(existing) => {
                            info.set_error(&conflict_message(
                                func,
                                "pattern",
                                &quote_pattern(existing),
                                &quote_pattern(s),
                            ));
                            return;
                        }
                        None => match parse_pattern(s) {
                            Err(e) => {
                                info.set_error(&format!(
                                    "invalid sequence pattern {}: {e}",
                                    quote_pattern(s)
                                ));
                                return;
                            }
                            // As in ClickHouse, a condition number beyond those
                            // passed is an error, not a step that never matches.
                            Ok(p) if p.max_condition().is_some_and(|n| n > cond_readers.len()) => {
                                info.set_error(&format!(
                                    "invalid sequence pattern {}: condition (?{}) is out of \
                                 range; {} conditions were passed",
                                    quote_pattern(s),
                                    p.max_condition().unwrap_or(0),
                                    cond_readers.len()
                                ));
                                return;
                            }
                            Ok(p) => state.set_compiled_pattern(s, p),
                        },
                    }
                }
            }

            let timestamp = ts_reader.read_i64(i);
            let mut bitmask: u32 = 0;
            for (c, reader) in cond_readers.iter().enumerate() {
                if reader.is_valid(i) && reader.read_bool(i) {
                    bitmask |= 1 << c;
                }
            }
            let event = Event::new(timestamp, bitmask);
            // Grow fallibly: an allocation failure becomes a SQL error
            // instead of aborting the host process.
            if event.has_any_condition() && state.events.try_reserve(1).is_err() {
                info.set_error(&out_of_memory_message(func, state.events.len() + 1));
                return;
            }
            state.update(event);
        }
    }
}

/// Combine shared by the `sequence_*` functions: appends the source's events
/// and rejects a target and source holding different patterns.
///
/// # Safety
///
/// Requires a valid `info` handle and `source`/`target` arrays of `count`
/// state pointers, with `source[i]` and `target[i]` distinct states (as
/// `DuckDB` guarantees), so the shared and mutable borrows do not alias.
pub(super) unsafe fn combine_impl(
    info: duckdb_function_info,
    source: *mut duckdb_aggregate_state,
    target: *mut duckdb_aggregate_state,
    count: idx_t,
    func: &str,
) {
    // SAFETY: forwarded from this function's contract.
    unsafe {
        let info = AggregateFunctionInfo::new(info);
        for i in 0..count as usize {
            let Some(src) = FfiState::<SequenceState>::with_state(*source.add(i)) else {
                continue;
            };
            let Some(tgt) = FfiState::<SequenceState>::with_state_mut(*target.add(i)) else {
                continue;
            };
            if let (Some(a), Some(b)) = (tgt.pattern_str.as_deref(), src.pattern_str.as_deref()) {
                if a != b {
                    info.set_error(&conflict_message(
                        func,
                        "pattern",
                        &quote_pattern(a),
                        &quote_pattern(b),
                    ));
                    return;
                }
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
