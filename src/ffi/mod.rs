// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! FFI bindings for registering behavioral analytics functions with `DuckDB`.
//!
//! # Architecture
//!
//! All modules except [`sessionize`] use the `quack-rs` v0.4.0 SDK for safe
//! state management and vector I/O:
//!
//! - [`quack_rs::aggregate::AggregateFunctionSetBuilder`] — registers function
//!   sets with N overloads, automatically calling
//!   `duckdb_aggregate_function_set_name` on each.
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
//!   types for function registration.
//!
//! Functions returning `LIST(T)` (`retention`, `sequence_match_events`) use raw
//! `libduckdb-sys` for function set registration only — the builder's
//! `.returns(TypeId)` cannot express parameterized types. All other operations
//! (state, input, output) use quack-rs.
//!
//! The [`sessionize`] module is the sole exception: it requires window function
//! registration via the C API, which `quack-rs` does not support. It uses raw
//! `libduckdb-sys` calls directly.

pub mod retention;
pub mod sequence;
pub mod sequence_match_events;
pub mod sequence_next_node;
pub mod sessionize;
pub mod window_funnel;

/// Registers all behavioral analytics functions using a raw `duckdb_connection` handle.
///
/// This function is called from the `quack_rs::entry_point!` macro closure in `lib.rs`.
///
/// # Safety
///
/// The caller must ensure `raw_con` is a valid `duckdb_connection` handle.
pub fn register_all_raw(raw_con: libduckdb_sys::duckdb_connection) {
    // Safety: The raw connection handle is valid — obtained via duckdb_connect
    // in the entry_point! macro and will be disconnected after registration.
    unsafe {
        sessionize::register_sessionize(raw_con);
        retention::register_retention(raw_con);
        window_funnel::register_window_funnel(raw_con);
        sequence::register_sequence_match(raw_con);
        sequence::register_sequence_count(raw_con);
        sequence_match_events::register_sequence_match_events(raw_con);
        sequence_next_node::register_sequence_next_node(raw_con);
    }
}
