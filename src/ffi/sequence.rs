//! FFI registration for `sequence_match` and `sequence_count` aggregate functions.

use crate::common::event::Event;
use crate::sequence::SequenceState;
use libduckdb_sys::*;
use std::ffi::CString;

/// Minimum number of boolean condition parameters for sequence functions.
const MIN_CONDITIONS: usize = 2;
/// Maximum number of boolean condition parameters for sequence functions.
const MAX_CONDITIONS: usize = 32;

/// Helper to register a sequence aggregate function set (shared by match and count).
///
/// Creates overloads for VARCHAR + TIMESTAMP + 2..=32 boolean parameters with
/// the given name, callbacks and return type.
///
/// # Safety
///
/// Requires a valid `duckdb_connection` handle.
unsafe fn register_sequence_function_set(
    con: duckdb_connection,
    func_name: &str,
    ret_type_id: DUCKDB_TYPE,
    state_size_fn: unsafe extern "C" fn(duckdb_function_info) -> idx_t,
    state_init_fn: unsafe extern "C" fn(duckdb_function_info, duckdb_aggregate_state),
    state_update_fn: unsafe extern "C" fn(
        duckdb_function_info,
        duckdb_data_chunk,
        *mut duckdb_aggregate_state,
    ),
    state_combine_fn: unsafe extern "C" fn(
        duckdb_function_info,
        *mut duckdb_aggregate_state,
        *mut duckdb_aggregate_state,
        idx_t,
    ),
    state_finalize_fn: unsafe extern "C" fn(
        duckdb_function_info,
        *mut duckdb_aggregate_state,
        duckdb_vector,
        idx_t,
        idx_t,
    ),
    state_destroy_fn: unsafe extern "C" fn(*mut duckdb_aggregate_state, idx_t),
) {
    unsafe {
        let name = CString::new(func_name).unwrap();
        let set = duckdb_create_aggregate_function_set(name.as_ptr());

        for n in MIN_CONDITIONS..=MAX_CONDITIONS {
            let func = duckdb_create_aggregate_function();
            duckdb_aggregate_function_set_name(func, name.as_ptr());

            // Parameter 0: VARCHAR (pattern)
            let varchar_type = duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_VARCHAR);
            duckdb_aggregate_function_add_parameter(func, varchar_type);
            duckdb_destroy_logical_type(&mut { varchar_type });

            // Parameter 1: TIMESTAMP
            let ts_type = duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_TIMESTAMP);
            duckdb_aggregate_function_add_parameter(func, ts_type);
            duckdb_destroy_logical_type(&mut { ts_type });

            // Parameters 2..2+n: BOOLEAN conditions
            let bool_type = duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_BOOLEAN);
            for _ in 0..n {
                duckdb_aggregate_function_add_parameter(func, bool_type);
            }
            duckdb_destroy_logical_type(&mut { bool_type });

            // Return type
            let ret_type = duckdb_create_logical_type(ret_type_id);
            duckdb_aggregate_function_set_return_type(func, ret_type);
            duckdb_destroy_logical_type(&mut { ret_type });

            duckdb_aggregate_function_set_functions(
                func,
                Some(state_size_fn),
                Some(state_init_fn),
                Some(state_update_fn),
                Some(state_combine_fn),
                Some(state_finalize_fn),
            );

            duckdb_aggregate_function_set_destructor(func, Some(state_destroy_fn));

            duckdb_add_aggregate_function_to_set(set, func);
            duckdb_destroy_aggregate_function(&mut { func });
        }

        let result = duckdb_register_aggregate_function_set(con, set);
        if result != DuckDBSuccess {
            eprintln!("behavioral: failed to register {func_name} function set");
        }

        duckdb_destroy_aggregate_function_set(&mut { set });
    }
}

