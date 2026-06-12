// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! Pattern parsing and matching for `sequence_match` and `sequence_count`.
//!
//! Implements a mini-regex language over event conditions, compatible with
//! `ClickHouse`'s `sequenceMatch`/`sequenceCount` pattern syntax.
//!
//! # Pattern Syntax
//!
//! ```text
//! (?N)      — Match an event where condition N (1-indexed) is true
//! .         — Match exactly one event (any conditions)
//! .*        — Match zero or more events (any conditions)
//! (?t>=N)   — Time constraint: at least N seconds since previous match
//! (?t<=N)   — Time constraint: at most N seconds since previous match
//! (?t>N)    — Time constraint: more than N seconds since previous match
//! (?t<N)    — Time constraint: less than N seconds since previous match
//! (?t==N)   — Time constraint: exactly N seconds since previous match
//! (?t!=N)   — Time constraint: not exactly N seconds since previous match
//! ```
//!
//! `(?t!=N)` is an extension beyond `ClickHouse`'s pattern syntax (`.` exists
//! there too, as `PatternActionType::AnyEvent`). Rows where no condition is
//! true are filtered during `update` (they cannot match any `(?N)` step), so
//! `.` matches exactly one *stored* event — one where at least one condition
//! fired. Consecutive `.*` runs are collapsed at parse time (semantically
//! identical, exponentially cheaper).
//!
//! # Time-Constraint Semantics
//!
//! Time constraints mirror `ClickHouse` (verified against
//! `AggregateFunctionSequenceMatch.cpp`): the constraint gates the next
//! pattern step without consuming an event, and non-matching events in
//! between are skipped whenever the gate could still (or again) hold — e.g.
//! `(?1)(?t<=10)(?2)` matches `(?2)` at any event within 10 seconds of
//! `(?1)`, regardless of interleaved events. Trailing `(?t<=N)`, `(?t<N)`
//! and `(?t>=0)` match the empty remainder, as in `ClickHouse`.
//!
//! Two deliberate adaptations: `N` is interpreted in **seconds** with the
//! elapsed time floored to whole seconds (`ClickHouse` compares raw
//! timestamp-column units, which equals whole seconds for `DateTime`; the
//! floor generalizes that to microsecond timestamps, so `(?t==N)` means
//! "within `[N, N+1)` seconds"). And constraints are anchored at the last
//! **matched condition** — `ClickHouse` re-anchors at wildcard positions,
//! which makes `(?t<=N)` after `.*` vacuous there.

pub mod executor;
pub mod parser;
