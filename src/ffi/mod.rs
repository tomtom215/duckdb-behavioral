// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! FFI bindings for registering behavioral analytics functions with `DuckDB`.
//!
//! # Architecture
//!
//! Every module uses the `quack-rs` v0.19.0 SDK for registration, state
//! management, and vector I/O:
//!
//! - [`quack_rs::aggregate::AggregateFunctionSetBuilder`] — registers function
//!   sets with N overloads. Supports both simple returns (`.returns(TypeId)`) and
//!   parameterized returns (`.returns_logical(LogicalType)`) for `LIST(T)` types.
//! - [`quack_rs::aggregate::FfiState<T>`] — `#[repr(C)]` wrapper providing safe
//!   init/destroy lifecycle and null-checked `with_state_mut()` accessors.
//! - `quack_rs::aggregate_update_callback!`, `aggregate_combine_callback!`, and
//!   `aggregate_finalize_callback!` — generate every aggregate's update,
//!   combine, and finalize callbacks with a panic guard that reports a panic as
//!   a SQL error instead of unwinding into `DuckDB` (the release profile uses
//!   `panic = "unwind"` so the guard is effective).
//! - [`quack_rs::vector::VectorReader`] — safe vector reading (`read_bool()`,
//!   `read_str()`, `read_i64()`, `read_interval()`).
//! - [`quack_rs::vector::VectorWriter`] — safe vector writing (`write_i32()`,
//!   `write_bool()`, `write_varchar()`, `set_null()` with automatic validity
//!   bitmap setup).
//! - [`quack_rs::vector::complex::ListVector`] — safe LIST vector output
//!   (`reserve()`, `set_size()`, `set_entry()`, `child_writer()`).
//! - [`quack_rs::types::LogicalType::list()`] — RAII construction of `LIST(T)`
//!   types for `returns_logical()` and function registration.
//!
//! All 8 aggregate functions use quack-rs builders for registration, as
//! function sets (`AggregateFunctionSetBuilder`): the seven variadic functions
//! have one overload per condition count (and per timestamp type), including
//! `retention`, `window_funnel_events`, and `sequence_match_events`, which use
//! `.returns_logical(LogicalType::list(...))` for their `LIST(T)` return
//! types; [`sessionize`] has a fixed argument list and two overloads, one per
//! timestamp type. [`version`] registers the `behavioral_version()` scalar through
//! quack-rs's scalar builder. No module in this crate registers through raw
//! `libduckdb-sys` calls.
//!
//! # Entry Point
//!
//! Registration uses the [`quack_rs::entry_point_v2!`] macro, which provides a
//! [`Connection`] implementing the [`Registrar`](quack_rs::connection::Registrar) trait — a version-agnostic API
//! for registering extension components. The extension uses only the stable
//! `DuckDB` C API (stamped C API version v1.2.0, ABI type `C_STRUCT`), so one
//! binary loads into `DuckDB` 1.3.2 and later releases.

pub mod retention;
pub mod sequence;
pub mod sequence_match_events;
pub mod sequence_next_node;
pub mod sessionize;
pub mod version;
pub mod view;
pub mod window_funnel;
pub mod window_funnel_events;

use quack_rs::connection::Connection;
use quack_rs::types::TypeId;

/// The timestamp types every function accepts. `TIMESTAMPTZ` is an absolute
/// instant stored, like `TIMESTAMP`, as int64 microseconds since the Unix
/// epoch (UTC), so both are read the same way; accepting it directly avoids
/// the `::TIMESTAMP` cast, which converts to local time and can reorder
/// events across a daylight-saving change.
pub const TIMESTAMP_TYPES: [TypeId; 2] = [TypeId::Timestamp, TypeId::TimestampTz];
use quack_rs::error::ExtensionError;

/// Registers all behavioral analytics functions using a [`Connection`] handle.
///
/// This function is called from the `quack_rs::entry_point_v2!` macro closure
/// in `lib.rs`. Every function registers through the
/// [`Registrar`](quack_rs::connection::Registrar) trait.
///
/// # Errors
///
/// Returns an error if any function registration fails.
pub fn register_all(con: &Connection) -> Result<(), ExtensionError> {
    sessionize::register_sessionize(con)?;
    retention::register_retention(con)?;
    window_funnel::register_window_funnel(con)?;
    window_funnel_events::register_window_funnel_events(con)?;
    sequence::register_sequence_match(con)?;
    sequence::register_sequence_count(con)?;
    sequence_match_events::register_sequence_match_events(con)?;
    sequence_next_node::register_sequence_next_node(con)?;
    version::register_behavioral_version(con)?;
    Ok(())
}
