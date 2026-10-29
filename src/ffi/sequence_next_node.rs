// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! FFI registration for the `sequence_next_node` aggregate function.
//!
//! Uses [`quack_rs::aggregate::AggregateFunctionSetBuilder`] for function set
//! registration, [`quack_rs::aggregate::FfiState`] for safe state management,
//! and the safe input and output views in [`super::view`] (including
//! `Varchars`, which reads the `duckdb_string_t` 16-byte inline/pointer
//! format).

use super::view::{Combine, Finalize, States, Update, VarcharKind, Varchars, condition_mask};
use crate::common::config::{conflict_message, out_of_memory_message};
use crate::sequence_next_node::{Base, Direction, SequenceNextNodeState, StoreError};
use quack_rs::aggregate::AggregateFunctionSetBuilder;
use quack_rs::types::TypeId;

/// Minimum number of event condition boolean parameters.
const MIN_EVENT_CONDITIONS: usize = 1;
/// Maximum number of event condition boolean parameters: one per bit of an
/// an event's condition mask.
const MAX_EVENT_CONDITIONS: usize = crate::common::event::MAX_EVENT_CONDITIONS;

/// Number of fixed parameters before the variable boolean event conditions.
///
/// Layout: VARCHAR (direction), VARCHAR (base), TIMESTAMP, VARCHAR (`event_column`),
/// BOOLEAN (`base_condition`), then BOOLEAN × N event conditions.
const FIXED_PARAMS: usize = 5;

impl quack_rs::aggregate::AggregateState for SequenceNextNodeState {}

/// Registers the `sequence_next_node` function with `DuckDB`.
///
/// Signature: `sequence_next_node(VARCHAR, VARCHAR, TIMESTAMP, VARCHAR, BOOLEAN, BOOLEAN [, ...]) -> VARCHAR`
///
/// Parameters:
/// - `direction`: `'forward'` or `'backward'`
/// - `base`: `'head'`, `'tail'`, `'first_match'`, or `'last_match'`
/// - `timestamp`: Event timestamp column
/// - `event_column`: Value column (returned as result)
/// - `base_condition`: Boolean condition for the base/anchor event
/// - `event1, event2, ...`: Sequential event conditions to match
///
/// # Errors
///
/// Returns an error if function registration fails.
pub fn register_sequence_next_node(
    con: &impl quack_rs::connection::Registrar,
) -> Result<(), quack_rs::error::ExtensionError> {
    let mut builder =
        AggregateFunctionSetBuilder::new("sequence_next_node").returns(TypeId::Varchar);
    // The same overloads for TIMESTAMP and TIMESTAMPTZ (both int64
    // microseconds since the epoch, read identically).
    for ts in super::TIMESTAMP_TYPES {
        builder = builder.overloads(
            MIN_EVENT_CONDITIONS..=MAX_EVENT_CONDITIONS,
            move |n, builder| {
                let mut b = builder
                    .param(TypeId::Varchar) // direction
                    .param(TypeId::Varchar) // base
                    .param(ts) // timestamp
                    .param(TypeId::Varchar) // event_column
                    .param(TypeId::Boolean); // base_condition
                for _ in 0..n {
                    b = b.param(TypeId::Boolean); // event conditions
                }
                b.ffi_state::<SequenceNextNodeState>()
                    .update(state_update)
                    .combine(state_combine)
                    .finalize(state_finalize)
            },
        );
    }
    // SAFETY: `con` is the connection the entry point registers on (this
    // function's contract), and every overload installs `FfiState<T>`'s size,
    // init and destroy callbacks together via `ffi_state::<T>()`, the pairing
    // `Registrar::register_*` requires.
    unsafe { con.register_aggregate_set(builder) }
}

// SAFETY (the callbacks below): DuckDB calls them with the handles
// `Update::from_raw`, `Combine::from_raw` and `Finalize::from_raw` require, for
// the aggregate registered in `register_sequence_next_node`:
// `FfiState<SequenceNextNodeState>` states, arguments (VARCHAR direction,
// VARCHAR base, TIMESTAMP | TIMESTAMPTZ, VARCHAR value, BOOLEAN base condition,
// BOOLEAN event conditions...), and a VARCHAR result.
quack_rs::aggregate_update_callback!(state_update, |info, input, states| {
    // SAFETY: see above.
    let call = unsafe { Update::<SequenceNextNodeState>::from_raw(info, input, states) };
    update(call);
});

