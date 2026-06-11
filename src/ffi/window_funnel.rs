// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! FFI registration for the `window_funnel` aggregate function.
//!
//! Uses [`quack_rs::aggregate::AggregateFunctionSetBuilder`] for function set
//! registration, [`quack_rs::aggregate::FfiState`] for safe state management,
//! and [`quack_rs::vector::VectorReader`] for safe vector reading.

use crate::common::event::Event;
use crate::common::timestamp::interval_to_micros;
use crate::window_funnel::{FunnelMode, WindowFunnelState};
use libduckdb_sys::*;
use quack_rs::aggregate::{AggregateFunctionInfo, AggregateFunctionSetBuilder, FfiState};
use quack_rs::types::TypeId;
use quack_rs::vector::{VectorReader, VectorWriter};

/// Error text listing every SQL mode string `window_funnel` accepts.
const VALID_MODES: &str = "'strict', 'strict_deduplication', 'strict_order', \
     'strict_increase', 'strict_once', 'allow_reentry', 'timestamp_dedup' \
     (comma-separated for combinations)";

/// Minimum number of boolean condition parameters for `window_funnel`.
const MIN_CONDITIONS: usize = 2;
/// Maximum number of boolean condition parameters for `window_funnel`.
const MAX_CONDITIONS: usize = 32;

impl quack_rs::aggregate::AggregateState for WindowFunnelState {}

/// Registers the `window_funnel` function with `DuckDB` as a function set
/// with overloads for two signatures:
///
/// 1. Without mode: `window_funnel(INTERVAL, TIMESTAMP, BOOLEAN, BOOLEAN [, ...]) -> INTEGER`
/// 2. With mode: `window_funnel(INTERVAL, VARCHAR, TIMESTAMP, BOOLEAN, BOOLEAN [, ...]) -> INTEGER`
///
/// The VARCHAR parameter accepts a comma-separated list of mode names
/// (e.g., `'strict_increase, strict_once'`).
///
/// # Safety
///
/// Requires a valid connection implementing the [`Registrar`](quack_rs::connection::Registrar) trait.
///
/// # Errors
///
/// Returns an error if function registration fails.
pub unsafe fn register_window_funnel(
    con: &impl quack_rs::connection::Registrar,
) -> Result<(), quack_rs::error::ExtensionError> {
    // Register both overload groups under the same function set name.
    // DuckDB distinguishes them by parameter types.
    let builder = AggregateFunctionSetBuilder::new("window_funnel")
        .returns(TypeId::Integer)
        // Group 1: WITHOUT mode parameter: (INTERVAL, TIMESTAMP, BOOL×N)
        .overloads(MIN_CONDITIONS..=MAX_CONDITIONS, |n, builder| {
            let mut b = builder.param(TypeId::Interval).param(TypeId::Timestamp);
            for _ in 0..n {
                b = b.param(TypeId::Boolean);
            }
            b.state_size(FfiState::<WindowFunnelState>::size_callback)
                .init(FfiState::<WindowFunnelState>::init_callback)
                .update(state_update)
                .combine(state_combine)
                .finalize(state_finalize)
                .destructor(FfiState::<WindowFunnelState>::destroy_callback)
        })
        // Group 2: WITH mode parameter: (INTERVAL, VARCHAR, TIMESTAMP, BOOL×N)
        .overloads(MIN_CONDITIONS..=MAX_CONDITIONS, |n, builder| {
            let mut b = builder
                .param(TypeId::Interval)
                .param(TypeId::Varchar)
                .param(TypeId::Timestamp);
            for _ in 0..n {
                b = b.param(TypeId::Boolean);
            }
            b.state_size(FfiState::<WindowFunnelState>::size_callback)
                .init(FfiState::<WindowFunnelState>::init_callback)
                .update(state_update_with_mode)
                .combine(state_combine)
                .finalize(state_finalize)
                .destructor(FfiState::<WindowFunnelState>::destroy_callback)
        });
    unsafe { con.register_aggregate_set(builder) }
}

// SAFETY: `input` is a valid DuckDB data chunk with columns (INTERVAL, TIMESTAMP,
// BOOLEAN...) as registered. `states` points to `row_count` aggregate state pointers.
unsafe extern "C" fn state_update(
    info: duckdb_function_info,
    input: duckdb_data_chunk,
    states: *mut duckdb_aggregate_state,
) {
    // No mode parameter: INTERVAL(0), TIMESTAMP(1), BOOLEAN(2..N)
    unsafe {
        update_impl(info, input, states, false);
    }
}

// SAFETY: `input` is a valid DuckDB data chunk with columns (INTERVAL, VARCHAR,
// TIMESTAMP, BOOLEAN...) as registered. The VARCHAR at column 1 contains the mode
// string. `states` points to `row_count` aggregate state pointers.
unsafe extern "C" fn state_update_with_mode(
    info: duckdb_function_info,
    input: duckdb_data_chunk,
    states: *mut duckdb_aggregate_state,
) {
    // With mode parameter: INTERVAL(0), VARCHAR(1), TIMESTAMP(2), BOOLEAN(3..N)
    unsafe {
        update_impl(info, input, states, true);
    }
}

