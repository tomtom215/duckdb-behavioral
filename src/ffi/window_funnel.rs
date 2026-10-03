// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! FFI registration for the `window_funnel` aggregate function.
//!
//! Uses [`quack_rs::aggregate::AggregateFunctionSetBuilder`] for function set
//! registration, [`quack_rs::aggregate::FfiState`] for safe state management,
//! and [`quack_rs::vector::VectorReader`] for safe vector reading.

use super::RawStringSlots;
use crate::common::config::{conflict_message, out_of_memory_message};
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
const MIN_CONDITIONS: usize = 1;
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

// SAFETY: `input` is a valid DuckDB data chunk with columns (INTERVAL, TIMESTAMP,
// BOOLEAN...) as registered. `states` points to `row_count` aggregate state pointers.
// Shared with `window_funnel_events`, which differs only in finalize.
quack_rs::aggregate_update_callback!(state_update, |info, input, states| {
    // No mode parameter: INTERVAL(0), TIMESTAMP(1), BOOLEAN(2..N)
    unsafe {
        update_impl(info, input, states, false, "window_funnel");
    }
});

// SAFETY: `input` is a valid DuckDB data chunk with columns (INTERVAL, VARCHAR,
// TIMESTAMP, BOOLEAN...) as registered. The VARCHAR at column 1 contains the mode
// string. `states` points to `row_count` aggregate state pointers.
// Shared with `window_funnel_events`, which differs only in finalize.
quack_rs::aggregate_update_callback!(state_update_with_mode, |info, input, states| {
    // With mode parameter: INTERVAL(0), VARCHAR(1), TIMESTAMP(2), BOOLEAN(3..N)
    unsafe {
        update_impl(info, input, states, true, "window_funnel");
    }
});

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
/// `func` names the SQL function in error messages (`window_funnel` or
/// `window_funnel_events`).
///
/// # Safety
///
/// Requires a valid `info` handle plus valid `input` data chunk and `states`
/// aggregate state pointers.
pub(super) unsafe fn update_impl(
    info: duckdb_function_info,
    input: duckdb_data_chunk,
    states: *mut duckdb_aggregate_state,
    has_mode: bool,
    func: &str,
) {
    // SAFETY: forwarded from this function's contract: `info`, `input` and
    // `states` are the handles DuckDB passed to the update callback, with
    // `states` holding one initialised state pointer per input row.
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

        // The mode and window are normally the same on every row: remember
        // the last raw value of each and its parse, and redo the parse only
        // when a row's raw value differs.
        let mode_slots = has_mode.then(|| RawStringSlots::new(input, 1));
        let mut last_mode: Option<([u8; 16], FunnelMode)> = None;
        let mut last_window: Option<((i32, i32, i64), i64)> = None;
        // DuckDB hands consecutive rows of one group the same state, so work
        // per run of equal state pointers: with one large group the whole
        // chunk is one run.
        let mut start = 0;
        while start < row_count {
            let state_ptr = *states.add(start);
            let mut end = start + 1;
            while end < row_count && *states.add(end) == state_ptr {
                end += 1;
            }
            let run = start..end;
            start = end;
            let Some(state) = FfiState::<WindowFunnelState>::with_state_mut(state_ptr) else {
                continue;
            };

            // 1. Configuration. Rows with a NULL timestamp or a NULL window
            //    are skipped (a zero default window would silently run a
            //    0-length funnel).
            let mut any_row = false;
            for i in run.clone() {
                if !ts_reader.is_valid(i) || !interval_reader.is_valid(i) {
                    continue;
                }
                any_row = true;
                let iv = interval_reader.read_interval(i);
                let raw_window = (iv.months, iv.days, iv.micros);
                match last_window {
                    Some((last, window_us))
                        if last == raw_window
                            && state.window_set
                            && state.window_size_us == window_us => {}
                    _ => {
                        if let Err(message) =
                            apply_window(state, iv.months, iv.days, iv.micros, func)
                        {
                            info.set_error(&message);
                            return;
                        }
                        last_window = Some((raw_window, state.window_size_us));
                    }
                }
                // Every non-NULL mode is parsed: an invalid or different mode
                // anywhere in the group is an error, whatever the row order.
                if let (Some(reader), Some(slots)) = (&mode_reader, &mode_slots) {
                    if reader.is_valid(i) {
                        let recorded = row_mode(reader, slots, i, &mut last_mode, func)
                            .and_then(|mode| record_mode(state, mode, func));
                        if let Err(message) = recorded {
                            info.set_error(&message);
                            return;
                        }
                    }
                }
            }
            if !any_row {
                continue;
            }
            state.num_conditions = num_conditions;

            // 2. Events. Reserve the run once, fallibly (an allocation
            //    failure becomes a SQL error instead of aborting the host
            //    process); `extend` then writes into the reserved space. As in
            //    `WindowFunnelState::update`, an event with no true condition
            //    is kept only under `strict_order`, where it breaks a chain.
            let needed = run.len();
            if state.events.capacity() - state.events.len() < needed
                && state.events.try_reserve(needed).is_err()
            {
                info.set_error(&out_of_memory_message(func, state.events.len() + needed));
                return;
            }
            // While a mode-taking state has not seen its mode yet (its rows
            // so far had a NULL mode), keep such events too: the group's mode
            // may still turn out to be `strict_order`. Finalize ignores them
            // in the other modes.
            let keep_empty =
                state.mode.has(FunnelMode::STRICT_ORDER) || (has_mode && !state.mode_set);
            state.events.extend(run.filter_map(|i| {
                if !ts_reader.is_valid(i) || !interval_reader.is_valid(i) {
                    return None;
                }
                let mut bitmask: u32 = 0;
                for (c, reader) in cond_readers.iter().enumerate() {
                    if reader.is_valid(i) && reader.read_bool(i) {
                        bitmask |= 1 << c;
                    }
                }
                (bitmask != 0 || keep_empty).then(|| Event::new(ts_reader.read_i64(i), bitmask))
            }));
        }
    }
}