/// `update`: parses and checks the group's direction and base, and collects
/// each row as an event of the state.
fn update(call: Update<SequenceNextNodeState>) {
    let Update {
        info,
        chunk,
        mut states,
    } = call;
    let num_event_conditions = chunk.cols().saturating_sub(FIXED_PARAMS);

    // SAFETY: column 0 is registered as VARCHAR (direction).
    let directions = unsafe { chunk.varchars(0) };
    // SAFETY: column 1 is registered as VARCHAR (base).
    let bases = unsafe { chunk.varchars(1) };
    // SAFETY: column 2 is registered as TIMESTAMP or TIMESTAMPTZ.
    let timestamps = unsafe { chunk.timestamps(2) };
    // SAFETY: column 3 is registered as VARCHAR (the event value).
    let values = unsafe { chunk.varchars(3) };
    // SAFETY: column 4 is registered as BOOLEAN (the base condition).
    let base_conditions = unsafe { chunk.bools(4) };
    // SAFETY: columns 5.. are registered as BOOLEAN (the event conditions).
    let event_conditions = unsafe { chunk.bool_range(FIXED_PARAMS..chunk.cols()) };

    // Last direction and base strings parsed in this chunk, with parses.
    let mut last_direction: Option<([u8; 16], Direction)> = None;
    let mut last_base: Option<([u8; 16], Base)> = None;
    for i in 0..chunk.rows() {
        let Some(state) = states.get_mut(i) else {
            continue;
        };

        // Every non-NULL direction and base is parsed: an unknown value or
        // one that differs from the group's earlier rows is an error,
        // whatever the row order.
        let configured = record_direction(&directions, i, &mut last_direction, state)
            .and_then(|()| record_base(&bases, i, &mut last_base, state));
        if let Err(message) = configured {
            info.set_error(&message);
            return;
        }

        // Set num_steps (once per state)
        if state.num_steps == 0 {
            state.num_steps = num_event_conditions;
        }

        // Skip NULL timestamps
        let Some(timestamp_us) = timestamps.get(i) else {
            continue;
        };

        let bitmask = condition_mask(&event_conditions, i);

        // The state copies the value into its own buffer and grows fallibly:
        // an allocation failure becomes a SQL error instead of aborting the
        // host process.
        if let Err(error) = state.try_push(
            timestamp_us,
            values.get(i),
            base_conditions.is_true(i),
            bitmask,
        ) {
            info.set_error(&store_error_message(error, state.event_count() + 1));
            return;
        }
    }
}

/// The SQL error for a state that could not store another event.
fn store_error_message(error: StoreError, events: usize) -> String {
    match error {
        StoreError::OutOfMemory => out_of_memory_message("sequence_next_node", events),
        StoreError::TooLarge => {
            format!("sequence_next_node: the values of one group exceed 4 GiB ({events} events)")
        }
    }
}

/// Records row `i`'s direction in the state, or returns the error message for
/// an unknown direction or one that differs from the group's earlier rows.
/// A `NULL` direction is ignored.
///
/// Normally the same string is on every row: its 16-byte `duckdb_string_t` slot
/// is compared with the last row's (one comparison, however long the string),
/// and only a new string is validated and parsed.
fn record_direction(
    directions: &Varchars,
    i: usize,
    last: &mut Option<([u8; 16], Direction)>,
    state: &mut SequenceNextNodeState,
) -> Result<(), String> {
    let Some(raw) = directions.slot(i) else {
        return Ok(());
    };
    let dir = match *last {
        Some((slot, dir)) if slot == raw => dir,
        _ => {
            let text = directions.get(i).unwrap_or_default();
            let dir = SequenceNextNodeState::parse_direction(text).ok_or_else(|| {
                format!(
                    "sequence_next_node: unknown direction '{text}'; \
                     expected 'forward' or 'backward'"
                )
            })?;
            *last = Some((raw, dir));
            dir
        }
    };
    match state.direction {
        Some(existing) if existing != dir => Err(conflict_message(
            "sequence_next_node",
            "direction",
            &format!("'{}'", direction_name(existing)),
            &format!("'{}'", direction_name(dir)),
        )),
        Some(_) => Ok(()),
        None => {
            state.set_direction(dir);
            Ok(())
        }
    }
}

/// Records row `i`'s base in the state; see [`record_direction`].
fn record_base(
    bases: &Varchars,
    i: usize,
    last: &mut Option<([u8; 16], Base)>,
    state: &mut SequenceNextNodeState,
) -> Result<(), String> {
    let Some(raw) = bases.slot(i) else {
        return Ok(());
    };
    let base = match *last {
        Some((slot, base)) if slot == raw => base,
        _ => {
            let text = bases.get(i).unwrap_or_default();
            let base = SequenceNextNodeState::parse_base(text).ok_or_else(|| {
                format!(
                    "sequence_next_node: unknown base '{text}'; expected \
                     'head', 'tail', 'first_match', or 'last_match'"
                )
            })?;
            *last = Some((raw, base));
            base
        }
    };
    match state.base {
        Some(existing) if existing != base => Err(conflict_message(
            "sequence_next_node",
            "base",
            &format!("'{}'", base_name(existing)),
            &format!("'{}'", base_name(base)),
        )),
        Some(_) => Ok(()),
        None => {
            state.set_base(base);
            Ok(())
        }
    }
}

