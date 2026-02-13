//! FFI bindings for registering behavioral analytics functions with `DuckDB`.
//!
//! This module bridges the pure Rust implementations with `DuckDB`'s C Extension API.
//!
//! # Architecture
//!
//! `DuckDB` does not yet provide a high-level Rust API for aggregate functions.
//! We use the raw `libduckdb-sys` FFI bindings to register aggregate functions
//! with the five required callbacks: `state_size`, `init`, `update`, `combine`,
//! and `finalize`.
//!
//! Each function module (`sessionize`, `retention`, `window_funnel`, `sequence`)
//! implements these callbacks as `unsafe extern "C"` functions that delegate to
//! the pure Rust implementations in the parent modules.

pub mod retention;
pub mod sequence;
pub mod sequence_match_events;
pub mod sequence_next_node;
pub mod sessionize;
pub mod window_funnel;

/// Registers all behavioral analytics functions using a raw `duckdb_connection` handle.
///
/// This function is called from the custom C entry point in `lib.rs`, which obtains
/// the connection directly via `duckdb_connect` — avoiding any struct layout assumptions.
///
/// # Safety
///
/// The caller must ensure `raw_con` is a valid `duckdb_connection` handle.
pub fn register_all_raw(raw_con: libduckdb_sys::duckdb_connection) {
    // Safety: The raw connection handle is valid — obtained via duckdb_connect
    // in behavioral_init_internal and will be disconnected after registration.
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
