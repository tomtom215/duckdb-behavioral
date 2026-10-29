// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! FFI registration for the `window_funnel` aggregate function.
//!
//! Uses [`quack_rs::aggregate::AggregateFunctionSetBuilder`] for function set
//! registration, [`quack_rs::aggregate::FfiState`] for safe state management,
//! and [`quack_rs::vector::VectorReader`] for safe vector reading.

use super::view::{Combine, Finalize, I32Kind, States, Update, Varchars};
use crate::common::config::{conflict_message, invalid_interval_message, out_of_memory_message};
use crate::common::event::Event;
use crate::common::timestamp::interval_to_micros;
use crate::window_funnel::{FunnelMode, WindowFunnelState};
use quack_rs::aggregate::AggregateFunctionSetBuilder;
use quack_rs::types::TypeId;

/// Error text listing every SQL mode string `window_funnel` accepts.
const VALID_MODES: &str = "'strict', 'strict_deduplication', 'strict_order', \
     'strict_increase', 'strict_once', 'allow_reentry', 'timestamp_dedup' \
     (comma-separated for combinations)";

/// Minimum number of boolean condition parameters for `window_funnel`.
const MIN_CONDITIONS: usize = 1;
/// Maximum number of boolean condition parameters: one per bit of an
/// an event's condition mask.
const MAX_CONDITIONS: usize = crate::common::event::MAX_EVENT_CONDITIONS;

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
/// # Errors
///
/// Returns an error if function registration fails.
pub fn register_window_funnel(
    con: &impl quack_rs::connection::Registrar,
) -> Result<(), quack_rs::error::ExtensionError> {
    // Register both overload groups under the same function set name.
    // DuckDB distinguishes them by parameter types.
    let mut builder = AggregateFunctionSetBuilder::new("window_funnel").returns(TypeId::Integer);
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
                b.ffi_state::<WindowFunnelState>()
                    .update(state_update)
                    .combine(state_combine)
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
                b.ffi_state::<WindowFunnelState>()
                    .update(state_update_with_mode)
                    .combine(state_combine)
                    .finalize(state_finalize)
            });
    }
    // SAFETY: `con` is the connection the entry point registers on (this
    // function's contract), and every overload installs `FfiState<T>`'s size,
    // init and destroy callbacks together via `ffi_state::<T>()`, the pairing
    // `Registrar::register_*` requires.
    unsafe { con.register_aggregate_set(builder) }
}

// SAFETY (the update callbacks below): DuckDB calls them with the handles
// `Update::from_raw` requires, for the aggregate registered in
// `register_window_funnel`: `FfiState<WindowFunnelState>` states and the
// columns that overload declares. Shared with `window_funnel_events`, which
// differs only in finalize.
quack_rs::aggregate_update_callback!(state_update, |info, input, states| {
    // SAFETY: see above.
    let call = unsafe { Update::<WindowFunnelState>::from_raw(info, input, states) };
    // No mode parameter: INTERVAL(0), TIMESTAMP(1), BOOLEAN(2..N)
    update_impl(call, false, "window_funnel");
});

quack_rs::aggregate_update_callback!(state_update_with_mode, |info, input, states| {
    // SAFETY: see above.
    let call = unsafe { Update::<WindowFunnelState>::from_raw(info, input, states) };
    // With mode parameter: INTERVAL(0), VARCHAR(1), TIMESTAMP(2), BOOLEAN(3..N)
    update_impl(call, true, "window_funnel");
});