quack_rs::aggregate_combine_callback!(state_combine, |info, source, target, count| {
    // SAFETY: see above.
    let Combine {
        info,
        source,
        mut target,
    } = unsafe { Combine::<SequenceNextNodeState>::from_raw(info, source, target, count) };
    for i in 0..source.count() {
        let Some((src, tgt)) = States::pair(&source, &mut target, i) else {
            continue;
        };
        if let (Some(a), Some(b)) = (tgt.direction, src.direction)
            && a != b
        {
            info.set_error(&conflict_message(
                "sequence_next_node",
                "direction",
                &format!("'{}'", direction_name(a)),
                &format!("'{}'", direction_name(b)),
            ));
            return;
        }
        if let (Some(a), Some(b)) = (tgt.base, src.base)
            && a != b
        {
            info.set_error(&conflict_message(
                "sequence_next_node",
                "base",
                &format!("'{}'", base_name(a)),
                &format!("'{}'", base_name(b)),
            ));
            return;
        }
        let events_after = tgt.event_count() + src.event_count();
        if let Err(error) = tgt.try_combine_in_place(src) {
            info.set_error(&store_error_message(error, events_after));
            return;
        }
    }
});

/// The SQL spelling of a direction, for error messages.
const fn direction_name(direction: Direction) -> &'static str {
    match direction {
        Direction::Forward => "forward",
        Direction::Backward => "backward",
    }
}

/// The SQL spelling of a base, for error messages.
const fn base_name(base: Base) -> &'static str {
    match base {
        Base::Head => "head",
        Base::Tail => "tail",
        Base::FirstMatch => "first_match",
        Base::LastMatch => "last_match",
    }
}

// NULL is written when no match is found.
quack_rs::aggregate_finalize_callback!(state_finalize, |info, source, result, count, offset| {
    // SAFETY: see above.
    let Finalize {
        info,
        mut states,
        mut out,
    } = unsafe {
        Finalize::<SequenceNextNodeState, VarcharKind>::from_raw(
            info, source, result, count, offset,
        )
    };
    for i in 0..out.count() {
        let Some(state) = states.get_mut(i) else {
            out.set_null(i);
            continue;
        };

        // As in ClickHouse: a forward chain from the last event (or a
        // backward chain from the first) has no adjacent event to return.
        // Checked here, on the effective values (a NULL direction means
        // 'forward', a NULL base 'first_match'), so the outcome does not
        // depend on which row supplied which argument.
        match (
            state.direction.unwrap_or(Direction::Forward),
            state.base.unwrap_or(Base::FirstMatch),
        ) {
            (Direction::Forward, Base::Tail) => {
                info.set_error(
                    "sequence_next_node: base 'tail' cannot be combined with direction \
                     'forward' (the chain would start at the last event)",
                );
                return;
            }
            (Direction::Backward, Base::Head) => {
                info.set_error(
                    "sequence_next_node: base 'head' cannot be combined with direction \
                     'backward' (the chain would start at the first event)",
                );
                return;
            }
            _ => {}
        }

        match state.finalize() {
            // DuckDB copies the string, whatever its length.
            Some(value) => out.write(i, &value),
            None => out.set_null(i),
        }
    }
});

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sequence_next_node::{Base, Direction, NextNodeEvent};
    use quack_rs::testing::AggregateTestHarness;
    use std::sync::Arc;

    #[test]
    fn test_next_node_combine_config_propagation() {
        // Simulate DuckDB's zero-initialized target combine pattern (Session 10 bug).
        let mut source = AggregateTestHarness::<SequenceNextNodeState>::new();
        source.update(|s| {
            s.set_direction(Direction::Forward);
            s.set_base(Base::Head);
            s.num_steps = 1;
            s.update(NextNodeEvent {
                timestamp_us: 1_000_000,
                value: Some(Arc::from("A")),
                base_condition: true,
                conditions: 0b1,
            });
        });

        let mut target = AggregateTestHarness::<SequenceNextNodeState>::new();
        target.combine(&source, |src, tgt| tgt.combine_in_place(src));

        let state = target.finalize();
        assert_eq!(state.direction, Some(Direction::Forward));
        assert_eq!(state.base, Some(Base::Head));
    }

    #[test]
    fn test_next_node_combine_event_union() {
        let mut a = AggregateTestHarness::<SequenceNextNodeState>::new();
        a.update(|s| {
            s.set_direction(Direction::Forward);
            s.set_base(Base::Head);
            s.num_steps = 1;
            s.update(NextNodeEvent {
                timestamp_us: 1_000_000,
                value: Some(Arc::from("A")),
                base_condition: true,
                conditions: 0b1,
            });
        });

        let mut b = AggregateTestHarness::<SequenceNextNodeState>::new();
        b.update(|s| {
            s.set_direction(Direction::Forward);
            s.set_base(Base::Head);
            s.num_steps = 1;
            s.update(NextNodeEvent {
                timestamp_us: 2_000_000,
                value: Some(Arc::from("B")),
                base_condition: false,
                conditions: 0,
            });
        });

        b.combine(&a, |src, tgt| tgt.combine_in_place(src));

        let mut state = b.finalize();
        // After combining and finalizing, the result depends on matching logic.
        // With forward/head and base_condition on A, next node after match should be B.
        let result = state.finalize();
        assert_eq!(result.as_deref(), Some("B"));
    }
}