/// Registers the `sequence_match` function with `DuckDB`.
///
/// Signature: `sequence_match(VARCHAR, TIMESTAMP, BOOLEAN, BOOLEAN [, ...]) -> BOOLEAN`
///
/// # Safety
///
/// Requires a valid `duckdb_connection` handle.
pub unsafe fn register_sequence_match(con: duckdb_connection) {
    unsafe {
        register_sequence_function_set(
            con,
            "sequence_match",
            DUCKDB_TYPE_DUCKDB_TYPE_BOOLEAN,
            match_state_size,
            match_state_init,
            sequence_state_update,
            sequence_state_combine,
            match_state_finalize,
            sequence_state_destroy,
        );
    }
}

/// Registers the `sequence_count` function with `DuckDB`.
///
/// Signature: `sequence_count(VARCHAR, TIMESTAMP, BOOLEAN, BOOLEAN [, ...]) -> BIGINT`
///
/// # Safety
///
/// Requires a valid `duckdb_connection` handle.
pub unsafe fn register_sequence_count(con: duckdb_connection) {
    unsafe {
        register_sequence_function_set(
            con,
            "sequence_count",
            DUCKDB_TYPE_DUCKDB_TYPE_BIGINT,
            count_state_size,
            count_state_init,
            sequence_state_update,
            sequence_state_combine,
            count_state_finalize,
            sequence_state_destroy,
        );
    }
}

// Both sequence_match and sequence_count share the same state structure and
// update/combine logic. Only finalize differs.

#[repr(C)]
struct FfiState {
    inner: *mut SequenceState,
}

// -- sequence_match callbacks --

// SAFETY: Pure computation returning byte size of FfiState.
unsafe extern "C" fn match_state_size(_info: duckdb_function_info) -> idx_t {
    std::mem::size_of::<FfiState>() as idx_t
}

// SAFETY: `state` is a DuckDB-allocated buffer of at least `state_size()` bytes.
unsafe extern "C" fn match_state_init(_info: duckdb_function_info, state: duckdb_aggregate_state) {
    unsafe {
        let ffi_state = &mut *(state as *mut FfiState);
        ffi_state.inner = Box::into_raw(Box::new(SequenceState::new()));
    }
}

// SAFETY: `source` points to `count` aggregate state pointers. `result` is a
// valid DuckDB BOOLEAN vector. Pattern errors produce NULL output via validity bitmap.
unsafe extern "C" fn match_state_finalize(
    _info: duckdb_function_info,
    source: *mut duckdb_aggregate_state,
    result: duckdb_vector,
    count: idx_t,
    offset: idx_t,
) {
    unsafe {
        let data = duckdb_vector_get_data(result) as *mut bool;
        let validity = duckdb_vector_get_validity(result);

        for i in 0..count as usize {
            let state_ptr = *source.add(i);
            let ffi_state = &mut *(state_ptr as *mut FfiState);
            let idx = offset as usize + i;

            if ffi_state.inner.is_null() {
                duckdb_validity_set_row_invalid(validity, idx as idx_t);
            } else {
                match (*ffi_state.inner).finalize_match() {
                    Ok(matched) => *data.add(idx) = matched,
                    Err(_) => duckdb_validity_set_row_invalid(validity, idx as idx_t),
                }
            }
        }
    }
}

// -- sequence_count callbacks --

// SAFETY: Pure computation returning byte size of FfiState.
unsafe extern "C" fn count_state_size(_info: duckdb_function_info) -> idx_t {
    std::mem::size_of::<FfiState>() as idx_t
}

// SAFETY: `state` is a DuckDB-allocated buffer of at least `state_size()` bytes.
unsafe extern "C" fn count_state_init(_info: duckdb_function_info, state: duckdb_aggregate_state) {
    unsafe {
        let ffi_state = &mut *(state as *mut FfiState);
        ffi_state.inner = Box::into_raw(Box::new(SequenceState::new()));
    }
}

