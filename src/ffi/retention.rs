// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! FFI registration for the `retention` aggregate function.
//!
//! Uses [`quack_rs::aggregate::AggregateFunctionSetBuilder`] with
//! [`returns_logical`][quack_rs::aggregate::AggregateFunctionSetBuilder::returns_logical]
//! for `LIST(BOOLEAN)` return type registration.
//! Uses [`quack_rs::aggregate::FfiState`] for safe state management,
//! [`quack_rs::vector::VectorReader`] for input, and
//! [`quack_rs::vector::complex::ListVector`] + [`quack_rs::vector::VectorWriter`]
//! for LIST output.

use crate::retention::RetentionState;
use libduckdb_sys::*;
use quack_rs::aggregate::{AggregateFunctionSetBuilder, FfiState};
use quack_rs::types::{LogicalType, TypeId};
use quack_rs::vector::complex::ListVector;
use quack_rs::vector::{VectorReader, VectorWriter};

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
/// Requires a valid connection implementing the [`Registrar`] trait.
///
/// # Errors
///
/// Returns an error if function registration fails.
pub unsafe fn register_retention(
    con: &impl quack_rs::connection::Registrar,
) -> Result<(), quack_rs::error::ExtensionError> {
    let builder = AggregateFunctionSetBuilder::new("retention")
        .returns_logical(LogicalType::list(TypeId::Boolean))
        .overloads(MIN_CONDITIONS..=MAX_CONDITIONS, |n, builder| {
            let mut b = builder;
            for _ in 0..n {
                b = b.param(TypeId::Boolean);
            }
            b.state_size(FfiState::<RetentionState>::size_callback)
                .init(FfiState::<RetentionState>::init_callback)
                .update(state_update)
                .combine(state_combine)
                .finalize(state_finalize)
                .destructor(FfiState::<RetentionState>::destroy_callback)
        });
    unsafe { con.register_aggregate_set(builder) }
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
// valid DuckDB LIST(BOOLEAN) vector. We use ListVector + VectorWriter to write
// entries: reserve space, set size, write list_entry offsets, then write child data.
unsafe extern "C" fn state_finalize(
    _info: duckdb_function_info,
    source: *mut duckdb_aggregate_state,
    result: duckdb_vector,
    count: idx_t,
    offset: idx_t,
) {
    unsafe {
        let mut parent_writer = VectorWriter::new(result);

        for i in 0..count as usize {
            let idx = offset as usize + i;

            let Some(state) = FfiState::<RetentionState>::with_state(*source.add(i)) else {
                parent_writer.set_null(idx);
                continue;
            };

            let retention_result = state.finalize();

            let current_size = ListVector::get_size(result) as u64;
            let new_size = current_size + retention_result.len() as u64;
            ListVector::reserve(result, new_size as usize);

            // Write boolean values to child vector
            let mut child_writer = ListVector::child_writer(result);
            for (j, &val) in retention_result.iter().enumerate() {
                child_writer.write_bool(current_size as usize + j, val);
            }

            ListVector::set_size(result, new_size as usize);
            ListVector::set_entry(result, idx, current_size, retention_result.len() as u64);
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
