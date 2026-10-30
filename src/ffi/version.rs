// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! FFI registration for the `behavioral_version()` scalar function.
//!
//! Returns the extension's crate version (`CARGO_PKG_VERSION`) so users can
//! check which build is loaded — useful when the community extension channel,
//! a local build, and a pinned install can all serve different versions.
//!
//! Registered via [`quack_rs::scalar::ScalarFunctionBuilder`] with the
//! panic-safe [`quack_rs::scalar_callback!`] macro.

use super::view::{Output, VarcharKind};
use libduckdb_sys::duckdb_data_chunk_get_size;
use quack_rs::scalar::ScalarFunctionBuilder;
use quack_rs::scalar_callback;
use quack_rs::types::TypeId;

scalar_callback!(version_callback, |_info, input, output| {
    // SAFETY: `input` is the chunk of the running scalar callback.
    let rows = unsafe { duckdb_data_chunk_get_size(input) };
    // SAFETY: `output` is the callback's VARCHAR result vector with room for
    // one value per input row.
    let mut out = unsafe { Output::<VarcharKind>::from_raw(output, rows, 0) };
    for i in 0..out.count() {
        out.write(i, env!("CARGO_PKG_VERSION"));
    }
});

/// Registers the `behavioral_version` function with `DuckDB`.
///
/// Signature: `behavioral_version() → VARCHAR`
///
/// ```sql
/// SELECT behavioral_version();  -- the crate version, e.g. '0.11.0'
/// ```
///
/// # Errors
///
/// Returns an error if function registration fails.
pub fn register_behavioral_version(
    con: &impl quack_rs::connection::Registrar,
) -> Result<(), quack_rs::error::ExtensionError> {
    let builder = ScalarFunctionBuilder::new("behavioral_version")
        .returns(TypeId::Varchar)
        .function(version_callback);
    // SAFETY: `con` is the connection the entry point registers on (this
    // function's contract); the scalar has no state.
    unsafe { con.register_scalar(builder) }
}
