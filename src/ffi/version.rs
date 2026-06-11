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

use libduckdb_sys::*;
use quack_rs::scalar::ScalarFunctionBuilder;
use quack_rs::scalar_callback;
use quack_rs::types::TypeId;
use quack_rs::vector::VectorWriter;

scalar_callback!(version_callback, |_info, input, output| {
    // SAFETY: `input` and `output` are valid handles provided by DuckDB; the
    // output VARCHAR vector has room for `duckdb_data_chunk_get_size` rows.
    unsafe {
        let rows = duckdb_data_chunk_get_size(input) as usize;
        let mut writer = VectorWriter::new(output);
        for i in 0..rows {
            writer.write_varchar(i, env!("CARGO_PKG_VERSION"));
        }
    }
});

/// Registers the `behavioral_version` function with `DuckDB`.
///
/// Signature: `behavioral_version() → VARCHAR`
///
/// ```sql
/// SELECT behavioral_version();  -- e.g. '0.8.0'
/// ```
///
/// # Safety
///
/// Requires a valid connection implementing the [`Registrar`](quack_rs::connection::Registrar) trait.
///
/// # Errors
///
/// Returns an error if function registration fails.
pub unsafe fn register_behavioral_version(
    con: &impl quack_rs::connection::Registrar,
) -> Result<(), quack_rs::error::ExtensionError> {
    let builder = ScalarFunctionBuilder::new("behavioral_version")
        .returns(TypeId::Varchar)
        .function(version_callback);
    unsafe { con.register_scalar(builder) }
}