/// Shared update implementation for both signatures.
///
/// When `has_mode` is true, column layout is:
///   \[0\] INTERVAL, \[1\] VARCHAR (mode), \[2\] TIMESTAMP, \[3..N\] BOOLEAN
/// When `has_mode` is false, column layout is:
///   \[0\] INTERVAL, \[1\] TIMESTAMP, \[2..N\] BOOLEAN
///
/// Invalid configuration (month-based or negative window, unknown mode string)
/// aborts the query via [`AggregateFunctionInfo::set_error`] instead of
/// silently producing wrong results.
///
/// # Safety
///
/// Requires a valid `info` handle plus valid `input` data chunk and `states`
/// aggregate state pointers.
unsafe fn update_impl(
    info: duckdb_function_info,
    input: duckdb_data_chunk,
    states: *mut duckdb_aggregate_state,
    has_mode: bool,
) {
    unsafe {
        let info = AggregateFunctionInfo::new(info);
        let row_count = duckdb_data_chunk_get_size(input) as usize;
        let col_count = duckdb_data_chunk_get_column_count(input) as usize;

        // Column indices depend on whether mode parameter is present
        let ts_col: usize = if has_mode { 2 } else { 1 };
        let bool_start: usize = if has_mode { 3 } else { 2 };
        let num_conditions = col_count.saturating_sub(bool_start);

        // Vector 0: INTERVAL (window size) — read via VectorReader
        let interval_reader = VectorReader::new(input, 0);

        // Mode vector (only if has_mode)
        let mode_reader = if has_mode {
            Some(VectorReader::new(input, 1))
        } else {
            None
        };

        // TIMESTAMP vector
        let ts_reader = VectorReader::new(input, ts_col);

        // BOOLEAN condition vectors
        let cond_readers: Vec<VectorReader> = (bool_start..col_count)
            .map(|c| VectorReader::new(input, c))
            .collect();

        for i in 0..row_count {
            let Some(state) = FfiState::<WindowFunnelState>::with_state_mut(*states.add(i)) else {
                continue;
            };

            // Skip NULL timestamps
            if !ts_reader.is_valid(i) {
                continue;
            }

            // Read window size from interval using VectorReader
            if interval_reader.is_valid(i) {
                let iv = interval_reader.read_interval(i);
                match interval_to_micros(iv.months, iv.days, iv.micros) {
                    Some(window_us) if window_us >= 0 => state.window_size_us = window_us,
                    Some(_) => {
                        info.set_error("window_funnel: INTERVAL window must be non-negative");
                        return;
                    }
                    None => {
                        info.set_error(
                            "window_funnel: invalid INTERVAL window: month-based intervals \
                             are ambiguous (28-31 days) and the total must fit in signed \
                             64-bit microseconds; use day/hour/minute/second units instead",
                        );
                        return;
                    }
                }
            }

            // Parse mode string (once per state, from first row that has it)
            if let Some(ref mode_reader) = mode_reader {
                if state.mode.is_default() && mode_reader.is_valid(i) {
                    let s = mode_reader.read_str(i);
                    match FunnelMode::parse_modes(s) {
                        Ok(mode) => state.mode = mode,
                        Err(unknown) => {
                            info.set_error(&format!(
                                "window_funnel: unknown mode '{unknown}'; valid modes are {VALID_MODES}"
                            ));
                            return;
                        }
                    }
                }
            }

            let timestamp = ts_reader.read_i64(i);

            // Pack conditions into u32 bitmask (max 32 conditions from function set)
            let mut bitmask: u32 = 0;
            for (c, reader) in cond_readers.iter().enumerate() {
                if reader.is_valid(i) && reader.read_bool(i) {
                    bitmask |= 1 << c;
                }
            }

            state.update(Event::new(timestamp, bitmask), num_conditions);
        }
    }
}

// SAFETY: `source` and `target` point to `count` aggregate state pointers.
// combine_in_place propagates window_size_us and mode from source to target
// when target has defaults (Session 10 bug fix).
unsafe extern "C" fn state_combine(
    _info: duckdb_function_info,
    source: *mut duckdb_aggregate_state,
    target: *mut duckdb_aggregate_state,
    count: idx_t,
) {
    unsafe {
        for i in 0..count as usize {
            let Some(src) = FfiState::<WindowFunnelState>::with_state(*source.add(i)) else {
                continue;
            };
            let Some(tgt) = FfiState::<WindowFunnelState>::with_state_mut(*target.add(i)) else {
                continue;
            };

            tgt.combine_in_place(src);
        }
    }
}