/// Shared update implementation for both signatures.
///
/// When `has_mode` is true, column layout is:
///   \[0\] INTERVAL, \[1\] VARCHAR (mode), \[2\] TIMESTAMP, \[3..N\] BOOLEAN
/// When `has_mode` is false, column layout is:
///   \[0\] INTERVAL, \[1\] TIMESTAMP, \[2..N\] BOOLEAN
///
/// Invalid configuration (month-based or negative window, unknown mode string)
/// aborts the query via `AggregateFunctionInfo::set_error` instead of
/// silently producing wrong results.
///
/// `func` names the SQL function in error messages (`window_funnel` or
/// `window_funnel_events`).
pub(super) fn update_impl(call: Update<WindowFunnelState>, has_mode: bool, func: &str) {
    let Update {
        info,
        chunk,
        mut states,
    } = call;

    // Column indices depend on whether mode parameter is present
    let ts_col: usize = if has_mode { 2 } else { 1 };
    let bool_start: usize = ts_col + 1;
    let num_conditions = chunk.cols().saturating_sub(bool_start);

    // SAFETY: column 0 is registered as INTERVAL (the window).
    let windows = unsafe { chunk.intervals(0) };
    // SAFETY: with a mode, column 1 is registered as VARCHAR.
    let modes = has_mode.then(|| unsafe { chunk.varchars(1) });
    // SAFETY: the timestamp column is registered as TIMESTAMP or TIMESTAMPTZ.
    let timestamps = unsafe { chunk.timestamps(ts_col) };
    // SAFETY: every column after the timestamp is registered as BOOLEAN.
    let conditions = unsafe { chunk.bool_range(bool_start..chunk.cols()) };

    // The mode and window are normally the same on every row: remember the
    // last raw value of each and its parse, and redo the parse only when a
    // row's raw value differs.
    let mut last_mode: Option<([u8; 16], FunnelMode)> = None;
    let mut last_window: Option<((i32, i32, i64), i64)> = None;
    // DuckDB hands consecutive rows of one group the same state, so work per
    // run of equal state pointers: with one large group the whole chunk is
    // one run.
    let mut start = 0;
    while start < chunk.rows() {
        let end = states.run_end(start);
        let run = start..end;
        start = end;
        let Some(state) = states.get_mut(run.start) else {
            continue;
        };

        // 1. Configuration. Rows with a NULL timestamp or a NULL window are
        //    skipped (a zero default window would silently run a 0-length
        //    funnel).
        let mut any_row = false;
        for i in run.clone() {
            let (true, Some(iv)) = (timestamps.is_valid(i), windows.get(i)) else {
                continue;
            };
            any_row = true;
            let raw_window = (iv.months, iv.days, iv.micros);
            match last_window {
                Some((last, window_us))
                    if last == raw_window
                        && state.window_set
                        && state.window_size_us == window_us => {}
                _ => {
                    if let Err(message) = apply_window(state, iv.months, iv.days, iv.micros, func) {
                        info.set_error(&message);
                        return;
                    }
                    last_window = Some((raw_window, state.window_size_us));
                }
            }
            // Every non-NULL mode is parsed: an invalid or different mode
            // anywhere in the group is an error, whatever the row order.
            if let Some(modes) = &modes {
                let recorded = row_mode(modes, i, &mut last_mode, func)
                    .and_then(|mode| mode.map_or(Ok(()), |mode| record_mode(state, mode, func)));
                if let Err(message) = recorded {
                    info.set_error(&message);
                    return;
                }
            }
        }
        if !any_row {
            continue;
        }
        state.num_conditions = num_conditions;

        // 2. Events. Reserve the run once, fallibly (an allocation failure
        //    becomes a SQL error instead of aborting the host process);
        //    `extend` then writes into the reserved space. As in
        //    `WindowFunnelState::update`, an event with no true condition is
        //    kept only under `strict_order`, where it breaks a chain.
        let needed = run.len();
        if state.events.capacity() - state.events.len() < needed
            && state.events.try_reserve(needed).is_err()
        {
            info.set_error(&out_of_memory_message(func, state.events.len() + needed));
            return;
        }
        // The same rule as `WindowFunnelState::update`, plus: a mode-taking
        // state that has not seen its mode yet (its rows so far had a NULL
        // mode) keeps such events too.
        let keep_empty = state.keeps_conditionless_events(has_mode && !state.mode_set);
        state.events.extend(run.filter_map(|i| {
            if !windows.is_valid(i) {
                return None;
            }
            let timestamp_us = timestamps.get(i)?;
            let mut bitmask: u32 = 0;
            for (c, condition) in conditions.iter().enumerate() {
                if condition.is_true(i) {
                    bitmask |= 1 << c;
                }
            }
            (bitmask != 0 || keep_empty).then(|| Event::new(timestamp_us, bitmask))
        }));
    }
}

