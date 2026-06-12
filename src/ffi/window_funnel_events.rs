// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! FFI registration for the `window_funnel_events` aggregate function.
//!
//! `window_funnel_events` shares [`WindowFunnelState`] and the update/combine
//! callbacks with `window_funnel` (see [`super::window_funnel`]) — it differs
//! only in finalize, which returns the winning chain's step timestamps as
//! `LIST(TIMESTAMP)` via [`quack_rs::vector::complex::ListVector`] instead of
//! the step count.

use crate::window_funnel::WindowFunnelState;
use libduckdb_sys::*;
use quack_rs::aggregate::{AggregateFunctionSetBuilder, FfiState};
use quack_rs::types::{LogicalType, TypeId};
use quack_rs::vector::complex::ListVector;

use super::window_funnel::{state_combine, update_impl};

/// Minimum number of boolean condition parameters for `window_funnel_events`.
const MIN_CONDITIONS: usize = 2;
/// Maximum number of boolean condition parameters for `window_funnel_events`.
const MAX_CONDITIONS: usize = 32;

// Note: AggregateState for WindowFunnelState is implemented in ffi/window_funnel.rs.

/// Registers the `window_funnel_events` function with `DuckDB` as a function
/// set with overloads for two signatures:
///
/// 1. Without mode: `window_funnel_events(INTERVAL, TIMESTAMP, BOOLEAN, BOOLEAN [, ...]) -> LIST(TIMESTAMP)`
/// 2. With mode: `window_funnel_events(INTERVAL, VARCHAR, TIMESTAMP, BOOLEAN, BOOLEAN [, ...]) -> LIST(TIMESTAMP)`
///
/// Returns the timestamps of each matched step of the best funnel chain — one
/// timestamp per step reached by `window_funnel` with the same arguments.
/// Empty list when no entry condition matches.
///
/// # Safety
///
/// Requires a valid connection implementing the [`Registrar`](quack_rs::connection::Registrar) trait.
///
/// # Errors
///
/// Returns an error if function registration fails.
pub unsafe fn register_window_funnel_events(
    con: &impl quack_rs::connection::Registrar,
) -> Result<(), quack_rs::error::ExtensionError> {
    let builder = AggregateFunctionSetBuilder::new("window_funnel_events")
        .returns_logical(LogicalType::list(TypeId::Timestamp))
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
        update_impl(info, input, states, false, "window_funnel_events");
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
        update_impl(info, input, states, true, "window_funnel_events");
    }
}

// SAFETY: `source` points to `count` aggregate state pointers. `result` is a
// valid DuckDB LIST(TIMESTAMP) vector. Each list entry is populated with the
// winning chain's step timestamps. Empty list when no entry condition matches.
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

            let Some(state) = FfiState::<WindowFunnelState>::with_state_mut(*source.add(i)) else {
                // Empty list for null state
                ListVector::set_entry(result, idx, list_offset, 0);
                continue;
            };

            let timestamps = state.finalize_events();
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
    use crate::common::event::Event;
    use crate::window_funnel::{FunnelMode, WindowFunnelState};
    use quack_rs::testing::AggregateTestHarness;

    #[test]
    fn test_funnel_events_combine_config_propagation() {
        // Zero-initialized target combine pattern: window size and mode must
        // propagate so finalize_events sees the right configuration.
        let mut source = AggregateTestHarness::<WindowFunnelState>::new();
        source.update(|s| {
            s.window_size_us = 10_000_000;
            s.mode = FunnelMode::STRICT_ONCE;
            s.update(Event::new(1_000_000, 0b01), 2);
            s.update(Event::new(2_000_000, 0b10), 2);
        });

        let mut target = AggregateTestHarness::<WindowFunnelState>::new();
        target.combine(&source, |src, tgt| tgt.combine_in_place(src));

        let mut state = target.finalize();
        assert_eq!(state.window_size_us, 10_000_000);
        assert_eq!(state.mode, FunnelMode::STRICT_ONCE);
        assert_eq!(state.finalize_events(), vec![1_000_000, 2_000_000]);
    }

    #[test]
    fn test_funnel_events_combine_merges_chains() {
        // Steps arrive in different segments; the merged state still
        // reconstructs the full chain.
        let mut a = AggregateTestHarness::<WindowFunnelState>::new();
        a.update(|s| {
            s.window_size_us = 10_000_000;
            s.update(Event::new(1_000_000, 0b001), 3);
        });

        let mut b = AggregateTestHarness::<WindowFunnelState>::new();
        b.update(|s| {
            s.window_size_us = 10_000_000;
            s.update(Event::new(2_000_000, 0b010), 3);
            s.update(Event::new(3_000_000, 0b100), 3);
        });

        b.combine(&a, |src, tgt| tgt.combine_in_place(src));

        let mut state = b.finalize();
        assert_eq!(
            state.finalize_events(),
            vec![1_000_000, 2_000_000, 3_000_000]
        );
    }
}
