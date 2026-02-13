//! FFI registration for the `retention` aggregate function.

use crate::retention::RetentionState;
use libduckdb_sys::*;
use std::ffi::CString;

/// Minimum number of boolean condition parameters for retention.
const MIN_CONDITIONS: usize = 2;
/// Maximum number of boolean condition parameters for retention.
const MAX_CONDITIONS: usize = 32;

/// Registers the `retention` function with `DuckDB` as a function set
/// with overloads for 2..=32 boolean parameters.
///
/// Signature: `retention(BOOLEAN, BOOLEAN [, BOOLEAN ...]) -> BOOLEAN[]`
///
/// # Safety
///
/// Requires a valid `duckdb_connection` handle.
pub unsafe fn register_retention(con: duckdb_connection) {
    unsafe {
        let name = CString::new("retention").unwrap();
        let set = duckdb_create_aggregate_function_set(name.as_ptr());

        for n in MIN_CONDITIONS..=MAX_CONDITIONS {
            let func = duckdb_create_aggregate_function();

            // Each function in the set must have a name set
            duckdb_aggregate_function_set_name(func, name.as_ptr());

            // Add n BOOLEAN parameters
            let bool_type = duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_BOOLEAN);
            for _ in 0..n {
                duckdb_aggregate_function_add_parameter(func, bool_type);
            }
            duckdb_destroy_logical_type(&mut { bool_type });

            // Return type: LIST(BOOLEAN)
            let inner_bool = duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_BOOLEAN);
            let list_type = duckdb_create_list_type(inner_bool);
            duckdb_aggregate_function_set_return_type(func, list_type);
            duckdb_destroy_logical_type(&mut { inner_bool });
            duckdb_destroy_logical_type(&mut { list_type });

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

        let result = duckdb_register_aggregate_function_set(con, set);
        if result != DuckDBSuccess {
            eprintln!("behavioral: failed to register retention function set");
        }

        duckdb_destroy_aggregate_function_set(&mut { set });
    }
}

#[repr(C)]
struct FfiState {
    inner: *mut RetentionState,
}

// SAFETY: Pure computation returning the byte size of FfiState.
unsafe extern "C" fn state_size(_info: duckdb_function_info) -> idx_t {
    std::mem::size_of::<FfiState>() as idx_t
}

// SAFETY: `state` is a DuckDB-allocated buffer of at least `state_size()` bytes.
// We initialize the inner pointer to a heap-allocated RetentionState.
unsafe extern "C" fn state_init(_info: duckdb_function_info, state: duckdb_aggregate_state) {
    unsafe {
        let ffi_state = &mut *(state as *mut FfiState);
        ffi_state.inner = Box::into_raw(Box::new(RetentionState::new()));
    }
}

// SAFETY: `input` is a valid DuckDB data chunk with N BOOLEAN columns (as registered).
// `states` points to `row_count` aggregate state pointers initialized by `state_init`.
// Vector data pointers are valid for `row_count` elements; null validity means all valid.
unsafe extern "C" fn state_update(
    _info: duckdb_function_info,
    input: duckdb_data_chunk,
    states: *mut duckdb_aggregate_state,
) {
    unsafe {
        let row_count = duckdb_data_chunk_get_size(input) as usize;
        let col_count = duckdb_data_chunk_get_column_count(input) as usize;

        // Read all boolean condition vectors
        let mut vectors: Vec<(*const bool, *mut u64)> = Vec::with_capacity(col_count);
        for c in 0..col_count {
            let vec = duckdb_data_chunk_get_vector(input, c as idx_t);
            let data = duckdb_vector_get_data(vec) as *const bool;
            let validity = duckdb_vector_get_validity(vec);
            vectors.push((data, validity));
        }

        for i in 0..row_count {
            let state_ptr = *states.add(i);
            let ffi_state = &mut *(state_ptr as *mut FfiState);
            let state = &mut *ffi_state.inner;

            let mut conditions = Vec::with_capacity(col_count);
            for &(data, validity) in &vectors {
                let valid =
                    validity.is_null() || duckdb_validity_row_is_valid(validity, i as idx_t);
                let value = if valid { *data.add(i) } else { false };
                conditions.push(value);
            }

            state.update(&conditions);
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

            let combined = (*tgt_ffi.inner).combine(&*src_ffi.inner);
            *tgt_ffi.inner = combined;
        }
    }
}

// SAFETY: `source` points to `count` aggregate state pointers. `result` is a
// valid DuckDB LIST(BOOLEAN) vector. We use DuckDB's list vector APIs to write
// entries: reserve space, set size, write list_entry offsets, then write child data.
unsafe extern "C" fn state_finalize(
    _info: duckdb_function_info,
    source: *mut duckdb_aggregate_state,
    result: duckdb_vector,
    count: idx_t,
    offset: idx_t,
) {
    unsafe {
        // Result is a LIST(BOOLEAN) vector
        for i in 0..count as usize {
            let state_ptr = *source.add(i);
            let ffi_state = &*(state_ptr as *const FfiState);
            let idx = offset as usize + i;

            if ffi_state.inner.is_null() {
                let validity = duckdb_vector_get_validity(result);
                duckdb_validity_set_row_invalid(validity, idx as idx_t);
                continue;
            }

            let retention_result = (*ffi_state.inner).finalize();

            // Write list entry to the result vector
            let child = duckdb_list_vector_get_child(result);
            let current_size = duckdb_list_vector_get_size(result);
            let new_size = current_size + retention_result.len() as idx_t;
            duckdb_list_vector_set_size(result, new_size);
            duckdb_list_vector_reserve(result, new_size);

            // Set the list entry (offset and length)
            let list_data = duckdb_vector_get_data(result) as *mut duckdb_list_entry;
            (*list_data.add(idx)).offset = current_size;
            (*list_data.add(idx)).length = retention_result.len() as idx_t;

            // Write boolean values to child vector
            let child_data = duckdb_vector_get_data(child) as *mut bool;
            for (j, &val) in retention_result.iter().enumerate() {
                *child_data.add(current_size as usize + j) = val;
            }
        }
    }
}

// SAFETY: `state` points to `count` aggregate state pointers. Each inner pointer
// was allocated by `Box::into_raw` in `state_init`. We reclaim via `Box::from_raw`
// to free heap memory, then null the pointer to prevent double-free.
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
