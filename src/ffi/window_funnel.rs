//! FFI registration for the `window_funnel` aggregate function.

use crate::common::event::Event;
use crate::common::timestamp::interval_to_micros;
use crate::window_funnel::{FunnelMode, WindowFunnelState};
use libduckdb_sys::*;
use std::ffi::CString;

/// Minimum number of boolean condition parameters for `window_funnel`.
const MIN_CONDITIONS: usize = 2;
/// Maximum number of boolean condition parameters for `window_funnel`.
const MAX_CONDITIONS: usize = 32;

/// Registers the `window_funnel` function with `DuckDB` as a function set
/// with overloads for two signatures:
///
/// 1. Without mode: `window_funnel(INTERVAL, TIMESTAMP, BOOLEAN, BOOLEAN [, ...]) -> INTEGER`
/// 2. With mode: `window_funnel(INTERVAL, VARCHAR, TIMESTAMP, BOOLEAN, BOOLEAN [, ...]) -> INTEGER`
///
/// The VARCHAR parameter accepts a comma-separated list of mode names
/// (e.g., `'strict_increase, strict_once'`).
///
/// # Safety
///
/// Requires a valid `duckdb_connection` handle.
pub unsafe fn register_window_funnel(con: duckdb_connection) {
    unsafe {
        let name = CString::new("window_funnel").unwrap();
        let set = duckdb_create_aggregate_function_set(name.as_ptr());

        // Register overloads WITHOUT mode parameter: (INTERVAL, TIMESTAMP, BOOL×N)
        for n in MIN_CONDITIONS..=MAX_CONDITIONS {
            let func = duckdb_create_aggregate_function();
            duckdb_aggregate_function_set_name(func, name.as_ptr());

            // Parameter 0: INTERVAL (window size)
            let interval_type = duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_INTERVAL);
            duckdb_aggregate_function_add_parameter(func, interval_type);
            duckdb_destroy_logical_type(&mut { interval_type });

            // Parameter 1: TIMESTAMP (event time)
            let ts_type = duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_TIMESTAMP);
            duckdb_aggregate_function_add_parameter(func, ts_type);
            duckdb_destroy_logical_type(&mut { ts_type });

            // Parameters 2..2+n: BOOLEAN conditions
            let bool_type = duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_BOOLEAN);
            for _ in 0..n {
                duckdb_aggregate_function_add_parameter(func, bool_type);
            }
            duckdb_destroy_logical_type(&mut { bool_type });

            // Return type: INTEGER
            let ret_type = duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_INTEGER);
            duckdb_aggregate_function_set_return_type(func, ret_type);
            duckdb_destroy_logical_type(&mut { ret_type });

            duckdb_aggregate_function_set_functions(
                func,
                Some(state_size),
                Some(state_init),
                Some(state_update),
                Some(state_combine),
                Some(state_finalize),
            );

            duckdb_aggregate_function_set_destructor(func, Some(state_destroy));

            duckdb_add_aggregate_function_to_set(set, func);
            duckdb_destroy_aggregate_function(&mut { func });
        }

        // Register overloads WITH mode parameter: (INTERVAL, VARCHAR, TIMESTAMP, BOOL×N)
        for n in MIN_CONDITIONS..=MAX_CONDITIONS {
            let func = duckdb_create_aggregate_function();
            duckdb_aggregate_function_set_name(func, name.as_ptr());

            // Parameter 0: INTERVAL (window size)
            let interval_type = duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_INTERVAL);
            duckdb_aggregate_function_add_parameter(func, interval_type);
            duckdb_destroy_logical_type(&mut { interval_type });

            // Parameter 1: VARCHAR (mode string)
            let varchar_type = duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_VARCHAR);
            duckdb_aggregate_function_add_parameter(func, varchar_type);
            duckdb_destroy_logical_type(&mut { varchar_type });

            // Parameter 2: TIMESTAMP (event time)
            let ts_type = duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_TIMESTAMP);
            duckdb_aggregate_function_add_parameter(func, ts_type);
            duckdb_destroy_logical_type(&mut { ts_type });

            // Parameters 3..3+n: BOOLEAN conditions
            let bool_type = duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_BOOLEAN);
            for _ in 0..n {
                duckdb_aggregate_function_add_parameter(func, bool_type);
            }
            duckdb_destroy_logical_type(&mut { bool_type });

            // Return type: INTEGER
            let ret_type = duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_INTEGER);
            duckdb_aggregate_function_set_return_type(func, ret_type);
            duckdb_destroy_logical_type(&mut { ret_type });

            duckdb_aggregate_function_set_functions(
                func,
                Some(state_size),
                Some(state_init),
                Some(state_update_with_mode),
                Some(state_combine),
                Some(state_finalize),
            );

            duckdb_aggregate_function_set_destructor(func, Some(state_destroy));

            duckdb_add_aggregate_function_to_set(set, func);
            duckdb_destroy_aggregate_function(&mut { func });
        }

        let result = duckdb_register_aggregate_function_set(con, set);
        if result != DuckDBSuccess {
            eprintln!("behavioral: failed to register window_funnel function set");
        }

        duckdb_destroy_aggregate_function_set(&mut { set });
    }
}

