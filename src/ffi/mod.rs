// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! FFI bindings for registering behavioral analytics functions with `DuckDB`.
//!
//! # Architecture
//!
//! All modules except [`sessionize`] use the `quack-rs` v0.14 SDK for
//! registration, state management, and vector I/O:
//!
//! - [`quack_rs::aggregate::AggregateFunctionSetBuilder`] — registers function
//!   sets with N overloads. Supports both simple returns (`.returns(TypeId)`) and
//!   parameterized returns (`.returns_logical(LogicalType)`) for `LIST(T)` types.
//! - [`quack_rs::aggregate::FfiState<T>`] — `#[repr(C)]` wrapper providing safe
//!   init/destroy lifecycle and null-checked `with_state_mut()` accessors.
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
//! All aggregate functions use quack-rs builders for registration: the six
//! variadic functions use `AggregateFunctionSetBuilder` (including `retention`
//! and `sequence_match_events`, which use
//! `.returns_logical(LogicalType::list(...))` for their `LIST(T)` return
//! types), and [`sessionize`] uses
//! [`quack_rs::aggregate::AggregateFunctionBuilder`] for its single fixed
//! signature. No module in this crate registers through raw `libduckdb-sys`
//! calls anymore.
//!
//! # Entry Point
//!
//! Registration uses the [`quack_rs::entry_point_v2!`] macro, which provides a
//! [`Connection`] implementing the [`Registrar`](quack_rs::connection::Registrar) trait — a version-agnostic API
//! for registering extension components across `DuckDB` 1.4.x and 1.5.x.

pub mod retention;
pub mod sequence;
pub mod sequence_match_events;
pub mod sequence_next_node;
pub mod sessionize;
pub mod window_funnel;

use quack_rs::connection::Connection;
use quack_rs::error::ExtensionError;

/// Registers all behavioral analytics functions using a [`Connection`] handle.
///
/// This function is called from the `quack_rs::entry_point_v2!` macro closure
/// in `lib.rs`. Every function registers through the
/// [`Registrar`](quack_rs::connection::Registrar) trait.
///
/// # Safety
///
/// The caller must ensure `con` holds a valid `duckdb_connection` handle.
///
/// # Errors
///
/// Returns an error if any function registration fails.
pub unsafe fn register_all(con: &Connection) -> Result<(), ExtensionError> {
    // Safety: The Connection holds a valid handle — obtained via duckdb_connect
    // in the entry_point_v2! macro and will be disconnected after registration.
    unsafe {
        sessionize::register_sessionize(con)?;
        retention::register_retention(con)?;
        window_funnel::register_window_funnel(con)?;
        sequence::register_sequence_match(con)?;
        sequence::register_sequence_count(con)?;
        sequence_match_events::register_sequence_match_events(con)?;
        sequence_next_node::register_sequence_next_node(con)?;
    }

    Ok(())
}
