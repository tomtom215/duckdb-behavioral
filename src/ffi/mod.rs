// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! FFI bindings for registering behavioral analytics functions with `DuckDB`.
//!
//! # Architecture
//!
//! All modules except [`sessionize`] use the `quack-rs` SDK for function
//! registration and safe state/vector management:
//!
//! - [`quack_rs::aggregate::AggregateFunctionSetBuilder`] — registers function
//!   sets with N overloads, automatically calling
//!   `duckdb_aggregate_function_set_name` on each (preventing the Session 10 bug).
//! - [`quack_rs::aggregate::FfiState<T>`] — `#[repr(C)]` wrapper providing safe
//!   init/destroy lifecycle and null-checked `with_state_mut()` accessors.
//! - [`quack_rs::vector::VectorReader`] — safe vector reading with `read_bool()`
//!   (reads as `u8 != 0`), `read_str()` (handles `duckdb_string_t` format), and
//!   `read_interval()`.
//! - [`quack_rs::vector::VectorWriter`] — safe vector writing with automatic
//!   `ensure_validity_writable` before null writes.
//!
//! The [`sessionize`] module is the sole exception: it requires window function
//! registration via the C API, which `quack-rs` does not yet support (v0.3.0).
//! It uses raw `libduckdb-sys` calls directly.

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
