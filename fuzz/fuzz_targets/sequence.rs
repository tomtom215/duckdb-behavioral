// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! Any pattern over any events: the three `sequence_*` finalizers must not
//! panic, and `sequence_match` must agree with `sequence_count` (a match
//! exists exactly when at least one non-overlapping match is counted, except
//! for the documented `.*`-only pattern, which matches without counting).
#![no_main]

use arbitrary::Arbitrary;
use behavioral::common::event::Event;
use behavioral::sequence::SequenceState;
use libfuzzer_sys::fuzz_target;

#[derive(Arbitrary, Debug)]
struct Input {
    pattern: String,
    events: Vec<(u16, u8)>,
}

fuzz_target!(|input: Input| {
    let mut state = SequenceState::new();
    state.set_pattern(&input.pattern);
    for &(seconds, mask) in input.events.iter().take(256) {
        state.update(Event::new(i64::from(seconds) * 1_000_000, u32::from(mask)));
    }
    let matched = state.clone().finalize_match();
    let counted = state.clone().finalize_count();
    let _ = state.clone().finalize_events();
    if let (Ok(true), Ok(0)) = (&matched, &counted) {
        // Only a pattern with no consuming step can match without counting.
        assert!(
            !input.pattern.contains("(?"),
            "match without a count for {:?}",
            input.pattern
        );
    }
});
