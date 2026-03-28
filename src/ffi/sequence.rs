// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! FFI registration for `sequence_match` and `sequence_count` aggregate functions.
//!
//! Uses [`quack_rs::aggregate::AggregateFunctionSetBuilder`] for function set
//! registration, [`quack_rs::aggregate::FfiState`] for safe state management,
//! and [`quack_rs::vector::VectorReader`] for safe vector reading.

use crate::common::event::Event;
use crate::sequence::SequenceState;
use libduckdb_sys::*;
use quack_rs::aggregate::{AggregateFunctionSetBuilder, FfiState};
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
/// Requires a valid connection implementing the [`Registrar`] trait.
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
            b.state_size(FfiState::<SequenceState>::size_callback)
                .init(FfiState::<SequenceState>::init_callback)
                .update(sequence_state_update)
                .combine(sequence_state_combine)
                .finalize(match_state_finalize)
                .destructor(FfiState::<SequenceState>::destroy_callback)
        });
    unsafe { con.register_aggregate_set(builder) }
}

/// Registers the `sequence_count` function with `DuckDB`.
///
/// Signature: `sequence_count(VARCHAR, TIMESTAMP, BOOLEAN, BOOLEAN [, ...]) -> BIGINT`
///
/// # Safety
///
/// Requires a valid connection implementing the [`Registrar`] trait.
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
            b.state_size(FfiState::<SequenceState>::size_callback)
                .init(FfiState::<SequenceState>::init_callback)
                .update(sequence_state_update)
                .combine(sequence_state_combine)
                .finalize(count_state_finalize)
                .destructor(FfiState::<SequenceState>::destroy_callback)
        });
    unsafe { con.register_aggregate_set(builder) }
}

// -- sequence_match finalize --

// SAFETY: `source` points to `count` aggregate state pointers. `result` is a
// valid DuckDB BOOLEAN vector. Pattern errors produce NULL output via validity bitmap.
unsafe extern "C" fn match_state_finalize(
    _info: duckdb_function_info,
    source: *mut duckdb_aggregate_state,
    result: duckdb_vector,
    count: idx_t,
    offset: idx_t,
) {
    unsafe {
        let mut writer = VectorWriter::new(result);

        for i in 0..count as usize {
            let idx = offset as usize + i;

            let Some(state) = FfiState::<SequenceState>::with_state_mut(*source.add(i)) else {
                writer.set_null(idx);
                continue;
            };

            match state.finalize_match() {
                Ok(matched) => writer.write_bool(idx, matched),
                Err(_) => writer.set_null(idx),
            }
        }
    }
}

// -- sequence_count finalize --

// SAFETY: `source` points to `count` aggregate state pointers. `result` is a
// valid DuckDB BIGINT vector. Pattern errors produce NULL output via validity bitmap.
unsafe extern "C" fn count_state_finalize(
    _info: duckdb_function_info,
    source: *mut duckdb_aggregate_state,
    result: duckdb_vector,
    count: idx_t,
    offset: idx_t,
) {
    unsafe {
        let mut writer = VectorWriter::new(result);

        for i in 0..count as usize {
            let idx = offset as usize + i;

            let Some(state) = FfiState::<SequenceState>::with_state_mut(*source.add(i)) else {
                writer.set_null(idx);
                continue;
            };

            match state.finalize_count() {
                Ok(n) => writer.write_i64(idx, n),
                Err(_) => writer.set_null(idx),
            }
        }
    }
}

// -- Shared update/combine callbacks --

// SAFETY: `input` is a valid DuckDB data chunk with columns (VARCHAR, TIMESTAMP,
// BOOLEAN...) as registered. `states` points to `row_count` aggregate state pointers.
// VARCHAR is read via VectorReader::read_str() which handles duckdb_string_t correctly.
unsafe extern "C" fn sequence_state_update(
    _info: duckdb_function_info,
    input: duckdb_data_chunk,
    states: *mut duckdb_aggregate_state,
) {
    unsafe {
        let row_count = duckdb_data_chunk_get_size(input) as usize;
        let col_count = duckdb_data_chunk_get_column_count(input) as usize;
        // Vector 0: VARCHAR (pattern) — read via VectorReader::read_str()
        let pattern_reader = VectorReader::new(input, 0);

        // Vector 1: TIMESTAMP
        let ts_reader = VectorReader::new(input, 1);

        // Vectors 2..N: BOOLEAN conditions
        let cond_readers: Vec<VectorReader> = (2..col_count)
            .map(|c| VectorReader::new(input, c))
            .collect();

        for i in 0..row_count {
            let Some(state) = FfiState::<SequenceState>::with_state_mut(*states.add(i)) else {
                continue;
            };

            // Read pattern from first row (same for all rows in a group)
            if state.pattern_str.is_none() && pattern_reader.is_valid(i) {
                let s = pattern_reader.read_str(i);
                state.set_pattern(s);
            }

            // Skip NULL timestamps
            if !ts_reader.is_valid(i) {
                continue;
            }

            let timestamp = ts_reader.read_i64(i);

            // Pack conditions into u32 bitmask (max 32 conditions from function set)
            let mut bitmask: u32 = 0;
            for (c, reader) in cond_readers.iter().enumerate() {
                if reader.is_valid(i) && reader.read_bool(i) {
                    bitmask |= 1 << c;
                }
            }

            state.update(Event::new(timestamp, bitmask));
        }
    }
}

// SAFETY: `source` and `target` point to `count` aggregate state pointers.
unsafe extern "C" fn sequence_state_combine(
    _info: duckdb_function_info,
    source: *mut duckdb_aggregate_state,
    target: *mut duckdb_aggregate_state,
    count: idx_t,
) {
    unsafe {
        for i in 0..count as usize {
            let Some(src) = FfiState::<SequenceState>::with_state(*source.add(i)) else {
                continue;
            };
            let Some(tgt) = FfiState::<SequenceState>::with_state_mut(*target.add(i)) else {
                continue;
            };

            tgt.combine_in_place(src);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