// SAFETY: DuckDB calls it with the handles `Combine::from_raw` requires.
// Shared with `window_funnel_events`, which differs only in finalize.
quack_rs::aggregate_combine_callback!(state_combine, |info, source, target, count| {
    // SAFETY: see above.
    let call = unsafe { Combine::<WindowFunnelState>::from_raw(info, source, target, count) };
    combine_impl(call, "window_funnel");
});

/// Validates a row's window and records it in the state, or returns the
/// error message: month-based or negative windows, and a window that differs
/// from one the state already holds.
fn apply_window(
    state: &mut WindowFunnelState,
    months: i32,
    days: i32,
    micros: i64,
    func: &str,
) -> Result<(), String> {
    match interval_to_micros(months, days, micros) {
        Some(window_us) if state.window_set && window_us != state.window_size_us => {
            Err(conflict_message(
                func,
                "window",
                &describe_micros(state.window_size_us),
                &describe_micros(window_us),
            ))
        }
        Some(window_us) if window_us >= 0 => {
            state.window_size_us = window_us;
            state.window_set = true;
            Ok(())
        }
        Some(_) => Err(format!("{func}: INTERVAL window must be non-negative")),
        None => Err(invalid_interval_message(func, "window", months)),
    }
}

/// The mode of row `i`, parsing it only when its raw string slot differs
/// from the last one parsed in this chunk (the mode is normally the same
/// string on every row). `Ok(None)` for a `NULL` mode.
fn row_mode(
    modes: &Varchars,
    i: usize,
    last_mode: &mut Option<([u8; 16], FunnelMode)>,
    func: &str,
) -> Result<Option<FunnelMode>, String> {
    if !modes.is_valid(i) {
        return Ok(None);
    }
    let slot = modes.slot(i);
    if let Some((last, mode)) = *last_mode
        && last == slot
    {
        return Ok(Some(mode));
    }
    let Some(text) = modes.get(i) else {
        return Ok(None);
    };
    let mode = parse_mode(text, func)?;
    *last_mode = Some((slot, mode));
    Ok(Some(mode))
}

/// Parses a mode string, or returns the error message: unknown modes and
/// `allow_reentry` without `strict_order`.
fn parse_mode(text: &str, func: &str) -> Result<FunnelMode, String> {
    match FunnelMode::parse_modes(text) {
        Ok(mode) if mode.has(FunnelMode::ALLOW_REENTRY) && !mode.has(FunnelMode::STRICT_ORDER) => {
            Err(format!(
                "{func}: mode 'allow_reentry' requires 'strict_order'"
            ))
        }
        Ok(mode) => Ok(mode),
        Err(unknown) => Err(format!(
            "{func}: unknown mode '{unknown}'; valid modes are {VALID_MODES}"
        )),
    }
}

/// Records a row's mode in the state, or returns the error for a mode that
/// differs from one the state already holds.
fn record_mode(state: &mut WindowFunnelState, mode: FunnelMode, func: &str) -> Result<(), String> {
    if state.mode_set && mode != state.mode {
        return Err(conflict_message(
            func,
            "mode",
            &format!("'{}'", state.mode),
            &format!("'{mode}'"),
        ));
    }
    state.mode = mode;
    state.mode_set = true;
    Ok(())
}

