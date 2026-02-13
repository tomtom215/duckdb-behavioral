//! FFI registration for the `sequence_match_events` aggregate function.

use crate::common::event::Event;
use crate::sequence::SequenceState;
use libduckdb_sys::*;
use std::ffi::CString;

/// Minimum number of boolean condition parameters for sequence functions.
const MIN_CONDITIONS: usize = 2;
/// Maximum number of boolean condition parameters for sequence functions.
const MAX_CONDITIONS: usize = 32;

/// Registers the `sequence_match_events` function with `DuckDB`.
///
/// Signature: `sequence_match_events(VARCHAR, TIMESTAMP, BOOLEAN, BOOLEAN [, ...]) -> LIST(TIMESTAMP)`
///
/// Returns an array of timestamps corresponding to each matched `(?N)` step in
/// the pattern. Empty array if no match.
///
/// # Safety
///
/// Requires a valid `duckdb_connection` handle.
pub unsafe fn register_sequence_match_events(con: duckdb_connection) {
    unsafe {
        let name = CString::new("sequence_match_events").unwrap();
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

            // Return type: LIST(TIMESTAMP)
            let inner_type = duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_TIMESTAMP);
            let list_type = duckdb_create_list_type(inner_type);
            duckdb_aggregate_function_set_return_type(func, list_type);
            duckdb_destroy_logical_type(&mut { inner_type });
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
            eprintln!("behavioral: failed to register sequence_match_events function set");
        }

        duckdb_destroy_aggregate_function_set(&mut { set });
    }
}

#[repr(C)]
struct FfiState {
    inner: *mut SequenceState,
}

// SAFETY: Pure computation returning byte size of FfiState.
unsafe extern "C" fn state_size(_info: duckdb_function_info) -> idx_t {
    std::mem::size_of::<FfiState>() as idx_t
}

// SAFETY: `state` is a DuckDB-allocated buffer of at least `state_size()` bytes.
unsafe extern "C" fn state_init(_info: duckdb_function_info, state: duckdb_aggregate_state) {
    unsafe {
        let ffi_state = &mut *(state as *mut FfiState);
        ffi_state.inner = Box::into_raw(Box::new(SequenceState::new()));
    }
}

// SAFETY: `input` is a valid DuckDB data chunk with columns (VARCHAR, TIMESTAMP,
// BOOLEAN...) as registered. `states` points to `row_count` aggregate state pointers.
unsafe extern "C" fn state_update(
    _info: duckdb_function_info,
    input: duckdb_data_chunk,
    states: *mut duckdb_aggregate_state,
) {
    unsafe {
        let row_count = duckdb_data_chunk_get_size(input) as usize;
        let col_count = duckdb_data_chunk_get_column_count(input) as usize;
        let num_conditions = col_count.saturating_sub(2);

        let pattern_vec = duckdb_data_chunk_get_vector(input, 0);

        let ts_vec = duckdb_data_chunk_get_vector(input, 1);
        let ts_data = duckdb_vector_get_data(ts_vec) as *const i64;
        let ts_validity = duckdb_vector_get_validity(ts_vec);

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

            if state.pattern_str.is_none() {
                let pattern_str_raw = duckdb_vector_get_data(pattern_vec);
                if !pattern_str_raw.is_null() {
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

            if !ts_validity.is_null() && !duckdb_validity_row_is_valid(ts_validity, i as idx_t) {
                continue;
            }

            let timestamp = *ts_data.add(i);

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
// valid DuckDB LIST(TIMESTAMP) vector. Each list entry is populated with the
// matched condition timestamps. Empty list on no match or pattern error.
unsafe extern "C" fn state_finalize(
    _info: duckdb_function_info,
    source: *mut duckdb_aggregate_state,
    result: duckdb_vector,
    count: idx_t,
    offset: idx_t,
) {
    unsafe {
        let list_child = duckdb_list_vector_get_child(result);
        let mut list_offset: idx_t = 0;

        for i in 0..count as usize {
            let state_ptr = *source.add(i);
            let ffi_state = &mut *(state_ptr as *mut FfiState);
            let idx = (offset as usize + i) as idx_t;

            if ffi_state.inner.is_null() {
                // Empty list for null state
                duckdb_list_vector_set_size(result, list_offset);
                let list_data = duckdb_vector_get_data(result) as *mut duckdb_list_entry;
                (*list_data.add(idx as usize)).offset = list_offset;
                (*list_data.add(idx as usize)).length = 0;
                continue;
            }

            let timestamps = (*ffi_state.inner).finalize_events().unwrap_or_default();

            let ts_count = timestamps.len() as idx_t;

            // Reserve space in the list child vector
            duckdb_list_vector_reserve(result, list_offset + ts_count);

            // Write timestamps into the child vector
            let child_data = duckdb_vector_get_data(list_child) as *mut i64;
            for (j, &ts) in timestamps.iter().enumerate() {
                *child_data.add((list_offset + j as idx_t) as usize) = ts;
            }

            // Set the list entry metadata
            let list_data = duckdb_vector_get_data(result) as *mut duckdb_list_entry;
            (*list_data.add(idx as usize)).offset = list_offset;
            (*list_data.add(idx as usize)).length = ts_count;

            list_offset += ts_count;
            duckdb_list_vector_set_size(result, list_offset);
        }
    }
}

// SAFETY: `state` points to `count` aggregate state pointers. Each inner pointer
// was allocated by `Box::into_raw` in `state_init`.
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
