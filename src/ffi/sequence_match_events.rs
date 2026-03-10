// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! FFI registration for the `sequence_match_events` aggregate function.
//!
//! Uses [`quack_rs::aggregate::FfiState`] for safe state management,
//! [`quack_rs::vector::VectorReader`] for safe vector reading, and
//! [`quack_rs::vector::complex::ListVector`] + [`quack_rs::vector::VectorWriter`]
//! for LIST output.
//!
//! Registration uses raw `libduckdb-sys` calls because
//! [`quack_rs::aggregate::AggregateFunctionSetBuilder`] does not support
//! parameterized return types like `LIST(TIMESTAMP)`.

use crate::common::event::Event;
use crate::sequence::SequenceState;
use libduckdb_sys::*;
use quack_rs::aggregate::FfiState;
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
/// Requires a valid `duckdb_connection` handle.
pub unsafe fn register_sequence_match_events(con: duckdb_connection) {
    unsafe {
        let name = c"sequence_match_events";
        let set = duckdb_create_aggregate_function_set(name.as_ptr());

        for n in MIN_CONDITIONS..=MAX_CONDITIONS {
            let func = duckdb_create_aggregate_function();
            duckdb_aggregate_function_set_name(func, name.as_ptr());

            // Parameter 0: VARCHAR (pattern)
            let varchar_type = duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_VARCHAR);
            duckdb_aggregate_function_add_parameter(func, varchar_type);
            duckdb_destroy_logical_type(&mut { varchar_type });

            // Parameter 1: TIMESTAMP
            let ts_type = duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_TIMESTAMP);
            duckdb_aggregate_function_add_parameter(func, ts_type);
            duckdb_destroy_logical_type(&mut { ts_type });

            // Parameters 2..2+n: BOOLEAN conditions
            let bool_type = duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_BOOLEAN);
            for _ in 0..n {
                duckdb_aggregate_function_add_parameter(func, bool_type);
            }
            duckdb_destroy_logical_type(&mut { bool_type });

            // Return type: LIST(TIMESTAMP) — cannot use AggregateFunctionSetBuilder
            let list_type =
                quack_rs::types::LogicalType::list(quack_rs::types::TypeId::Timestamp);
            duckdb_aggregate_function_set_return_type(func, list_type.as_raw());

            duckdb_aggregate_function_set_functions(
                func,
                Some(FfiState::<SequenceState>::size_callback),
                Some(FfiState::<SequenceState>::init_callback),
                Some(state_update),
                Some(state_combine),
                Some(state_finalize),
            );

            duckdb_aggregate_function_set_destructor(
                func,
                Some(FfiState::<SequenceState>::destroy_callback),
            );

            duckdb_add_aggregate_function_to_set(set, func);
            duckdb_destroy_aggregate_function(&mut { func });
        }

        let result = duckdb_register_aggregate_function_set(con, set);
        if result != DuckDBSuccess {
            eprintln!("behavioral: failed to register sequence_match_events function set");
        }

        duckdb_destroy_aggregate_function_set(&mut { set });
    }
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
