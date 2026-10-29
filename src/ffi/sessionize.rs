// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! FFI registration for the `sessionize` aggregate/window function.
//!
//! `sessionize` is registered as a plain aggregate function set via
//! [`quack_rs::aggregate::AggregateFunctionSetBuilder`]: two overloads with the
//! same fixed arguments, one for `TIMESTAMP` and one for `TIMESTAMPTZ` (the
//! other modules add one overload per condition count). `DuckDB`'s
//! windowing machinery drives any aggregate through the same
//! update/combine/finalize callbacks when it appears in an `OVER` clause —
//! there is no window-specific registration. The segment-tree evaluation is
//! why [`SessionizeBoundaryState::combine`] must be O(1) and why the
//! `current_row_null` flag propagates from the right-hand segment.
//!
//! Historical note: this module was hand-rolled on raw `libduckdb-sys` until
//! quack-rs grew its aggregate builders, whose registration performs the
//! identical C-API call sequence (including default NULL handling, i.e. no
//! `duckdb_aggregate_function_set_special_handling`). See `LESSONS.md` for
//! context on the original decision.

use super::view::{Combine, Finalize, I64Kind, States, Update};
use crate::common::config::invalid_interval_message;
use crate::common::timestamp::interval_to_micros;
use crate::sessionize::SessionizeBoundaryState;
use quack_rs::aggregate::{AggregateFunctionSetBuilder, AggregateOverloadBuilder};
use quack_rs::types::TypeId;

impl quack_rs::aggregate::AggregateState for SessionizeBoundaryState {}

/// Registers the `sessionize` function with `DuckDB`.
///
/// Signature: `sessionize(TIMESTAMP | TIMESTAMPTZ, INTERVAL) → BIGINT`
///
/// Used as a window function:
/// ```sql
/// SELECT sessionize(event_time, INTERVAL '30 minutes')
///   OVER (PARTITION BY user_id ORDER BY event_time)
/// FROM events
/// ```
///
/// # Errors
///
/// Returns an error if function registration fails.
pub fn register_sessionize(
    con: &impl quack_rs::connection::Registrar,
) -> Result<(), quack_rs::error::ExtensionError> {
    // One overload per timestamp type (TIMESTAMP and TIMESTAMPTZ are both
    // int64 microseconds since the epoch, read identically).
    let builder = super::TIMESTAMP_TYPES.into_iter().fold(
        AggregateFunctionSetBuilder::new("sessionize").returns(TypeId::BigInt),
        |set, ts| {
            set.overload(
                AggregateOverloadBuilder::new()
                    .param(ts)
                    .param(TypeId::Interval)
                    .ffi_state::<SessionizeBoundaryState>()
                    .update(state_update)
                    .combine(state_combine)
                    .finalize(state_finalize),
            )
        },
    );
    // SAFETY: `con` is the connection the entry point registers on (this
    // function's contract), and every overload installs `FfiState<T>`'s size,
    // init and destroy callbacks together via `ffi_state::<T>()`, the pairing
    // `Registrar::register_*` requires.
    unsafe { con.register_aggregate_set(builder) }
}

// SAFETY (the three callbacks below): DuckDB calls each with the handles the
// `Update`, `Combine` and `Finalize` constructors require, for the aggregate
// registered in `register_sessionize`: `FfiState<SessionizeBoundaryState>`
// states, arguments (TIMESTAMP | TIMESTAMPTZ, INTERVAL), a BIGINT result.
quack_rs::aggregate_update_callback!(state_update, |info, input, states| {
    // SAFETY: see above.
    let call = unsafe { Update::<SessionizeBoundaryState>::from_raw(info, input, states) };
    update(call);
});

/// `update`: adds each row's timestamp to its state.
fn update(call: Update<SessionizeBoundaryState>) {
    let Update {
        info,
        chunk,
        mut states,
    } = call;
    // SAFETY: column 0 is registered as TIMESTAMP or TIMESTAMPTZ.
    let timestamps = unsafe { chunk.timestamps(0) };
    // SAFETY: column 1 is registered as INTERVAL.
    let gaps = unsafe { chunk.intervals(1) };

    for i in 0..chunk.rows() {
        let Some(state) = states.get_mut(i) else {
            continue;
        };

        // NULL timestamps: mark state so finalize emits NULL for this row
        let Some(timestamp_us) = timestamps.get(i) else {
            state.mark_null_row();
            continue;
        };

        // NULL gap threshold: skip the row entirely
        let Some(gap) = gaps.get(i) else {
            continue;
        };

        // The gap is read per row, because each row's gap judges that row.
        // Invalid gaps (month-based, overflowing or negative) abort the query
        // instead of silently sessionizing with a zero threshold.
        match interval_to_micros(gap.months, gap.days, gap.micros) {
            Some(threshold_us) if threshold_us >= 0 => state.threshold_us = threshold_us,
            Some(_) => {
                info.set_error("sessionize: INTERVAL gap must be non-negative");
                return;
            }
            None => {
                info.set_error(&invalid_interval_message("sessionize", "gap", gap.months));
                return;
            }
        }

        // Rows of one state arrive in window order. A timestamp below the
        // previous one means the window is not ordered by ascending
        // timestamp (`ORDER BY ts DESC`, or another column); the gap check
        // would read every such step as a huge gap and open a session per
        // row, silently. Ties are fine.
        if state.last_ts.is_some_and(|prev| timestamp_us < prev) {
            info.set_error(
                "sessionize: rows must arrive in ascending timestamp order; use \
                 OVER (... ORDER BY <timestamp column>) with an ascending order",
            );
            return;
        }

        state.update(timestamp_us);
    }
}

// `target` is the LEFT (earlier) segment in DuckDB's segment tree, `source`
// the RIGHT (later) one; `SessionizeBoundaryState::combine` preserves that
// orientation (cross-boundary check, right-hand `current_row_null` wins).
quack_rs::aggregate_combine_callback!(state_combine, |info, source, target, count| {
    // SAFETY: see above.
    let Combine {
        info,
        source,
        mut target,
    } = unsafe { Combine::<SessionizeBoundaryState>::from_raw(info, source, target, count) };
    for i in 0..source.count() {
        if let Some((src, tgt)) = States::pair(&source, &mut target, i) {
            // The target is the earlier segment. A source that starts before the
            // target ends was combined out of order (a plain GROUP BY, whose
            // thread-local states DuckDB merges in any order): an error, not a
            // silently different session count.
            if let (Some(left_last), Some(right_first)) = (tgt.last_ts, src.first_ts)
                && right_first < left_last
            {
                info.set_error(
                    "sessionize: rows must arrive in ascending timestamp order; use \
                     OVER (... ORDER BY <timestamp column>) with an ascending order",
                );
                return;
            }
            *tgt = tgt.combine(src);
        }
    }
});

// Empty states and NULL-timestamp rows produce NULL output.
quack_rs::aggregate_finalize_callback!(state_finalize, |info, source, result, count, offset| {
    // SAFETY: see above.
    let Finalize {
        states, mut out, ..
    } = unsafe {
        Finalize::<SessionizeBoundaryState, I64Kind>::from_raw(info, source, result, count, offset)
    };
    for i in 0..out.count() {
        match states.get(i) {
            Some(state) if state.first_ts.is_some() && !state.current_row_null => {
                out.write(i, state.finalize());
            }
            _ => out.set_null(i),
        }
    }
});

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