// SAFETY: `source` and `target` point to `count` aggregate state pointers.
// Shared with `window_funnel_events`, which differs only in finalize.
quack_rs::aggregate_combine_callback!(state_combine, |info, source, target, count| {
    unsafe { combine_impl(info, source, target, count, "window_funnel") }
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
        None => Err(format!(
            "{func}: invalid INTERVAL window: month-based intervals are ambiguous \
             (28-31 days) and the total must fit in signed 64-bit microseconds; use \
             day/hour/minute/second units instead"
        )),
    }
}

/// The mode of row `i`, parsing it only when its raw string slot differs
/// from the last one parsed in this chunk (the mode is normally the same
/// string on every row).
///
/// # Safety
///
/// `reader` and `slots` must read the chunk's `VARCHAR` mode column, and row
/// `i` must be in bounds and non-`NULL`.
unsafe fn row_mode(
    reader: &VectorReader,
    slots: &RawStringSlots,
    i: usize,
    last_mode: &mut Option<([u8; 16], FunnelMode)>,
    func: &str,
) -> Result<FunnelMode, String> {
    // SAFETY: forwarded from this function's contract.
    let slot = unsafe { slots.get(i) };
    match *last_mode {
        Some((last, mode)) if last == slot => Ok(mode),
        _ => {
            // SAFETY: forwarded from this function's contract.
            let mode = parse_mode(unsafe { reader.read_str(i) }, func)?;
            *last_mode = Some((slot, mode));
            Ok(mode)
        }
    }
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
///
/// # Safety
///
/// Requires a valid `info` handle and `source`/`target` arrays of `count`
/// state pointers, with `source[i]` and `target[i]` distinct states (as
/// `DuckDB` guarantees), so the shared and mutable borrows do not alias.
pub(super) unsafe fn combine_impl(
    info: duckdb_function_info,
    source: *mut duckdb_aggregate_state,
    target: *mut duckdb_aggregate_state,
    count: idx_t,
    func: &str,
) {
    // SAFETY: forwarded from this function's contract.
    unsafe {
        let info = AggregateFunctionInfo::new(info);
        for i in 0..count as usize {
            let Some(src) = FfiState::<WindowFunnelState>::with_state(*source.add(i)) else {
                continue;
            };
            let Some(tgt) = FfiState::<WindowFunnelState>::with_state_mut(*target.add(i)) else {
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
}

/// Describes a window length for error messages, e.g. `INTERVAL 3600 seconds`.
fn describe_micros(us: i64) -> String {
    if us % 1_000_000 == 0 {
        format!("INTERVAL {} seconds", us / 1_000_000)
    } else {
        format!("INTERVAL {us} microseconds")
    }
}

// SAFETY: `source` points to `count` aggregate state pointers. `result` is a
// valid DuckDB INTEGER vector with room for `offset + count` elements.
quack_rs::aggregate_finalize_callback!(state_finalize, |_info, source, result, count, offset| {
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
        assert!(apply_mode(&mut state, "bogus", "f")
            .unwrap_err()
            .contains("unknown mode"));
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
