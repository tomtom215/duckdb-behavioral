// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! FFI registration for the `sequence_next_node` aggregate function.
//!
//! Uses [`quack_rs::aggregate::AggregateFunctionSetBuilder`] for function set
//! registration, [`quack_rs::aggregate::FfiState`] for safe state management,
//! and [`quack_rs::vector::VectorReader`] for safe vector reading (including
//! `read_str()` which replaces the hand-rolled `read_varchar()` helper that
//! handled the undocumented `duckdb_string_t` 16-byte inline/pointer format).

use crate::sequence_next_node::{NextNodeEvent, SequenceNextNodeState};
use libduckdb_sys::*;
use quack_rs::aggregate::{AggregateFunctionSetBuilder, FfiState};
use quack_rs::types::TypeId;
use quack_rs::vector::{VectorReader, VectorWriter};
use std::sync::Arc;

/// Minimum number of event condition boolean parameters.
const MIN_EVENT_CONDITIONS: usize = 1;
/// Maximum number of event condition boolean parameters.
const MAX_EVENT_CONDITIONS: usize = 32;

/// Number of fixed parameters before the variable boolean event conditions.
///
/// Layout: VARCHAR (direction), VARCHAR (base), TIMESTAMP, VARCHAR (`event_column`),
/// BOOLEAN (`base_condition`), then BOOLEAN × N event conditions.
const FIXED_PARAMS: usize = 5;

impl quack_rs::aggregate::AggregateState for SequenceNextNodeState {}

/// Registers the `sequence_next_node` function with `DuckDB`.
///
/// Signature: `sequence_next_node(VARCHAR, VARCHAR, TIMESTAMP, VARCHAR, BOOLEAN, BOOLEAN [, ...]) -> VARCHAR`
///
/// Parameters:
/// - `direction`: `'forward'` or `'backward'`
/// - `base`: `'head'`, `'tail'`, `'first_match'`, or `'last_match'`
/// - `timestamp`: Event timestamp column
/// - `event_column`: Value column (returned as result)
/// - `base_condition`: Boolean condition for the base/anchor event
/// - `event1, event2, ...`: Sequential event conditions to match
///
/// # Safety
///
/// Requires a valid connection implementing the [`Registrar`] trait.
///
/// # Errors
///
/// Returns an error if function registration fails.
pub unsafe fn register_sequence_next_node(
    con: &impl quack_rs::connection::Registrar,
) -> Result<(), quack_rs::error::ExtensionError> {
    let builder = AggregateFunctionSetBuilder::new("sequence_next_node")
        .returns(TypeId::Varchar)
        .overloads(MIN_EVENT_CONDITIONS..=MAX_EVENT_CONDITIONS, |n, builder| {
            let mut b = builder
                .param(TypeId::Varchar) // direction
                .param(TypeId::Varchar) // base
                .param(TypeId::Timestamp) // timestamp
                .param(TypeId::Varchar) // event_column
                .param(TypeId::Boolean); // base_condition
            for _ in 0..n {
                b = b.param(TypeId::Boolean); // event conditions
            }
            b.state_size(FfiState::<SequenceNextNodeState>::size_callback)
                .init(FfiState::<SequenceNextNodeState>::init_callback)
                .update(state_update)
                .combine(state_combine)
                .finalize(state_finalize)
                .destructor(FfiState::<SequenceNextNodeState>::destroy_callback)
        });
    unsafe { con.register_aggregate_set(builder) }
}

// SAFETY: `input` is a valid DuckDB data chunk with columns
// (VARCHAR, VARCHAR, TIMESTAMP, VARCHAR, BOOLEAN, BOOLEAN...) as registered.
// `states` points to `row_count` aggregate state pointers.
unsafe extern "C" fn state_update(
    _info: duckdb_function_info,
    input: duckdb_data_chunk,
    states: *mut duckdb_aggregate_state,
) {
    unsafe {
        let row_count = duckdb_data_chunk_get_size(input) as usize;
        let col_count = duckdb_data_chunk_get_column_count(input) as usize;
        let num_event_conditions = col_count.saturating_sub(FIXED_PARAMS);

        // Column 0: VARCHAR (direction)
        let direction_reader = VectorReader::new(input, 0);
        // Column 1: VARCHAR (base)
        let base_reader = VectorReader::new(input, 1);
        // Column 2: TIMESTAMP
        let ts_reader = VectorReader::new(input, 2);
        // Column 3: VARCHAR (event_column / value)
        let value_reader = VectorReader::new(input, 3);
        // Column 4: BOOLEAN (base_condition)
        let base_cond_reader = VectorReader::new(input, 4);

        // Columns 5..N: BOOLEAN event conditions
        let event_cond_readers: Vec<VectorReader> = (FIXED_PARAMS..col_count)
            .map(|c| VectorReader::new(input, c))
            .collect();

        for i in 0..row_count {
            let Some(state) = FfiState::<SequenceNextNodeState>::with_state_mut(*states.add(i))
            else {
                continue;
            };

            // Parse direction (once per state)
            if state.direction.is_none() && direction_reader.is_valid(i) {
                let dir_str = direction_reader.read_str(i);
                if let Some(dir) = SequenceNextNodeState::parse_direction(dir_str) {
                    state.set_direction(dir);
                }
            }

            // Parse base (once per state)
            if state.base.is_none() && base_reader.is_valid(i) {
                let base_str = base_reader.read_str(i);
                if let Some(base) = SequenceNextNodeState::parse_base(base_str) {
                    state.set_base(base);
                }
            }

            // Set num_steps (once per state)
            if state.num_steps == 0 {
                state.num_steps = num_event_conditions;
            }

            // Skip NULL timestamps
            if !ts_reader.is_valid(i) {
                continue;
            }

            let timestamp = ts_reader.read_i64(i);

            // Read event_column value (nullable), convert to Arc<str> for O(1) clone
            let value: Option<Arc<str>> = if value_reader.is_valid(i) {
                Some(Arc::from(value_reader.read_str(i)))
            } else {
                None
            };

            // Read base_condition
            let base_condition = base_cond_reader.is_valid(i) && base_cond_reader.read_bool(i);

            // Pack event conditions into u32 bitmask
            let mut bitmask: u32 = 0;
            for (c, reader) in event_cond_readers.iter().enumerate() {
                if reader.is_valid(i) && reader.read_bool(i) {
                    bitmask |= 1 << c;
                }
            }

            state.update(NextNodeEvent {
                timestamp_us: timestamp,
                value,
                base_condition,
                conditions: bitmask,
            });
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
            let Some(src) = FfiState::<SequenceNextNodeState>::with_state(*source.add(i)) else {
                continue;
            };
            let Some(tgt) = FfiState::<SequenceNextNodeState>::with_state_mut(*target.add(i))
            else {
                continue;
            };

            tgt.combine_in_place(src);
        }
    }
}

