// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! FFI registration for the `sequence_next_node` aggregate function.

use crate::sequence_next_node::{NextNodeEvent, SequenceNextNodeState};
use libduckdb_sys::*;
use std::ffi::CString;
use std::sync::Arc;

/// Minimum number of event condition boolean parameters.
const MIN_EVENT_CONDITIONS: usize = 1;
/// Maximum number of event condition boolean parameters.
const MAX_EVENT_CONDITIONS: usize = 32;

/// Number of fixed parameters before the variable boolean event conditions.
///
/// Layout: VARCHAR (direction), VARCHAR (base), TIMESTAMP, VARCHAR (`event_column`),
/// BOOLEAN (`base_condition`), then BOOLEAN × N event conditions.
const FIXED_PARAMS: usize = 5;

/// Registers the `sequence_next_node` function with `DuckDB`.
///
/// Signature: `sequence_next_node(VARCHAR, VARCHAR, TIMESTAMP, VARCHAR, BOOLEAN, BOOLEAN [, ...]) -> VARCHAR`
///
/// Parameters:
/// - `direction`: `'forward'` or `'backward'`
/// - `base`: `'head'`, `'tail'`, `'first_match'`, or `'last_match'`
/// - `timestamp`: Event timestamp column
/// - `event_column`: Value column (returned as result)
/// - `base_condition`: Boolean condition for the base/anchor event
/// - `event1, event2, ...`: Sequential event conditions to match
///
/// # Safety
///
/// Requires a valid `duckdb_connection` handle.
pub unsafe fn register_sequence_next_node(con: duckdb_connection) {
    unsafe {
        let name = c"sequence_next_node";
        let set = duckdb_create_aggregate_function_set(name.as_ptr());

        for n in MIN_EVENT_CONDITIONS..=MAX_EVENT_CONDITIONS {
            let func = duckdb_create_aggregate_function();
            duckdb_aggregate_function_set_name(func, name.as_ptr());

            // Parameter 0: VARCHAR (direction)
            let varchar_type = duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_VARCHAR);
            duckdb_aggregate_function_add_parameter(func, varchar_type);
            duckdb_destroy_logical_type(&mut { varchar_type });

            // Parameter 1: VARCHAR (base)
            let varchar_type2 = duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_VARCHAR);
            duckdb_aggregate_function_add_parameter(func, varchar_type2);
            duckdb_destroy_logical_type(&mut { varchar_type2 });

            // Parameter 2: TIMESTAMP
            let ts_type = duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_TIMESTAMP);
            duckdb_aggregate_function_add_parameter(func, ts_type);
            duckdb_destroy_logical_type(&mut { ts_type });

            // Parameter 3: VARCHAR (event_column — value to return)
            let varchar_type3 = duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_VARCHAR);
            duckdb_aggregate_function_add_parameter(func, varchar_type3);
            duckdb_destroy_logical_type(&mut { varchar_type3 });

            // Parameter 4: BOOLEAN (base_condition)
            let bool_type = duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_BOOLEAN);
            duckdb_aggregate_function_add_parameter(func, bool_type);
            duckdb_destroy_logical_type(&mut { bool_type });

            // Parameters 5..5+n: BOOLEAN event conditions
            let bool_type2 = duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_BOOLEAN);
            for _ in 0..n {
                duckdb_aggregate_function_add_parameter(func, bool_type2);
            }
            duckdb_destroy_logical_type(&mut { bool_type2 });

            // Return type: VARCHAR (nullable)
            let ret_type = duckdb_create_logical_type(DUCKDB_TYPE_DUCKDB_TYPE_VARCHAR);
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

        let result = duckdb_register_aggregate_function_set(con, set);
        if result != DuckDBSuccess {
            eprintln!("behavioral: failed to register sequence_next_node function set");
        }

        duckdb_destroy_aggregate_function_set(&mut { set });
    }
}

#[repr(C)]
struct FfiState {
    inner: *mut SequenceNextNodeState,
}

/// Reads a VARCHAR value from a `DuckDB` vector at the given row index.
///
/// # Safety
///
/// Requires a valid `DuckDB` vector with VARCHAR data.
unsafe fn read_varchar(vec: duckdb_vector, row: usize) -> Option<String> {
    unsafe {
        let data = duckdb_vector_get_data(vec);
        let validity = duckdb_vector_get_validity(vec);

        if !validity.is_null() && !duckdb_validity_row_is_valid(validity, row as idx_t) {
            return None;
        }

        if data.is_null() {
            return None;
        }

        let str_struct =
            data.add(row * std::mem::size_of::<duckdb_string_t>()) as *const duckdb_string_t;
        let str_ptr = duckdb_string_t_data(str_struct.cast_mut());
        if str_ptr.is_null() {
            return None;
        }

        let len = duckdb_string_t_length(*str_struct);
        let bytes = std::slice::from_raw_parts(str_ptr as *const u8, len as usize);
        std::str::from_utf8(bytes).ok().map(String::from)
    }
}

// SAFETY: Pure computation returning byte size of FfiState.
unsafe extern "C" fn state_size(_info: duckdb_function_info) -> idx_t {
    std::mem::size_of::<FfiState>() as idx_t
}

// SAFETY: `state` is a DuckDB-allocated buffer of at least `state_size()` bytes.
unsafe extern "C" fn state_init(_info: duckdb_function_info, state: duckdb_aggregate_state) {
    unsafe {
        let ffi_state = &mut *(state as *mut FfiState);
        ffi_state.inner = Box::into_raw(Box::new(SequenceNextNodeState::new()));
    }
}

