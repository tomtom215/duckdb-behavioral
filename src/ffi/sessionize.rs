//! FFI registration for the `sessionize` aggregate/window function.

use crate::common::timestamp::interval_to_micros;
use crate::sessionize::SessionizeBoundaryState;
use libduckdb_sys::*;
use std::ffi::CString;

/// Registers the `sessionize` function with `DuckDB`.
///
/// Signature: `sessionize(TIMESTAMP, INTERVAL) → BIGINT`
///
/// Used as a window function:
/// ```sql
/// SELECT sessionize(event_time, INTERVAL '30 minutes')
///   OVER (PARTITION BY user_id ORDER BY event_time)
/// FROM events
/// ```
///
/// # Safety
///
/// Requires a valid `duckdb_connection` handle.
pub unsafe fn register_sessionize(con: duckdb_connection) {
    unsafe {
        let func = duckdb_create_aggregate_function();

        let name = CString::new("sessionize").unwrap();
        duckdb_aggregate_function_set_name(func, name.as_ptr());

        // Parameter 0: TIMESTAMP (event timestamp)
        let ts_type = duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_TIMESTAMP);
        duckdb_aggregate_function_add_parameter(func, ts_type);
        duckdb_destroy_logical_type(&mut { ts_type });

        // Parameter 1: INTERVAL (gap threshold)
        let interval_type = duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_INTERVAL);
        duckdb_aggregate_function_add_parameter(func, interval_type);
        duckdb_destroy_logical_type(&mut { interval_type });

        // Return type: BIGINT (session ID)
        let ret_type = duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_BIGINT);
        duckdb_aggregate_function_set_return_type(func, ret_type);
        duckdb_destroy_logical_type(&mut { ret_type });

        // Set callbacks
        duckdb_aggregate_function_set_functions(
            func,
            Some(state_size),
            Some(state_init),
            Some(state_update),
            Some(state_combine),
            Some(state_finalize),
        );

        duckdb_aggregate_function_set_destructor(func, Some(state_destroy));

        let result = duckdb_register_aggregate_function(con, func);
        if result != DuckDBSuccess {
            eprintln!("behavioral: failed to register sessionize function");
        }

        duckdb_destroy_aggregate_function(&mut { func });
    }
}

/// State stored in `DuckDB`'s aggregate state buffer.
/// Points to a heap-allocated [`SessionizeBoundaryState`].
#[repr(C)]
struct FfiState {
    inner: *mut SessionizeBoundaryState,
}

// SAFETY: Returns the byte size of FfiState for DuckDB's state allocation.
// Pure computation with no pointer dereferences.
unsafe extern "C" fn state_size(_info: duckdb_function_info) -> idx_t {
    std::mem::size_of::<FfiState>() as idx_t
}

// SAFETY: `state` is a DuckDB-allocated buffer of at least `state_size()` bytes.
// We initialize the inner pointer to a heap-allocated SessionizeBoundaryState
// which will be freed in `state_destroy`.
unsafe extern "C" fn state_init(_info: duckdb_function_info, state: duckdb_aggregate_state) {
    unsafe {
        let ffi_state = &mut *(state as *mut FfiState);
        ffi_state.inner = Box::into_raw(Box::new(SessionizeBoundaryState::new()));
    }
}

// SAFETY: `input` is a valid DuckDB data chunk with the registered column types
// (TIMESTAMP, INTERVAL). `states` points to `row_count` aggregate state pointers,
// each initialized by `state_init`. All vector data pointers are valid for
// `row_count` elements. Validity bitmaps may be null (meaning all rows are valid).
unsafe extern "C" fn state_update(
    _info: duckdb_function_info,
    input: duckdb_data_chunk,
    states: *mut duckdb_aggregate_state,
) {
    unsafe {
        let row_count = duckdb_data_chunk_get_size(input) as usize;

        // Vector 0: TIMESTAMP (i64 microseconds)
        let ts_vec = duckdb_data_chunk_get_vector(input, 0);
        let ts_data = duckdb_vector_get_data(ts_vec) as *const i64;
        let ts_validity = duckdb_vector_get_validity(ts_vec);

        // Vector 1: INTERVAL (months: i32, days: i32, micros: i64)
        let interval_vec = duckdb_data_chunk_get_vector(input, 1);
        let interval_data = duckdb_vector_get_data(interval_vec) as *const u8;
        let interval_validity = duckdb_vector_get_validity(interval_vec);

        for i in 0..row_count {
            let state_ptr = *states.add(i);
            let ffi_state = &mut *(state_ptr as *mut FfiState);
            let state = &mut *ffi_state.inner;

            // NULL timestamps: mark state so finalize emits NULL for this row
            if !ts_validity.is_null() && !duckdb_validity_row_is_valid(ts_validity, i as idx_t) {
                state.mark_null_row();
                continue;
            }

            // Read interval threshold (same for all rows, but read per-row for safety)
            if !interval_validity.is_null()
                && !duckdb_validity_row_is_valid(interval_validity, i as idx_t)
            {
                continue;
            }

            // Parse interval: { months: i32, days: i32, micros: i64 } = 16 bytes
            let interval_ptr = interval_data.add(i * 16);
            let months = *(interval_ptr as *const i32);
            let days = *(interval_ptr.add(4) as *const i32);
            let micros = *(interval_ptr.add(8) as *const i64);

            if let Some(threshold_us) = interval_to_micros(months, days, micros) {
                state.threshold_us = threshold_us;
            }

            let timestamp = *ts_data.add(i);
            state.update(timestamp);
        }
    }
}

// SAFETY: `source` and `target` point to `count` aggregate state pointers,
// each initialized by `state_init`. Null checks guard against uninitialized states.
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

            let src_state = &*src_ffi.inner;
            let tgt_state = &*tgt_ffi.inner;
            let combined = tgt_state.combine(src_state);
            *tgt_ffi.inner = combined;
        }
    }
}

// SAFETY: `source` points to `count` aggregate state pointers. `result` is a
// valid DuckDB BIGINT vector with room for `offset + count` elements. Null
// inner pointers or empty states produce NULL output via validity bitmap.
unsafe extern "C" fn state_finalize(
    _info: duckdb_function_info,
    source: *mut duckdb_aggregate_state,
    result: duckdb_vector,
    count: idx_t,
    offset: idx_t,
) {
    unsafe {
        let data = duckdb_vector_get_data(result) as *mut i64;
        duckdb_vector_ensure_validity_writable(result);
        let validity = duckdb_vector_get_validity(result);

        for i in 0..count as usize {
            let state_ptr = *source.add(i);
            let ffi_state = &*(state_ptr as *const FfiState);
            let idx = offset as usize + i;

            if ffi_state.inner.is_null()
                || (*ffi_state.inner).first_ts.is_none()
                || (*ffi_state.inner).current_row_null
            {
                duckdb_validity_set_row_invalid(validity, idx as idx_t);
            } else {
                *data.add(idx) = (*ffi_state.inner).finalize();
            }
        }
    }
}

// SAFETY: `state` points to `count` aggregate state pointers. Each inner pointer
// was allocated by `Box::into_raw` in `state_init`. We reclaim the Box to free
// heap memory, then null the pointer to prevent double-free.
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
