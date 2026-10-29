// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! FFI registration for the `window_funnel_events` aggregate function.
//!
//! `window_funnel_events` shares [`WindowFunnelState`] and the update/combine
//! callbacks with `window_funnel` (see [`super::window_funnel`]) — it differs
//! only in finalize, which returns the winning chain's step timestamps as
//! `LIST(TIMESTAMP)` via [`quack_rs::vector::complex::ListVector`] instead of
//! the step count.

use super::view::{Combine, Finalize, TimestampListKind, Update};
use super::window_funnel::{combine_impl, update_impl};
use crate::window_funnel::WindowFunnelState;
use quack_rs::aggregate::AggregateFunctionSetBuilder;
use quack_rs::types::{LogicalType, TypeId};

/// Minimum number of boolean condition parameters for `window_funnel_events`.
const MIN_CONDITIONS: usize = 1;
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
/// # Errors
///
/// Returns an error if function registration fails.
pub fn register_window_funnel_events(
    con: &impl quack_rs::connection::Registrar,
) -> Result<(), quack_rs::error::ExtensionError> {
    let mut builder = AggregateFunctionSetBuilder::new("window_funnel_events")
        .returns_logical(LogicalType::list(TypeId::Timestamp));
    // The same overloads for TIMESTAMP and TIMESTAMPTZ (both int64
    // microseconds since the epoch, read identically).
    for ts in super::TIMESTAMP_TYPES {
        builder = builder
            // Group 1: WITHOUT mode parameter: (INTERVAL, TIMESTAMP, BOOL×N)
            .overloads(MIN_CONDITIONS..=MAX_CONDITIONS, move |n, builder| {
                let mut b = builder.param(TypeId::Interval).param(ts);
                for _ in 0..n {
                    b = b.param(TypeId::Boolean);
                }
                b.returns_logical(LogicalType::list(ts))
                    .ffi_state::<WindowFunnelState>()
                    .update(state_update)
                    .combine(events_combine)
                    .finalize(state_finalize)
            })
            // Group 2: WITH mode parameter: (INTERVAL, VARCHAR, TIMESTAMP, BOOL×N)
            .overloads(MIN_CONDITIONS..=MAX_CONDITIONS, move |n, builder| {
                let mut b = builder
                    .param(TypeId::Interval)
                    .param(TypeId::Varchar)
                    .param(ts);
                for _ in 0..n {
                    b = b.param(TypeId::Boolean);
                }
                b.returns_logical(LogicalType::list(ts))
                    .ffi_state::<WindowFunnelState>()
                    .update(state_update_with_mode)
                    .combine(events_combine)
                    .finalize(state_finalize)
            });
    }
    // SAFETY: `con` is the connection the entry point registers on (this
    // function's contract), and every overload installs `FfiState<T>`'s size,
    // init and destroy callbacks together via `ffi_state::<T>()`, the pairing
    // `Registrar::register_*` requires.
    unsafe { con.register_aggregate_set(builder) }
}

// SAFETY (the callbacks below): DuckDB calls them with the handles
// `Update::from_raw`, `Combine::from_raw` and `Finalize::from_raw` require, for
// the aggregate registered in `register_window_funnel_events`:
// `FfiState<WindowFunnelState>` states, the columns each overload declares, and
// a `LIST(TIMESTAMP | TIMESTAMPTZ)` result.
quack_rs::aggregate_update_callback!(state_update, |info, input, states| {
    // SAFETY: see above.
    let call = unsafe { Update::<WindowFunnelState>::from_raw(info, input, states) };
    // No mode parameter: INTERVAL(0), TIMESTAMP(1), BOOLEAN(2..N)
    update_impl(call, false, "window_funnel_events");
});

quack_rs::aggregate_update_callback!(state_update_with_mode, |info, input, states| {
    // SAFETY: see above.
    let call = unsafe { Update::<WindowFunnelState>::from_raw(info, input, states) };
    // With mode parameter: INTERVAL(0), VARCHAR(1), TIMESTAMP(2), BOOLEAN(3..N)
    update_impl(call, true, "window_funnel_events");
});

quack_rs::aggregate_combine_callback!(events_combine, |info, source, target, count| {
    // SAFETY: see above.
    let call = unsafe { Combine::<WindowFunnelState>::from_raw(info, source, target, count) };
    combine_impl(call, "window_funnel_events");
});

// Each list entry is the winning chain's step timestamps; an empty list when
// no entry condition matches (or the state pointer is null).
quack_rs::aggregate_finalize_callback!(state_finalize, |info, source, result, count, offset| {
    // SAFETY: see above.
    let Finalize {
        mut states,
        mut out,
        ..
    } = unsafe {
        Finalize::<WindowFunnelState, TimestampListKind>::from_raw(
            info, source, result, count, offset,
        )
    };
    for i in 0..out.count() {
        let timestamps = states
            .get_mut(i)
            .map(WindowFunnelState::finalize_events)
            .unwrap_or_default();
        out.push(i, &timestamps);
    }
});

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
