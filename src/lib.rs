//! # `behavioral` — Behavioral Analytics Extension for `DuckDB`
//!
//! Provides seven functions for behavioral analytics, inspired by `ClickHouse`'s
//! behavioral analytics functions but designed for `DuckDB`'s SQL dialect.
//!
//! ## Functions
//!
//! | Function | Type | Description |
//! |----------|------|-------------|
//! | `sessionize(ts, gap)` | Window | Assigns session IDs based on inactivity gaps |
//! | `retention(c1, ..., cN)` | Aggregate | Cohort retention analysis |
//! | `window_funnel(window, ts, c1, ..., cN)` | Aggregate | Conversion funnel analysis |
//! | `sequence_match(pattern, ts, c1, ..., cN)` | Aggregate | Pattern matching over event sequences |
//! | `sequence_count(pattern, ts, c1, ..., cN)` | Aggregate | Counts pattern matches in event sequences |
//! | `sequence_match_events(pattern, ts, c1, ..., cN)` | Aggregate | Returns matched step timestamps |
//! | `sequence_next_node(dir, base, ts, val, bc, e1, ..., eN)` | Aggregate | Next event after pattern match |
//!
//! ## Installation
//!
//! ```sql
//! INSTALL behavioral FROM community;
//! LOAD behavioral;
//! ```

pub mod common;
pub mod pattern;
pub mod retention;
pub mod sequence;
pub mod sequence_next_node;
pub mod sessionize;
pub mod window_funnel;

mod ffi;

/// Extension entry point called by `DuckDB` when the extension is loaded.
///
/// This is a hand-written C entry point that bypasses `duckdb::Connection` entirely,
/// avoiding fragile struct layout assumptions. Instead, we obtain the raw
/// `duckdb_connection` directly via the C API (`duckdb_connect`), register all
/// functions, and disconnect.
///
/// # Safety
///
/// Called by `DuckDB`'s extension loading mechanism via FFI.
/// `info` and `access` must be valid pointers provided by `DuckDB`.
#[no_mangle]
pub unsafe extern "C" fn behavioral_init_c_api(
    info: libduckdb_sys::duckdb_extension_info,
    access: *const libduckdb_sys::duckdb_extension_access,
) -> bool {
    match behavioral_init_internal(info, access) {
        Ok(result) => result,
        Err(e) => {
            let error_c_string = std::ffi::CString::new(e.to_string());
            if let Ok(err) = error_c_string {
                (*access).set_error.unwrap()(info, err.as_ptr());
            } else {
                let fallback = c"Extension init failed and could not allocate error string";
                (*access).set_error.unwrap()(info, fallback.as_ptr());
            }
            false
        }
    }
}

unsafe fn behavioral_init_internal(
    info: libduckdb_sys::duckdb_extension_info,
    access: *const libduckdb_sys::duckdb_extension_access,
) -> Result<bool, Box<dyn std::error::Error>> {
    // Initialize the extension API function pointers (populates global atomic statics).
    // "v1.2.0" matches the default minimum version used by duckdb-loadable-macros.
    let have_api = libduckdb_sys::duckdb_rs_extension_api_init(info, access, "v1.2.0")
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

    if !have_api {
        // API version mismatch — DuckDB is too old for this extension.
        return Ok(false);
    }

    // Get the database handle directly from the extension access struct.
    let db: libduckdb_sys::duckdb_database = *(*access).get_database.unwrap()(info);

    // Open a raw connection for function registration.
    let mut raw_con: libduckdb_sys::duckdb_connection = std::ptr::null_mut();
    let rc = libduckdb_sys::duckdb_connect(db, &mut raw_con);
    if rc != libduckdb_sys::DuckDBSuccess {
        return Err("Failed to open DuckDB connection for extension registration".into());
    }

    // Register all behavioral analytics functions.
    ffi::register_all_raw(raw_con);

    // Clean up the registration connection.
    libduckdb_sys::duckdb_disconnect(&mut raw_con);

    Ok(true)
}
