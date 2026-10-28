// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! FFI registration for the `sequence_match_events` aggregate function.
//!
//! Uses [`quack_rs::aggregate::AggregateFunctionSetBuilder`] with
//! [`returns_logical`][quack_rs::aggregate::AggregateFunctionSetBuilder::returns_logical]
//! for `LIST(TIMESTAMP)` return type registration.
//! Uses [`quack_rs::aggregate::FfiState`] for safe state management,
//! [`quack_rs::vector::VectorReader`] for input, and
//! [`quack_rs::vector::complex::ListVector`] + [`quack_rs::vector::VectorWriter`]
//! for LIST output.

use super::sequence::{combine_impl, update_impl};
use super::view::{Combine, Finalize, TimestampListKind, Update};
use crate::sequence::SequenceState;
use quack_rs::aggregate::AggregateFunctionSetBuilder;
use quack_rs::types::{LogicalType, TypeId};

/// Minimum number of boolean condition parameters for sequence functions.
const MIN_CONDITIONS: usize = 2;
/// Maximum number of boolean condition parameters for sequence functions.
const MAX_CONDITIONS: usize = 32;

// Note: AggregateState for SequenceState is implemented in ffi/sequence.rs.

/// Registers the `sequence_match_events` function with `DuckDB`.
///
/// Signature: `sequence_match_events(VARCHAR, TIMESTAMP, BOOLEAN, BOOLEAN [, ...]) -> LIST(TIMESTAMP)`
///
/// Returns an array of timestamps corresponding to each matched `(?N)` step in
/// the pattern. When the full pattern never matches, the timestamps of the
/// LONGEST partial chain are returned (`ClickHouse` `sequenceMatchEvents`
/// semantics); empty array when no condition ever fired.
///
/// # Errors
///
/// Returns an error if function registration fails.
pub fn register_sequence_match_events(
    con: &impl quack_rs::connection::Registrar,
) -> Result<(), quack_rs::error::ExtensionError> {
    let mut builder = AggregateFunctionSetBuilder::new("sequence_match_events")
        .returns_logical(LogicalType::list(TypeId::Timestamp));
    // The same overloads for TIMESTAMP and TIMESTAMPTZ (both int64
    // microseconds since the epoch, read identically).
    for ts in super::TIMESTAMP_TYPES {
        builder = builder.overloads(MIN_CONDITIONS..=MAX_CONDITIONS, move |n, builder| {
            let mut b = builder.param(TypeId::Varchar).param(ts);
            for _ in 0..n {
                b = b.param(TypeId::Boolean);
            }
            b.returns_logical(LogicalType::list(ts))
                .ffi_state::<SequenceState>()
                .update(state_update)
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

// SAFETY (the callbacks below): DuckDB calls them with the handles
// `Update::from_raw`, `Combine::from_raw` and `Finalize::from_raw` require, for
// the aggregate registered in `register_sequence_match_events`:
// `FfiState<SequenceState>` states, arguments (VARCHAR pattern,
// TIMESTAMP | TIMESTAMPTZ, BOOLEAN...), and a `LIST(TIMESTAMP | TIMESTAMPTZ)`
// result.
quack_rs::aggregate_update_callback!(state_update, |info, input, states| {
    // SAFETY: see above.
    let call = unsafe { Update::<SequenceState>::from_raw(info, input, states) };
    update_impl(call, "sequence_match_events");
});

quack_rs::aggregate_combine_callback!(state_combine, |info, source, target, count| {
    // SAFETY: see above.
    let call = unsafe { Combine::<SequenceState>::from_raw(info, source, target, count) };
    combine_impl(call, "sequence_match_events");
});

// Each list entry holds the matched condition timestamps: empty on no match or
// a NULL pattern; any other error (an invalid pattern) aborts the query.
quack_rs::aggregate_finalize_callback!(state_finalize, |info, source, result, count, offset| {
    // SAFETY: see above.
    let Finalize {
        info,
        mut states,
        mut out,
    } = unsafe {
        Finalize::<SequenceState, TimestampListKind>::from_raw(info, source, result, count, offset)
    };
    for i in 0..out.count() {
        let timestamps = match states.get_mut(i) {
            None => Vec::new(),
            Some(state) => match state.finalize_events() {
                Ok(ts) => ts,
                // A NULL pattern finalizes as an empty list.
                Err(_) if state.pattern_str.is_none() => Vec::new(),
                Err(e) => {
                    info.set_error(&format!("sequence_match_events: {e}"));
                    Vec::new()
                }
            },
        };
        out.push(i, &timestamps);
    }
});

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::event::Event;
    use quack_rs::testing::AggregateTestHarness;

    #[test]
    fn test_sequence_events_empty_pattern() {
        let mut state = AggregateTestHarness::<SequenceState>::aggregate(
            vec![Event::new(1_000_000, 0b01), Event::new(2_000_000, 0b10)],
            |s, event| {
                if s.pattern_str.is_none() {
                    s.set_pattern("(?3)"); // condition 3 never fires
                }
                s.update(event);
            },
        );
        let events = state.finalize_events().unwrap();
        assert_eq!(events, Vec::<i64>::new());
    }

    #[test]
    fn test_sequence_events_combine_timestamp_union() {
        let mut a = AggregateTestHarness::<SequenceState>::new();
        a.update(|s| {
            s.set_pattern("(?1).*(?2)");
            s.update(Event::new(1_000_000, 0b01));
        });

        let mut b = AggregateTestHarness::<SequenceState>::new();
        b.update(|s| {
            s.set_pattern("(?1).*(?2)");
            s.update(Event::new(2_000_000, 0b10));
        });

        b.combine(&a, |src, tgt| tgt.combine_in_place(src));

        let mut state = b.finalize();
        let events = state.finalize_events().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0], 1_000_000);
        assert_eq!(events[1], 2_000_000);
    }

    #[test]
    fn test_sequence_events_config_propagation() {
        // Zero-initialized target combine pattern (Session 10 bug).
        let mut source = AggregateTestHarness::<SequenceState>::new();
        source.update(|s| {
            s.set_pattern("(?1).*(?2)");
            s.update(Event::new(1_000_000, 0b01));
            s.update(Event::new(2_000_000, 0b10));
        });

        let mut target = AggregateTestHarness::<SequenceState>::new();
        target.combine(&source, |src, tgt| tgt.combine_in_place(src));

        let mut state = target.finalize();
        assert!(state.pattern_str.is_some());
        let events = state.finalize_events().unwrap();
        assert_eq!(events.len(), 2);
    }
}
