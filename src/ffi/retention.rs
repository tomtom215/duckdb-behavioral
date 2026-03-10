// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! FFI registration for the `retention` aggregate function.
//!
//! Uses [`quack_rs::aggregate::FfiState`] for safe state management and
//! [`quack_rs::vector::VectorReader`] for safe vector reading.
//!
//! Registration uses raw `libduckdb-sys` calls because
//! [`quack_rs::aggregate::AggregateFunctionSetBuilder`] does not support
//! parameterized return types like `LIST(BOOLEAN)` — the `returns()` method
//! takes a [`quack_rs::types::TypeId`] which cannot express list element types.

use crate::retention::RetentionState;
use libduckdb_sys::*;
use quack_rs::aggregate::FfiState;
use quack_rs::vector::VectorReader;

/// Minimum number of boolean condition parameters for retention.
const MIN_CONDITIONS: usize = 2;
/// Maximum number of boolean condition parameters for retention.
const MAX_CONDITIONS: usize = 32;

impl quack_rs::aggregate::AggregateState for RetentionState {}

/// Registers the `retention` function with `DuckDB` as a function set
/// with overloads for 2..=32 boolean parameters.
///
/// Signature: `retention(BOOLEAN, BOOLEAN [, BOOLEAN ...]) -> BOOLEAN[]`
///
/// # Safety
///
/// Requires a valid `duckdb_connection` handle.
pub unsafe fn register_retention(con: duckdb_connection) {
    unsafe {
        let name = c"retention";
        let set = duckdb_create_aggregate_function_set(name.as_ptr());

        for n in MIN_CONDITIONS..=MAX_CONDITIONS {
            let func = duckdb_create_aggregate_function();

            // Each function in the set must have a name set
            duckdb_aggregate_function_set_name(func, name.as_ptr());

            // Add n BOOLEAN parameters
            let bool_type = duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_BOOLEAN);
            for _ in 0..n {
                duckdb_aggregate_function_add_parameter(func, bool_type);
            }
            duckdb_destroy_logical_type(&mut { bool_type });

            // Return type: LIST(BOOLEAN) — cannot use AggregateFunctionSetBuilder
            // because it only supports TypeId, not parameterized list types.
            let inner_bool = duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_BOOLEAN);
            let list_type = duckdb_create_list_type(inner_bool);
            duckdb_aggregate_function_set_return_type(func, list_type);
            duckdb_destroy_logical_type(&mut { inner_bool });
            duckdb_destroy_logical_type(&mut { list_type });

            duckdb_aggregate_function_set_functions(
                func,
                Some(FfiState::<RetentionState>::size_callback),
                Some(FfiState::<RetentionState>::init_callback),
                Some(state_update),
                Some(state_combine),
                Some(state_finalize),
            );

            duckdb_aggregate_function_set_destructor(
                func,
                Some(FfiState::<RetentionState>::destroy_callback),
            );

            duckdb_add_aggregate_function_to_set(set, func);
            duckdb_destroy_aggregate_function(&mut { func });
        }

        let result = duckdb_register_aggregate_function_set(con, set);
        if result != DuckDBSuccess {
            eprintln!("behavioral: failed to register retention function set");
        }

        duckdb_destroy_aggregate_function_set(&mut { set });
    }
}

// SAFETY: `input` is a valid DuckDB data chunk with N BOOLEAN columns (as registered).
// `states` points to `row_count` aggregate state pointers initialized by `FfiState::init_callback`.
unsafe extern "C" fn state_update(
    _info: duckdb_function_info,
    input: duckdb_data_chunk,
    states: *mut duckdb_aggregate_state,
) {
    unsafe {
        let row_count = duckdb_data_chunk_get_size(input) as usize;
        let col_count = duckdb_data_chunk_get_column_count(input) as usize;

        // Read all boolean condition vectors using VectorReader
        let readers: Vec<VectorReader> = (0..col_count)
            .map(|c| VectorReader::new(input, c))
            .collect();

        let mut conditions = Vec::with_capacity(col_count);
        for i in 0..row_count {
            let Some(state) = FfiState::<RetentionState>::with_state_mut(*states.add(i)) else {
                continue;
            };

            conditions.clear();
            for reader in &readers {
                let valid = reader.is_valid(i);
                let value = if valid { reader.read_bool(i) } else { false };
                conditions.push(value);
            }

            state.update(&conditions);
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
            let Some(src) = FfiState::<RetentionState>::with_state(*source.add(i)) else {
                continue;
            };
            let Some(tgt) = FfiState::<RetentionState>::with_state_mut(*target.add(i)) else {
                continue;
            };

            let combined = tgt.combine(src);
            *tgt = combined;
        }
    }
}

