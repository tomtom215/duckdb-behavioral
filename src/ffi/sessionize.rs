// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! FFI registration for the `sessionize` aggregate/window function.
//!
//! `sessionize` is registered as a plain aggregate function via
//! [`quack_rs::aggregate::AggregateFunctionBuilder`] (single fixed signature,
//! unlike the variadic function sets used by the other modules). `DuckDB`'s
//! windowing machinery drives any aggregate through the same
//! update/combine/finalize callbacks when it appears in an `OVER` clause —
//! there is no window-specific registration. The segment-tree evaluation is
//! why [`SessionizeBoundaryState::combine`] must be O(1) and why the
//! `current_row_null` flag propagates from the right-hand segment.
//!
//! Historical note: this module was hand-rolled on raw `libduckdb-sys` until
//! quack-rs grew `AggregateFunctionBuilder` + `Registrar::register_aggregate`,
//! whose registration performs the identical C-API call sequence (including
//! default NULL handling, i.e. no `duckdb_aggregate_function_set_special_handling`).
//! See `LESSONS.md` for context on the original decision.

use crate::common::timestamp::interval_to_micros;
use crate::sessionize::SessionizeBoundaryState;
use libduckdb_sys::*;
use quack_rs::aggregate::{AggregateFunctionBuilder, FfiState};
use quack_rs::types::TypeId;
use quack_rs::vector::{VectorReader, VectorWriter};

impl quack_rs::aggregate::AggregateState for SessionizeBoundaryState {}

/// Registers the `sessionize` function with `DuckDB`.
///
/// Signature: `sessionize(TIMESTAMP, INTERVAL) → BIGINT`
///
/// Used as a window function:
/// ```sql
/// SELECT sessionize(event_time, INTERVAL '30 minutes')
///   OVER (PARTITION BY user_id ORDER BY event_time)
/// FROM events
/// ```
///
/// # Safety
///
/// Requires a valid connection implementing the [`Registrar`](quack_rs::connection::Registrar) trait.
///
/// # Errors
///
/// Returns an error if function registration fails.
pub unsafe fn register_sessionize(
    con: &impl quack_rs::connection::Registrar,
) -> Result<(), quack_rs::error::ExtensionError> {
    let builder = AggregateFunctionBuilder::new("sessionize")
        .param(TypeId::Timestamp)
        .param(TypeId::Interval)
        .returns(TypeId::BigInt)
        .state_size(FfiState::<SessionizeBoundaryState>::size_callback)
        .init(FfiState::<SessionizeBoundaryState>::init_callback)
        .update(state_update)
        .combine(state_combine)
        .finalize(state_finalize)
        .destructor(FfiState::<SessionizeBoundaryState>::destroy_callback);
    unsafe { con.register_aggregate(builder) }
}

// SAFETY: `input` is a valid DuckDB data chunk with columns (TIMESTAMP, INTERVAL)
// as registered. `states` points to `row_count` aggregate state pointers, each
// initialized by `FfiState::init_callback`.
unsafe extern "C" fn state_update(
    _info: duckdb_function_info,
    input: duckdb_data_chunk,
    states: *mut duckdb_aggregate_state,
) {
    unsafe {
        let row_count = duckdb_data_chunk_get_size(input) as usize;

        // Vector 0: TIMESTAMP (event timestamp)
        let ts_reader = VectorReader::new(input, 0);
        // Vector 1: INTERVAL (gap threshold)
        let interval_reader = VectorReader::new(input, 1);

        for i in 0..row_count {
            let Some(state) = FfiState::<SessionizeBoundaryState>::with_state_mut(*states.add(i))
            else {
                continue;
            };

            // NULL timestamps: mark state so finalize emits NULL for this row
            if !ts_reader.is_valid(i) {
                state.mark_null_row();
                continue;
            }

            // NULL gap threshold: skip the row entirely
            if !interval_reader.is_valid(i) {
                continue;
            }

            // Read interval threshold (same for all rows, but read per-row for safety)
            let iv = interval_reader.read_interval(i);
            if let Some(threshold_us) = interval_to_micros(iv.months, iv.days, iv.micros) {
                state.threshold_us = threshold_us;
            }

            state.update(ts_reader.read_i64(i));
        }
    }
}