// SAFETY: `source` points to `count` aggregate state pointers. `result` is a
// valid DuckDB BIGINT vector. Pattern errors produce NULL output via validity bitmap.
unsafe extern "C" fn count_state_finalize(
    _info: duckdb_function_info,
    source: *mut duckdb_aggregate_state,
    result: duckdb_vector,
    count: idx_t,
    offset: idx_t,
) {
    unsafe {
        let data = duckdb_vector_get_data(result) as *mut i64;
        let validity = duckdb_vector_get_validity(result);

        for i in 0..count as usize {
            let state_ptr = *source.add(i);
            let ffi_state = &mut *(state_ptr as *mut FfiState);
            let idx = offset as usize + i;

            if ffi_state.inner.is_null() {
                duckdb_validity_set_row_invalid(validity, idx as idx_t);
            } else {
                match (*ffi_state.inner).finalize_count() {
                    Ok(n) => *data.add(idx) = n,
                    Err(_) => duckdb_validity_set_row_invalid(validity, idx as idx_t),
                }
            }
        }
    }
}

// -- Shared update/combine/destroy callbacks --

// SAFETY: `input` is a valid DuckDB data chunk with columns (VARCHAR, TIMESTAMP,
// BOOLEAN...) as registered. `states` points to `row_count` aggregate state pointers.
// VARCHAR is read via DuckDB's duckdb_string_t API. The string data pointer and
// length are guaranteed valid by DuckDB for the lifetime of the data chunk.
unsafe extern "C" fn sequence_state_update(
    _info: duckdb_function_info,
    input: duckdb_data_chunk,
    states: *mut duckdb_aggregate_state,
) {
    unsafe {
        let row_count = duckdb_data_chunk_get_size(input) as usize;
        let col_count = duckdb_data_chunk_get_column_count(input) as usize;
        let num_conditions = col_count.saturating_sub(2); // subtract pattern and timestamp

        // Vector 0: VARCHAR (pattern)
        let pattern_vec = duckdb_data_chunk_get_vector(input, 0);

        // Vector 1: TIMESTAMP
        let ts_vec = duckdb_data_chunk_get_vector(input, 1);
        let ts_data = duckdb_vector_get_data(ts_vec) as *const i64;
        let ts_validity = duckdb_vector_get_validity(ts_vec);

        // Vectors 2..N: BOOLEAN conditions
        let mut cond_vectors: Vec<(*const bool, *mut u64)> = Vec::with_capacity(num_conditions);
        for c in 2..col_count {
            let vec = duckdb_data_chunk_get_vector(input, c as idx_t);
            let data = duckdb_vector_get_data(vec) as *const bool;
            let validity = duckdb_vector_get_validity(vec);
            cond_vectors.push((data, validity));
        }

        for i in 0..row_count {
            let state_ptr = *states.add(i);
            let ffi_state = &mut *(state_ptr as *mut FfiState);
            let state = &mut *ffi_state.inner;

            // Read pattern from first row (same for all rows in a group)
            if state.pattern_str.is_none() {
                let pattern_str_raw = duckdb_vector_get_data(pattern_vec);
                if !pattern_str_raw.is_null() {
                    // DuckDB VARCHAR is stored as duckdb_string_t
                    // For inline strings (<=12 chars): stored directly
                    // For longer strings: pointer + length
                    let str_struct = pattern_str_raw.add(i * std::mem::size_of::<duckdb_string_t>())
                        as *const duckdb_string_t;
                    let str_ptr = duckdb_string_t_data(str_struct.cast_mut());
                    if !str_ptr.is_null() {
                        let len = duckdb_string_t_length(*str_struct);
                        let bytes = std::slice::from_raw_parts(str_ptr as *const u8, len as usize);
                        if let Ok(s) = std::str::from_utf8(bytes) {
                            state.set_pattern(s);
                        }
                    }
                }
            }

            // Skip NULL timestamps
            if !ts_validity.is_null() && !duckdb_validity_row_is_valid(ts_validity, i as idx_t) {
                continue;
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

            state.update(Event::new(timestamp, bitmask));
        }
    }
}

// SAFETY: `source` and `target` point to `count` aggregate state pointers.
// Null checks guard against uninitialized states.
unsafe extern "C" fn sequence_state_combine(
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

// SAFETY: `state` points to `count` aggregate state pointers. Each inner pointer
// was allocated by `Box::into_raw` in `*_state_init`. We reclaim via `Box::from_raw`
// then null the pointer to prevent double-free.
unsafe extern "C" fn sequence_state_destroy(state: *mut duckdb_aggregate_state, count: idx_t) {
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