// SAFETY: `input` is a valid DuckDB data chunk with columns
// (VARCHAR, VARCHAR, TIMESTAMP, VARCHAR, BOOLEAN, BOOLEAN...) as registered.
// `states` points to `row_count` aggregate state pointers.
unsafe extern "C" fn state_update(
    _info: duckdb_function_info,
    input: duckdb_data_chunk,
    states: *mut duckdb_aggregate_state,
) {
    unsafe {
        let row_count = duckdb_data_chunk_get_size(input) as usize;
        let col_count = duckdb_data_chunk_get_column_count(input) as usize;
        let num_event_conditions = col_count.saturating_sub(FIXED_PARAMS);

        // Column 0: VARCHAR (direction)
        let direction_vec = duckdb_data_chunk_get_vector(input, 0);
        // Column 1: VARCHAR (base)
        let base_vec = duckdb_data_chunk_get_vector(input, 1);
        // Column 2: TIMESTAMP
        let ts_vec = duckdb_data_chunk_get_vector(input, 2);
        let ts_data = duckdb_vector_get_data(ts_vec) as *const i64;
        let ts_validity = duckdb_vector_get_validity(ts_vec);
        // Column 3: VARCHAR (event_column / value)
        let value_vec = duckdb_data_chunk_get_vector(input, 3);
        // Column 4: BOOLEAN (base_condition)
        let base_cond_vec = duckdb_data_chunk_get_vector(input, 4);
        let base_cond_data = duckdb_vector_get_data(base_cond_vec) as *const u8;
        let base_cond_validity = duckdb_vector_get_validity(base_cond_vec);

        // Columns 5..N: BOOLEAN event conditions
        let mut event_cond_vectors: Vec<(*const u8, *mut u64)> =
            Vec::with_capacity(num_event_conditions);
        for c in FIXED_PARAMS..col_count {
            let vec = duckdb_data_chunk_get_vector(input, c as idx_t);
            let data = duckdb_vector_get_data(vec) as *const u8;
            let validity = duckdb_vector_get_validity(vec);
            event_cond_vectors.push((data, validity));
        }

        for i in 0..row_count {
            let state_ptr = *states.add(i);
            let ffi_state = &mut *(state_ptr as *mut FfiState);
            let state = &mut *ffi_state.inner;

            // Parse direction (once per state)
            if state.direction.is_none() {
                if let Some(dir_str) = read_varchar(direction_vec, i) {
                    if let Some(dir) = SequenceNextNodeState::parse_direction(&dir_str) {
                        state.set_direction(dir);
                    }
                }
            }

            // Parse base (once per state)
            if state.base.is_none() {
                if let Some(base_str) = read_varchar(base_vec, i) {
                    if let Some(base) = SequenceNextNodeState::parse_base(&base_str) {
                        state.set_base(base);
                    }
                }
            }

            // Set num_steps (once per state)
            if state.num_steps == 0 {
                state.num_steps = num_event_conditions;
            }

            // Skip NULL timestamps
            if !ts_validity.is_null() && !duckdb_validity_row_is_valid(ts_validity, i as idx_t) {
                continue;
            }

            let timestamp = *ts_data.add(i);

            // Read event_column value (nullable), convert to Arc<str> for O(1) clone
            let value: Option<Arc<str>> = read_varchar(value_vec, i).map(Arc::from);

            // Read base_condition
            let base_condition = {
                let valid = base_cond_validity.is_null()
                    || duckdb_validity_row_is_valid(base_cond_validity, i as idx_t);
                valid && *base_cond_data.add(i) != 0
            };

            // Pack event conditions into u32 bitmask
            let mut bitmask: u32 = 0;
            for (c, &(data, validity)) in event_cond_vectors.iter().enumerate() {
                let valid =
                    validity.is_null() || duckdb_validity_row_is_valid(validity, i as idx_t);
                if valid && *data.add(i) != 0 {
                    bitmask |= 1 << c;
                }
            }

            state.update(NextNodeEvent {
                timestamp_us: timestamp,
                value,
                base_condition,
                conditions: bitmask,
            });
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
// valid DuckDB VARCHAR vector. NULL is set via validity bitmap when no match found.
unsafe extern "C" fn state_finalize(
    _info: duckdb_function_info,
    source: *mut duckdb_aggregate_state,
    result: duckdb_vector,
    count: idx_t,
    offset: idx_t,
) {
    unsafe {
        duckdb_vector_ensure_validity_writable(result);
        let validity = duckdb_vector_get_validity(result);

        for i in 0..count as usize {
            let state_ptr = *source.add(i);
            let ffi_state = &mut *(state_ptr as *mut FfiState);
            let idx = (offset as usize + i) as idx_t;

            if ffi_state.inner.is_null() {
                duckdb_validity_set_row_invalid(validity, idx);
                continue;
            }

            match (*ffi_state.inner).finalize() {
                Some(value) => {
                    // Interior null bytes would cause CString::new to fail.
                    // Strip them defensively rather than silently producing
                    // an empty string via unwrap_or_default().
                    let sanitized: String = value.replace('\0', "");
                    if let Ok(c_str) = CString::new(sanitized) {
                        duckdb_vector_assign_string_element(result, idx, c_str.as_ptr());
                    } else {
                        // Should be unreachable after stripping null bytes,
                        // but return NULL rather than panicking across FFI.
                        duckdb_validity_set_row_invalid(validity, idx);
                    }
                }
                None => {
                    duckdb_validity_set_row_invalid(validity, idx);
                }
            }
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
