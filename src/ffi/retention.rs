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

use super::view::{BoolListKind, Combine, Finalize, States, Update};
use crate::retention::RetentionState;
use quack_rs::aggregate::AggregateFunctionSetBuilder;
use quack_rs::types::{LogicalType, TypeId};

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
/// # Errors
///
/// Returns an error if function registration fails.
pub fn register_retention(
    con: &impl quack_rs::connection::Registrar,
) -> Result<(), quack_rs::error::ExtensionError> {
    let builder = AggregateFunctionSetBuilder::new("retention")
        .returns_logical(LogicalType::list(TypeId::Boolean))
        .overloads(MIN_CONDITIONS..=MAX_CONDITIONS, |n, builder| {
            let mut b = builder;
            for _ in 0..n {
                b = b.param(TypeId::Boolean);
            }
            b.ffi_state::<RetentionState>()
                .update(state_update)
                .combine(state_combine)
                .finalize(state_finalize)
        });
    // SAFETY: `con` is the connection the entry point registers on (this
    // function's contract), and every overload installs `FfiState<T>`'s size,
    // init and destroy callbacks together via `ffi_state::<T>()`, the pairing
    // `Registrar::register_*` requires.
    unsafe { con.register_aggregate_set(builder) }
}

// SAFETY (the callbacks below): DuckDB calls them with the handles
// `Update::from_raw`, `Combine::from_raw` and `Finalize::from_raw` require, for
// the aggregate registered in `register_retention`: `FfiState<RetentionState>`
// states, N BOOLEAN arguments, and a `LIST(BOOLEAN)` result.
quack_rs::aggregate_update_callback!(state_update, |info, input, states| {
    // SAFETY: see above.
    let Update {
        chunk, mut states, ..
    } = unsafe { Update::<RetentionState>::from_raw(info, input, states) };
    // SAFETY: every argument column is registered as BOOLEAN.
    let columns = unsafe { chunk.bool_range(0..chunk.cols()) };

    let mut conditions = Vec::with_capacity(columns.len());
    for i in 0..chunk.rows() {
        let Some(state) = states.get_mut(i) else {
            continue;
        };
        // A NULL condition counts as false.
        conditions.clear();
        conditions.extend(columns.iter().map(|column| column.is_true(i)));
        state.update(&conditions);
    }
});

quack_rs::aggregate_combine_callback!(state_combine, |info, source, target, count| {
    // SAFETY: see above.
    let Combine {
        source, mut target, ..
    } = unsafe { Combine::<RetentionState>::from_raw(info, source, target, count) };
    for i in 0..source.count() {
        if let Some((src, tgt)) = States::pair(&source, &mut target, i) {
            *tgt = tgt.combine(src);
        }
    }
});

quack_rs::aggregate_finalize_callback!(state_finalize, |info, source, result, count, offset| {
    // SAFETY: see above.
    let Finalize {
        states, mut out, ..
    } = unsafe {
        Finalize::<RetentionState, BoolListKind>::from_raw(info, source, result, count, offset)
    };
    for i in 0..out.count() {
        match states.get(i) {
            Some(state) => out.push(i, &state.finalize()),
            None => out.set_null(i),
        }
    }
});

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
