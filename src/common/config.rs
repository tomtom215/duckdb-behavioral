// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! Checks for configuration arguments that must be constant within a group.
//!
//! Arguments such as a `sequence_*` pattern or a `window_funnel` window are
//! read per row, but one group can only use one value. Taking the first value
//! a partial state happened to see made the result (and whether an invalid
//! later value raised an error) depend on row order and on how `DuckDB`
//! split the group across threads. A group with two different non-`NULL`
//! values is an error instead; every update and combine compares values, so
//! some comparison always sees both and the error does not depend on order.

/// The error for a group that has two different values of a configuration
/// argument. The values are listed in sorted order so the message does not
/// depend on which one arrived first.
#[must_use]
pub fn conflict_message(function: &str, argument: &str, a: &str, b: &str) -> String {
    let (first, second) = if a <= b { (a, b) } else { (b, a) };
    format!(
        "{function}: the {argument} argument must be the same for every row of a \
         group, but one group has both {first} and {second}"
    )
}

/// The error for an `INTERVAL` argument that is not a fixed span of time.
///
/// An interval is rejected when it has months (28-31 days) or its total does
/// not fit in 64-bit microseconds. `months` is the interval's month component,
/// which picks the explanation.
#[must_use]
pub fn invalid_interval_message(function: &str, argument: &str, months: i32) -> String {
    if months == 0 {
        format!(
            "{function}: invalid INTERVAL {argument}: the total must fit in signed 64-bit \
             microseconds"
        )
    } else {
        format!(
            "{function}: invalid INTERVAL {argument}: month-based intervals are ambiguous \
             (28-31 days); use day/hour/minute/second units instead"
        )
    }
}

/// The error for an event buffer that could not grow.
#[must_use]
pub fn out_of_memory_message(function: &str, events: usize) -> String {
    format!("{function}: out of memory while collecting {events} events for one group")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_interval_message_names_the_actual_problem() {
        let months = invalid_interval_message("f", "window", 1);
        assert!(months.contains("month-based"), "{months}");
        assert!(!months.contains("64-bit"), "{months}");
        let overflow = invalid_interval_message("f", "gap", 0);
        assert!(overflow.contains("64-bit"), "{overflow}");
        assert!(!overflow.contains("month"), "{overflow}");
    }

    #[test]
    fn conflict_message_does_not_depend_on_argument_order() {
        assert_eq!(
            conflict_message("f", "pattern", "'b'", "'a'"),
            conflict_message("f", "pattern", "'a'", "'b'")
        );
        assert!(conflict_message("f", "pattern", "'b'", "'a'").contains("'a' and 'b'"));
    }
}
