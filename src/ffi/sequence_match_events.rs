// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! FFI registration for the `sequence_match_events` aggregate function.
//!
//! Uses [`quack_rs::aggregate::AggregateFunctionSetBuilder`] with
//! [`returns_logical`][quack_rs::aggregate::AggregateFunctionSetBuilder::returns_logical]
//! for `LIST(TIMESTAMP)` return type registration.
//! Uses [`quack_rs::aggregate::FfiState`] for safe state management,
//! [`quack_rs::vector::VectorReader`] for input, and
//! [`quack_rs::vector::complex::ListVector`] + [`quack_rs::vector::VectorWriter`]
//! for LIST output.

use crate::common::event::Event;
use crate::sequence::SequenceState;
use libduckdb_sys::*;
use quack_rs::aggregate::{AggregateFunctionSetBuilder, FfiState};
use quack_rs::types::{LogicalType, TypeId};
use quack_rs::vector::complex::ListVector;
use quack_rs::vector::VectorReader;

/// Minimum number of boolean condition parameters for sequence functions.
const MIN_CONDITIONS: usize = 2;
/// Maximum number of boolean condition parameters for sequence functions.
const MAX_CONDITIONS: usize = 32;

// Note: AggregateState for SequenceState is implemented in ffi/sequence.rs.

/// Registers the `sequence_match_events` function with `DuckDB`.
///
/// Signature: `sequence_match_events(VARCHAR, TIMESTAMP, BOOLEAN, BOOLEAN [, ...]) -> LIST(TIMESTAMP)`
///
/// Returns an array of timestamps corresponding to each matched `(?N)` step in
/// the pattern. Empty array if no match.
///
/// # Safety
///
/// Requires a valid connection implementing the [`Registrar`] trait.
///
/// # Errors
///
/// Returns an error if function registration fails.
pub unsafe fn register_sequence_match_events(
    con: &impl quack_rs::connection::Registrar,
) -> Result<(), quack_rs::error::ExtensionError> {
    let builder = AggregateFunctionSetBuilder::new("sequence_match_events")
        .returns_logical(LogicalType::list(TypeId::Timestamp))
        .overloads(MIN_CONDITIONS..=MAX_CONDITIONS, |n, builder| {
            let mut b = builder.param(TypeId::Varchar).param(TypeId::Timestamp);
            for _ in 0..n {
                b = b.param(TypeId::Boolean);
            }
            b.state_size(FfiState::<SequenceState>::size_callback)
                .init(FfiState::<SequenceState>::init_callback)
                .update(state_update)
                .combine(state_combine)
                .finalize(state_finalize)
                .destructor(FfiState::<SequenceState>::destroy_callback)
        });
    unsafe { con.register_aggregate_set(builder) }
}

// SAFETY: `input` is a valid DuckDB data chunk with columns (VARCHAR, TIMESTAMP,
// BOOLEAN...) as registered. `states` points to `row_count` aggregate state pointers.
unsafe extern "C" fn state_update(
    _info: duckdb_function_info,
    input: duckdb_data_chunk,
    states: *mut duckdb_aggregate_state,
) {
    unsafe {
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

            if state.pattern_str.is_none() && pattern_reader.is_valid(i) {
                let s = pattern_reader.read_str(i);
                state.set_pattern(s);
            }

            if !ts_reader.is_valid(i) {
                continue;
            }

            let timestamp = ts_reader.read_i64(i);

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
unsafe extern "C" fn state_combine(
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

// SAFETY: `source` points to `count` aggregate state pointers. `result` is a
// valid DuckDB LIST(TIMESTAMP) vector. Each list entry is populated with the
// matched condition timestamps. Empty list on no match or pattern error.
unsafe extern "C" fn state_finalize(
    _info: duckdb_function_info,
    source: *mut duckdb_aggregate_state,
    result: duckdb_vector,
    count: idx_t,
    offset: idx_t,
) {
    unsafe {
        let mut list_offset = ListVector::get_size(result) as u64;

        for i in 0..count as usize {
            let idx = offset as usize + i;

            let Some(state) = FfiState::<SequenceState>::with_state_mut(*source.add(i)) else {
                // Empty list for null state
                ListVector::set_entry(result, idx, list_offset, 0);
                continue;
            };

            let timestamps = state.finalize_events().unwrap_or_default();
            let ts_count = timestamps.len() as u64;

            // Reserve space in the list child vector
            ListVector::reserve(result, (list_offset + ts_count) as usize);

            // Write timestamps into the child vector
            let mut child_writer = ListVector::child_writer(result);
            for (j, &ts) in timestamps.iter().enumerate() {
                child_writer.write_i64(list_offset as usize + j, ts);
            }

            // Set the list entry metadata
            ListVector::set_entry(result, idx, list_offset, ts_count);

            list_offset += ts_count;
            ListVector::set_size(result, list_offset as usize);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quack_rs::testing::AggregateTestHarness;

    #[test]
    fn test_sequence_events_empty_pattern() {
        let mut state = AggregateTestHarness::<SequenceState>::aggregate(
            vec![Event::new(1_000_000, 0b01), Event::new(2_000_000, 0b10)],
            |s, event| {
                if s.pattern_str.is_none() {
                    s.set_pattern("(?3)"); // condition 3 never fires
                }
                s.update(event);
            },
        );
        let events = state.finalize_events().unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn test_sequence_events_combine_timestamp_union() {
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
        let events = state.finalize_events().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0], 1_000_000);
        assert_eq!(events[1], 2_000_000);
    }

    #[test]
    fn test_sequence_events_config_propagation() {
        // Zero-initialized target combine pattern (Session 10 bug).
        let mut source = AggregateTestHarness::<SequenceState>::new();
        source.update(|s| {
            s.set_pattern("(?1).*(?2)");
            s.update(Event::new(1_000_000, 0b01));
            s.update(Event::new(2_000_000, 0b10));
        });

        let mut target = AggregateTestHarness::<SequenceState>::new();
        target.combine(&source, |src, tgt| tgt.combine_in_place(src));

        let mut state = target.finalize();
        assert!(state.pattern_str.is_some());
        let events = state.finalize_events().unwrap();
        assert_eq!(events.len(), 2);
    }
}