// SAFETY: `source` points to `count` aggregate state pointers. `result` is a
// valid DuckDB INTEGER vector with room for `offset + count` elements.
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

            let Some(state) = FfiState::<WindowFunnelState>::with_state_mut(*source.add(i)) else {
                writer.set_null(idx);
                continue;
            };

            let step = state.finalize();
            writer.write_i32(idx, step as i32);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quack_rs::testing::AggregateTestHarness;

    #[test]
    fn test_funnel_combine_window_size_propagation() {
        // This is the EXACT bug from Session 10: source has window_size_us=3_600_000_000,
        // target has window_size_us=0 (default). After combine, target must have the
        // source's window_size_us.
        let mut source = AggregateTestHarness::<WindowFunnelState>::new();
        source.update(|s| {
            s.window_size_us = 3_600_000_000; // 1 hour
            s.update(Event::new(1_000_000, 0b01), 2);
        });

        let mut target = AggregateTestHarness::<WindowFunnelState>::new();
        // Target is default — window_size_us = 0.

        target.combine(&source, |src, tgt| tgt.combine_in_place(src));

        let state = target.finalize();
        assert_eq!(state.window_size_us, 3_600_000_000);
    }

    #[test]
    fn test_funnel_combine_mode_propagation() {
        let mut source = AggregateTestHarness::<WindowFunnelState>::new();
        source.update(|s| {
            s.mode = FunnelMode::STRICT_ORDER;
            s.window_size_us = 1_000_000;
            s.update(Event::new(1_000_000, 0b01), 2);
        });

        let mut target = AggregateTestHarness::<WindowFunnelState>::new();
        target.combine(&source, |src, tgt| tgt.combine_in_place(src));

        let state = target.finalize();
        assert_eq!(state.mode, FunnelMode::STRICT_ORDER);
    }

    #[test]
    fn test_funnel_combine_events_merged() {
        let mut a = AggregateTestHarness::<WindowFunnelState>::new();
        a.update(|s| {
            s.window_size_us = 10_000_000; // 10 seconds
            s.update(Event::new(1_000_000, 0b01), 2); // step 1
        });

        let mut b = AggregateTestHarness::<WindowFunnelState>::new();
        b.update(|s| {
            s.window_size_us = 10_000_000;
            s.update(Event::new(2_000_000, 0b10), 2); // step 2
        });

        b.combine(&a, |src, tgt| tgt.combine_in_place(src));

        let mut state = b.finalize();
        // Events from both states merged, should reach step 2.
        let result = state.finalize();
        assert_eq!(result, 2);
    }

    #[test]
    fn test_funnel_combine_three_way_associativity() {
        // (A combine B) combine C should equal A combine (B combine C).
        let events_a = vec![Event::new(1_000_000, 0b001)];
        let events_b = vec![Event::new(2_000_000, 0b010)];
        let events_c = vec![Event::new(3_000_000, 0b100)];

        // Path 1: (A combine B) combine C
        let mut a1 = AggregateTestHarness::<WindowFunnelState>::new();
        a1.update(|s| {
            s.window_size_us = 10_000_000;
            for &e in &events_a {
                s.update(e, 3);
            }
        });
        let mut b1 = AggregateTestHarness::<WindowFunnelState>::new();
        b1.update(|s| {
            s.window_size_us = 10_000_000;
            for &e in &events_b {
                s.update(e, 3);
            }
        });
        b1.combine(&a1, |src, tgt| tgt.combine_in_place(src));
        let mut c1 = AggregateTestHarness::<WindowFunnelState>::new();
        c1.update(|s| {
            s.window_size_us = 10_000_000;
            for &e in &events_c {
                s.update(e, 3);
            }
        });
        c1.combine(&b1, |src, tgt| tgt.combine_in_place(src));
        let mut result1 = c1.finalize();
        let r1 = result1.finalize();

        // Path 2: A combine (B combine C)
        let mut b2 = AggregateTestHarness::<WindowFunnelState>::new();
        b2.update(|s| {
            s.window_size_us = 10_000_000;
            for &e in &events_b {
                s.update(e, 3);
            }
        });
        let mut c2 = AggregateTestHarness::<WindowFunnelState>::new();
        c2.update(|s| {
            s.window_size_us = 10_000_000;
            for &e in &events_c {
                s.update(e, 3);
            }
        });
        c2.combine(&b2, |src, tgt| tgt.combine_in_place(src));
        let mut a2 = AggregateTestHarness::<WindowFunnelState>::new();
        a2.update(|s| {
            s.window_size_us = 10_000_000;
            for &e in &events_a {
                s.update(e, 3);
            }
        });
        c2.combine(&a2, |src, tgt| tgt.combine_in_place(src));
        let mut result2 = c2.finalize();
        let r2 = result2.finalize();

        assert_eq!(r1, r2, "combine must be associative");
    }
}
