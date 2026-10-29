// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! Any events, window and mode: `window_funnel` and `window_funnel_events`
//! must not panic, the step count must stay within 0..=steps, and the chain
//! `window_funnel_events` returns must have exactly that many timestamps.
#![no_main]

use arbitrary::Arbitrary;
use behavioral::common::event::Event;
use behavioral::window_funnel::{FunnelMode, WindowFunnelState};
use libfuzzer_sys::fuzz_target;

#[derive(Arbitrary, Debug)]
struct Input {
    window_seconds: u16,
    mode_bits: u8,
    steps: u8,
    events: Vec<(i16, u32)>,
}

fuzz_target!(|input: Input| {
    let steps = usize::from(input.steps % 32) + 1;
    let step_mask = u32::try_from((1u64 << steps) - 1).unwrap_or(u32::MAX);
    let mut mode = FunnelMode::from_bits(input.mode_bits & 0x3f);
    // The SQL layer rejects allow_reentry without strict_order.
    if mode.has(FunnelMode::ALLOW_REENTRY) {
        mode = mode.with(FunnelMode::STRICT_ORDER);
    }
    let mut state = WindowFunnelState::new();
    state.window_size_us = i64::from(input.window_seconds) * 1_000_000;
    state.window_set = true;
    state.mode = mode;
    state.mode_set = true;
    state.num_conditions = steps;
    for &(seconds, conditions) in input.events.iter().take(512) {
        state.update(
            Event::new(i64::from(seconds) * 1_000_000, conditions & step_mask),
            steps,
        );
    }
    let reached = state.clone().finalize();
    let chain = state.clone().finalize_events();
    assert!((0..=steps as i64).contains(&reached), "reached {reached}");
    assert_eq!(chain.len() as i64, reached, "mode {mode}");
});