#[repr(C)]
struct FfiState {
    inner: *mut WindowFunnelState,
}

// SAFETY: Pure computation returning the byte size of FfiState.
unsafe extern "C" fn state_size(_info: duckdb_function_info) -> idx_t {
    std::mem::size_of::<FfiState>() as idx_t
}

// SAFETY: `state` is a DuckDB-allocated buffer of at least `state_size()` bytes.
// We initialize the inner pointer to a heap-allocated WindowFunnelState.
unsafe extern "C" fn state_init(_info: duckdb_function_info, state: duckdb_aggregate_state) {
    unsafe {
        let ffi_state = &mut *(state as *mut FfiState);
        ffi_state.inner = Box::into_raw(Box::new(WindowFunnelState::new()));
    }
}

// SAFETY: `input` is a valid DuckDB data chunk with columns (INTERVAL, TIMESTAMP,
// BOOLEAN...) as registered. `states` points to `row_count` aggregate state pointers
// initialized by `state_init`. Vector data pointers are valid for `row_count` elements.
// Interval data is read at 16-byte stride matching DuckDB's duckdb_interval layout.
unsafe extern "C" fn state_update(
    _info: duckdb_function_info,
    input: duckdb_data_chunk,
    states: *mut duckdb_aggregate_state,
) {
    // No mode parameter: INTERVAL(0), TIMESTAMP(1), BOOLEAN(2..N)
    unsafe {
        update_impl(input, states, false);
    }
}

// SAFETY: `input` is a valid DuckDB data chunk with columns (INTERVAL, VARCHAR,
// TIMESTAMP, BOOLEAN...) as registered. The VARCHAR at column 1 contains the mode
// string. `states` points to `row_count` aggregate state pointers.
unsafe extern "C" fn state_update_with_mode(
    _info: duckdb_function_info,
    input: duckdb_data_chunk,
    states: *mut duckdb_aggregate_state,
) {
    // With mode parameter: INTERVAL(0), VARCHAR(1), TIMESTAMP(2), BOOLEAN(3..N)
    unsafe {
        update_impl(input, states, true);
    }
}

