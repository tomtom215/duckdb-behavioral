// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! The pattern parser must return `Ok` or `Err` for any string, never panic
//! or hang (a panic is a SQL error at best).
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(pattern) = std::str::from_utf8(data) {
        let _ = behavioral::pattern::parser::parse_pattern(pattern);
    }
});