/// Combine shared by `window_funnel` and `window_funnel_events`: appends the
/// source's events, propagates the window and mode into fresh targets, and
/// rejects a target and source holding different windows or modes.
pub(super) fn combine_impl(call: Combine<WindowFunnelState>, func: &str) {
    let Combine {
        info,
        source,
        mut target,
    } = call;
    for i in 0..source.count() {
        let Some((src, tgt)) = States::pair(&source, &mut target, i) else {
            continue;
        };
        if tgt.window_set && src.window_set && tgt.window_size_us != src.window_size_us {
            info.set_error(&conflict_message(
                func,
                "window",
                &describe_micros(tgt.window_size_us),
                &describe_micros(src.window_size_us),
            ));
            return;
        }
        if tgt.mode_set && src.mode_set && tgt.mode != src.mode {
            info.set_error(&conflict_message(
                func,
                "mode",
                &format!("'{}'", tgt.mode),
                &format!("'{}'", src.mode),
            ));
            return;
        }
        if tgt.events.try_reserve(src.events.len()).is_err() {
            info.set_error(&out_of_memory_message(
                func,
                tgt.events.len() + src.events.len(),
            ));
            return;
        }
        tgt.combine_in_place(src);
    }
}

/// Describes a window length for error messages, e.g. `INTERVAL 3600 seconds`.
fn describe_micros(us: i64) -> String {
    if us % 1_000_000 == 0 {
        format!("INTERVAL {} seconds", us / 1_000_000)
    } else {
        format!("INTERVAL {us} microseconds")
    }
}

// SAFETY: DuckDB calls it with the handles `Finalize::from_raw` requires; the
// INTEGER result vector matches the registered return type.
quack_rs::aggregate_finalize_callback!(state_finalize, |info, source, result, count, offset| {
    // SAFETY: see above.
    let Finalize {
        mut states,
        mut out,
        ..
    } = unsafe {
        Finalize::<WindowFunnelState, I32Kind>::from_raw(info, source, result, count, offset)
    };
    for i in 0..out.count() {
        match states.get_mut(i) {
            Some(state) => out.write(i, state.finalize() as i32),
            None => out.set_null(i),
        }
    }
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_window_rejects_a_different_window_including_after_zero() {
        let mut state = WindowFunnelState::new();
        // A zero window is a real value, not "unset".
        apply_window(&mut state, 0, 0, 0, "f").unwrap();
        assert!(state.window_set);
        apply_window(&mut state, 0, 0, 0, "f").unwrap();
        let err = apply_window(&mut state, 0, 0, 3_600_000_000, "f").unwrap_err();
        assert!(
            err.contains("the window argument must be the same"),
            "{err}"
        );
        assert!(
            err.contains("INTERVAL 0 seconds and INTERVAL 3600 seconds"),
            "{err}"
        );
        // Equal totals spelled differently are the same window.
        let mut state = WindowFunnelState::new();
        apply_window(&mut state, 0, 1, 0, "f").unwrap();
        apply_window(&mut state, 0, 0, 86_400_000_000, "f").unwrap();
        assert!(apply_window(&mut state, 1, 0, 0, "f").is_err());
    }

    #[test]
    fn apply_mode_rejects_a_different_mode() {
        let mut state = WindowFunnelState::new();
        let apply_mode = |state: &mut WindowFunnelState, text: &str, func: &str| {
            record_mode(state, parse_mode(text, func)?, func)
        };
        apply_mode(&mut state, "strict_order", "f").unwrap();
        apply_mode(&mut state, " STRICT_ORDER ", "f").unwrap();
        let err = apply_mode(&mut state, "strict_once", "f").unwrap_err();
        assert!(err.contains("the mode argument must be the same"), "{err}");
        assert!(
            apply_mode(&mut state, "bogus", "f")
                .unwrap_err()
                .contains("unknown mode")
        );
        let mut fresh = WindowFunnelState::new();
        assert!(apply_mode(&mut fresh, "allow_reentry", "f").is_err());
        // An empty mode string is a set (default) mode, distinct from a NULL.
        let mut empty = WindowFunnelState::new();
        apply_mode(&mut empty, "", "f").unwrap();
        assert!(empty.mode_set);
        assert!(apply_mode(&mut empty, "strict_order", "f").is_err());
    }

    #[test]
    fn combine_carries_a_zero_window_into_a_fresh_target() {
        let mut source = WindowFunnelState::new();
        source.window_size_us = 0;
        source.window_set = true;
        let mut target = WindowFunnelState::new();
        target.combine_in_place(&source);
        assert!(target.window_set);
        assert_eq!(target.window_size_us, 0);
    }
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