// SAFETY: `source` points to `count` aggregate state pointers. `result` is a
// valid DuckDB LIST(BOOLEAN) vector. We use DuckDB's list vector APIs to write
// entries: reserve space, set size, write list_entry offsets, then write child data.
unsafe extern "C" fn state_finalize(
    _info: duckdb_function_info,
    source: *mut duckdb_aggregate_state,
    result: duckdb_vector,
    count: idx_t,
    offset: idx_t,
) {
    unsafe {
        // Ensure validity bitmap is writable once before the loop
        duckdb_vector_ensure_validity_writable(result);
        let validity = duckdb_vector_get_validity(result);

        for i in 0..count as usize {
            let idx = offset as usize + i;

            let Some(state) = FfiState::<RetentionState>::with_state(*source.add(i)) else {
                duckdb_validity_set_row_invalid(validity, idx as idx_t);
                continue;
            };

            let retention_result = state.finalize();

            // Write list entry to the result vector
            let child = duckdb_list_vector_get_child(result);
            let current_size = duckdb_list_vector_get_size(result);
            let new_size = current_size + retention_result.len() as idx_t;
            duckdb_list_vector_reserve(result, new_size);
            duckdb_list_vector_set_size(result, new_size);

            // Set the list entry (offset and length)
            let list_data = duckdb_vector_get_data(result) as *mut duckdb_list_entry;
            (*list_data.add(idx)).offset = current_size;
            (*list_data.add(idx)).length = retention_result.len() as idx_t;

            // Write boolean values to child vector
            let child_data = duckdb_vector_get_data(child) as *mut u8;
            for (j, &val) in retention_result.iter().enumerate() {
                *child_data.add(current_size as usize + j) = u8::from(val);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quack_rs::testing::AggregateTestHarness;

    #[test]
    fn test_retention_combine_propagates_config() {
        // Simulate DuckDB's zero-initialized target combine pattern (Session 10 bug).
        let mut source = AggregateTestHarness::<RetentionState>::new();
        source.update(|s| s.update(&[true, true, false]));

        let mut target = AggregateTestHarness::<RetentionState>::new();
        // Target is fresh/default — no updates yet.

        target.combine(&source, |src, tgt| {
            let combined = tgt.combine(src);
            *tgt = combined;
        });

        let state = target.finalize();
        assert_eq!(state.conditions_met, 0b011);
        assert_eq!(state.num_conditions, 3);
    }

    #[test]
    fn test_retention_combine_empty_source() {
        let mut target = AggregateTestHarness::<RetentionState>::new();
        target.update(|s| s.update(&[true, false]));

        let source = AggregateTestHarness::<RetentionState>::new();
        // Source is empty — should not change target.

        target.combine(&source, |src, tgt| {
            let combined = tgt.combine(src);
            *tgt = combined;
        });

        let state = target.finalize();
        assert_eq!(state.conditions_met, 0b01);
        assert_eq!(state.num_conditions, 2);
    }

    #[test]
    fn test_retention_combine_empty_target() {
        let source = AggregateTestHarness::<RetentionState>::with_state({
            let mut s = RetentionState::new();
            s.update(&[true, true]);
            s
        });

        let mut target = AggregateTestHarness::<RetentionState>::new();
        target.combine(&source, |src, tgt| {
            let combined = tgt.combine(src);
            *tgt = combined;
        });

        let state = target.finalize();
        assert_eq!(state.conditions_met, 0b11);
        assert_eq!(state.num_conditions, 2);
    }

    #[test]
    fn test_retention_harness_full_lifecycle() {
        let state = AggregateTestHarness::<RetentionState>::aggregate(
            vec![vec![true, false, true], vec![false, true, false]],
            |s, conditions| s.update(&conditions),
        );
        assert_eq!(state.finalize(), vec![true, true, true]);
    }
}