// SAFETY: `source` and `target` point to `count` aggregate state pointers.
// `target` is the LEFT (earlier) segment in DuckDB's segment tree, `source`
// the RIGHT (later) one; `SessionizeBoundaryState::combine` preserves that
// orientation (cross-boundary check, right-hand `current_row_null` wins).
unsafe extern "C" fn state_combine(
    _info: duckdb_function_info,
    source: *mut duckdb_aggregate_state,
    target: *mut duckdb_aggregate_state,
    count: idx_t,
) {
    unsafe {
        for i in 0..count as usize {
            let Some(src) = FfiState::<SessionizeBoundaryState>::with_state(*source.add(i)) else {
                continue;
            };
            let Some(tgt) = FfiState::<SessionizeBoundaryState>::with_state_mut(*target.add(i))
            else {
                continue;
            };

            *tgt = tgt.combine(src);
        }
    }
}

// SAFETY: `source` points to `count` aggregate state pointers. `result` is a
// valid DuckDB BIGINT vector with room for `offset + count` elements. Empty
// states and NULL-timestamp rows produce NULL output via the validity bitmap.
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
            let idx = offset as usize + i;

            let Some(state) = FfiState::<SessionizeBoundaryState>::with_state(*source.add(i))
            else {
                writer.set_null(idx);
                continue;
            };

            if state.first_ts.is_none() || state.current_row_null {
                writer.set_null(idx);
            } else {
                writer.write_i64(idx, state.finalize());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quack_rs::testing::AggregateTestHarness;

    /// Mirrors the FFI combine: `target` is the left segment, `source` the right.
    fn ffi_combine(src: &SessionizeBoundaryState, tgt: &mut SessionizeBoundaryState) {
        *tgt = tgt.combine(src);
    }

    #[test]
    fn test_sessionize_combine_threshold_propagation() {
        // DuckDB's segment tree combines sources into zero-initialized targets;
        // the gap threshold must survive that.
        let mut source = AggregateTestHarness::<SessionizeBoundaryState>::new();
        source.update(|s| {
            s.threshold_us = 1_800_000_000; // 30 minutes
            s.update(1_000_000);
        });

        let mut target = AggregateTestHarness::<SessionizeBoundaryState>::new();
        target.combine(&source, ffi_combine);

        let state = target.finalize();
        assert_eq!(state.threshold_us, 1_800_000_000);
        assert_eq!(state.finalize(), 1);
    }

    #[test]
    fn test_sessionize_combine_cross_segment_boundary() {
        // Left segment ends at t=1s, right segment starts at t=10s with a 5s
        // threshold: the cross-segment gap is a session boundary.
        let mut left = AggregateTestHarness::<SessionizeBoundaryState>::new();
        left.update(|s| {
            s.threshold_us = 5_000_000;
            s.update(0);
            s.update(1_000_000);
        });

        let mut right = AggregateTestHarness::<SessionizeBoundaryState>::new();
        right.update(|s| {
            s.threshold_us = 5_000_000;
            s.update(10_000_000);
        });

        left.combine(&right, ffi_combine);

        let state = left.finalize();
        assert_eq!(state.finalize(), 2, "cross-segment gap must open a session");
    }

    #[test]
    fn test_sessionize_combine_null_flag_from_right_segment() {
        // The rightmost leaf in the frame is the current row; its NULL flag
        // must win the combine so finalize emits NULL.
        let mut left = AggregateTestHarness::<SessionizeBoundaryState>::new();
        left.update(|s| {
            s.threshold_us = 5_000_000;
            s.update(0);
        });

        let mut right = AggregateTestHarness::<SessionizeBoundaryState>::new();
        right.update(SessionizeBoundaryState::mark_null_row);

        left.combine(&right, ffi_combine);

        let state = left.finalize();
        assert!(state.current_row_null, "right segment's NULL flag must win");
    }

    #[test]
    fn test_sessionize_combine_three_way_associativity() {
        let make = |timestamps: &[i64]| {
            let mut h = AggregateTestHarness::<SessionizeBoundaryState>::new();
            h.update(|s| {
                s.threshold_us = 2_000_000;
                for &ts in timestamps {
                    s.update(ts);
                }
            });
            h
        };

        // Path 1: (A ⊕ B) ⊕ C
        let mut ab = make(&[0, 1_000_000]);
        ab.combine(&make(&[5_000_000]), ffi_combine);
        ab.combine(&make(&[6_000_000, 20_000_000]), ffi_combine);
        let r1 = ab.finalize().finalize();

        // Path 2: A ⊕ (B ⊕ C)
        let mut bc = make(&[5_000_000]);
        bc.combine(&make(&[6_000_000, 20_000_000]), ffi_combine);
        let mut a = make(&[0, 1_000_000]);
        a.combine(&bc, ffi_combine);
        let r2 = a.finalize().finalize();

        assert_eq!(r1, r2, "combine must be associative");
        assert_eq!(r1, 3, "gaps at 1s→5s and 6s→20s open two boundaries");
    }
}