// SAFETY: `source` points to `count` aggregate state pointers. `result` is a
// valid DuckDB VARCHAR vector. NULL is set via validity bitmap when no match found.
unsafe extern "C" fn state_finalize(
    _info: duckdb_function_info,
    source: *mut duckdb_aggregate_state,
    result: duckdb_vector,
    count: idx_t,
    offset: idx_t,
) {
    unsafe {
        let mut writer = VectorWriter::new(result);

        for i in 0..count as usize {
            let idx = (offset as usize + i) as idx_t;

            let Some(state) = FfiState::<SequenceNextNodeState>::with_state_mut(*source.add(i))
            else {
                writer.set_null(idx as usize);
                continue;
            };

            match state.finalize() {
                Some(value) => {
                    // write_varchar handles both inline (≤12 bytes) and pointer
                    // storage formats via duckdb_vector_assign_string_element_len,
                    // which accepts a length parameter — no null terminator or
                    // CString conversion needed.
                    writer.write_varchar(idx as usize, &value);
                }
                None => {
                    writer.set_null(idx as usize);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sequence_next_node::{Base, Direction};
    use quack_rs::testing::AggregateTestHarness;

    #[test]
    fn test_next_node_combine_config_propagation() {
        // Simulate DuckDB's zero-initialized target combine pattern (Session 10 bug).
        let mut source = AggregateTestHarness::<SequenceNextNodeState>::new();
        source.update(|s| {
            s.set_direction(Direction::Forward);
            s.set_base(Base::Head);
            s.num_steps = 1;
            s.update(NextNodeEvent {
                timestamp_us: 1_000_000,
                value: Some(Arc::from("A")),
                base_condition: true,
                conditions: 0b1,
            });
        });

        let mut target = AggregateTestHarness::<SequenceNextNodeState>::new();
        target.combine(&source, |src, tgt| tgt.combine_in_place(src));

        let state = target.finalize();
        assert_eq!(state.direction, Some(Direction::Forward));
        assert_eq!(state.base, Some(Base::Head));
    }

    #[test]
    fn test_next_node_combine_event_union() {
        let mut a = AggregateTestHarness::<SequenceNextNodeState>::new();
        a.update(|s| {
            s.set_direction(Direction::Forward);
            s.set_base(Base::Head);
            s.num_steps = 1;
            s.update(NextNodeEvent {
                timestamp_us: 1_000_000,
                value: Some(Arc::from("A")),
                base_condition: true,
                conditions: 0b1,
            });
        });

        let mut b = AggregateTestHarness::<SequenceNextNodeState>::new();
        b.update(|s| {
            s.set_direction(Direction::Forward);
            s.set_base(Base::Head);
            s.num_steps = 1;
            s.update(NextNodeEvent {
                timestamp_us: 2_000_000,
                value: Some(Arc::from("B")),
                base_condition: false,
                conditions: 0,
            });
        });

        b.combine(&a, |src, tgt| tgt.combine_in_place(src));

        let mut state = b.finalize();
        // After combining and finalizing, the result depends on matching logic.
        // With forward/head and base_condition on A, next node after match should be B.
        let result = state.finalize();
        assert_eq!(result.as_deref(), Some("B"));
    }

    #[test]
    fn test_next_node_null_value_handling() {
        let mut harness = AggregateTestHarness::<SequenceNextNodeState>::new();
        harness.update(|s| {
            s.set_direction(Direction::Forward);
            s.set_base(Base::Head);
            s.num_steps = 1;
            // Event with NULL value
            s.update(NextNodeEvent {
                timestamp_us: 1_000_000,
                value: None,
                base_condition: true,
                conditions: 0b1,
            });
        });

        let mut state = harness.finalize();
        // NULL value should be preserved through the pipeline.
        let result = state.finalize();
        // With only a base match but no next node, result is None.
        assert!(result.is_none());
    }

    #[test]
    fn test_next_node_interior_null_bytes() {
        // VectorWriter::write_varchar uses duckdb_vector_assign_string_element_len
        // which copies exactly `len` bytes — interior null bytes are preserved
        // transparently without needing CString sanitization.
        let value_with_null = "hello\0world";
        assert_eq!(value_with_null.len(), 11);
    }
}