/// Shared update implementation for both signatures.
///
/// When `has_mode` is true, column layout is:
///   \[0\] INTERVAL, \[1\] VARCHAR (mode), \[2\] TIMESTAMP, \[3..N\] BOOLEAN
/// When `has_mode` is false, column layout is:
///   \[0\] INTERVAL, \[1\] TIMESTAMP, \[2..N\] BOOLEAN
///
/// # Safety
///
/// Requires valid `input` data chunk and `states` aggregate state pointers.
unsafe fn update_impl(
    input: duckdb_data_chunk,
    states: *mut duckdb_aggregate_state,
    has_mode: bool,
) {
    unsafe {
        let row_count = duckdb_data_chunk_get_size(input) as usize;
        let col_count = duckdb_data_chunk_get_column_count(input) as usize;

        // Column indices depend on whether mode parameter is present
        let mode_col: Option<idx_t> = if has_mode { Some(1) } else { None };
        let ts_col: idx_t = if has_mode { 2 } else { 1 };
        let bool_start: usize = if has_mode { 3 } else { 2 };
        let num_conditions = col_count.saturating_sub(bool_start);

        // Vector 0: INTERVAL (window size) — always at column 0
        let interval_vec = duckdb_data_chunk_get_vector(input, 0);
        let interval_data = duckdb_vector_get_data(interval_vec) as *const u8;

        // TIMESTAMP vector
        let ts_vec = duckdb_data_chunk_get_vector(input, ts_col);
        let ts_data = duckdb_vector_get_data(ts_vec) as *const i64;
        let ts_validity = duckdb_vector_get_validity(ts_vec);

        // BOOLEAN condition vectors
        let mut cond_vectors: Vec<(*const bool, *mut u64)> = Vec::with_capacity(num_conditions);
        for c in bool_start..col_count {
            let vec = duckdb_data_chunk_get_vector(input, c as idx_t);
            let data = duckdb_vector_get_data(vec) as *const bool;
            let validity = duckdb_vector_get_validity(vec);
            cond_vectors.push((data, validity));
        }

        for i in 0..row_count {
            let state_ptr = *states.add(i);
            let ffi_state = &mut *(state_ptr as *mut FfiState);
            let state = &mut *ffi_state.inner;

            // Skip NULL timestamps
            if !ts_validity.is_null() && !duckdb_validity_row_is_valid(ts_validity, i as idx_t) {
                continue;
            }

            // Read window size from interval
            let interval_ptr = interval_data.add(i * 16);
            let months = *(interval_ptr as *const i32);
            let days = *(interval_ptr.add(4) as *const i32);
            let micros = *(interval_ptr.add(8) as *const i64);

            if let Some(window_us) = interval_to_micros(months, days, micros) {
                state.window_size_us = window_us;
            }

            // Parse mode string (once per state, from first row that has it)
            if let Some(mode_idx) = mode_col {
                if state.mode.is_default() {
                    let mode_vec = duckdb_data_chunk_get_vector(input, mode_idx);
                    let mode_str_raw = duckdb_vector_get_data(mode_vec);
                    if !mode_str_raw.is_null() {
                        let str_struct = mode_str_raw
                            .add(i * std::mem::size_of::<duckdb_string_t>())
                            as *const duckdb_string_t;
                        let str_ptr = duckdb_string_t_data(str_struct.cast_mut());
                        if !str_ptr.is_null() {
                            let len = duckdb_string_t_length(*str_struct);
                            let bytes =
                                std::slice::from_raw_parts(str_ptr as *const u8, len as usize);
                            if let Ok(s) = std::str::from_utf8(bytes) {
                                if let Ok(mode) = FunnelMode::parse_modes(s) {
                                    state.mode = mode;
                                }
                            }
                        }
                    }
                }
            }

            let timestamp = *ts_data.add(i);

            // Pack conditions into u32 bitmask (max 32 conditions from function set)
            let mut bitmask: u32 = 0;
            for (c, &(data, validity)) in cond_vectors.iter().enumerate() {
                let valid =
                    validity.is_null() || duckdb_validity_row_is_valid(validity, i as idx_t);
                if valid && *data.add(i) {
                    bitmask |= 1 << c;
                }
            }

            state.update(Event::new(timestamp, bitmask), num_conditions);
        }
    }
}

// SAFETY: `source` and `target` point to `count` aggregate state pointers.
// Null checks guard against uninitialized states.
unsafe extern "C" fn state_combine(
    _info: duckdb_function_info,
    source: *mut duckdb_aggregate_state,
    target: *mut duckdb_aggregate_state,
    count: idx_t,
) {
    unsafe {
        for i in 0..count as usize {
            let src_ptr = *source.add(i);
            let tgt_ptr = *target.add(i);
            let src_ffi = &*(src_ptr as *const FfiState);
            let tgt_ffi = &mut *(tgt_ptr as *mut FfiState);

            if src_ffi.inner.is_null() || tgt_ffi.inner.is_null() {
                continue;
            }

            (*tgt_ffi.inner).combine_in_place(&*src_ffi.inner);
        }
    }
}

// SAFETY: `source` points to `count` aggregate state pointers. `result` is a
// valid DuckDB INTEGER vector with room for `offset + count` elements.
unsafe extern "C" fn state_finalize(
    _info: duckdb_function_info,
    source: *mut duckdb_aggregate_state,
    result: duckdb_vector,
    count: idx_t,
    offset: idx_t,
) {
    unsafe {
        let data = duckdb_vector_get_data(result) as *mut i32;
        duckdb_vector_ensure_validity_writable(result);
        let validity = duckdb_vector_get_validity(result);

        for i in 0..count as usize {
            let state_ptr = *source.add(i);
            let ffi_state = &mut *(state_ptr as *mut FfiState);
            let idx = offset as usize + i;

            if ffi_state.inner.is_null() {
                duckdb_validity_set_row_invalid(validity, idx as idx_t);
            } else {
                let state_ref = &mut *ffi_state.inner;
                let step = state_ref.finalize();
                *data.add(idx) = step as i32;
            }
        }
    }
}

// SAFETY: `state` points to `count` aggregate state pointers. Each inner pointer
// was allocated by `Box::into_raw` in `state_init`. We reclaim via `Box::from_raw`
// then null the pointer to prevent double-free.
unsafe extern "C" fn state_destroy(state: *mut duckdb_aggregate_state, count: idx_t) {
    unsafe {
        for i in 0..count as usize {
            let state_ptr = *state.add(i);
            let ffi_state = &mut *(state_ptr as *mut FfiState);
            if !ffi_state.inner.is_null() {
                drop(Box::from_raw(ffi_state.inner));
                ffi_state.inner = std::ptr::null_mut();
            }
        }
    }
}
