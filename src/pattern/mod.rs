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

pub mod executor;
pub mod parser;
