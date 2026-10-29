// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! Randomized checks of the guarantees the aggregate states make to `DuckDB`.
//!
//! `DuckDB` feeds a group's rows to many partial states in an order and a
//! combine-tree shape of its own choosing, into targets it initialises to
//! zero. These tests state what that means for the result and check it on
//! random inputs:
//!
//! - **Reference**: `window_funnel` equals an exhaustive reference (every
//!   chain kept, written from the semantics in `CLAUDE.md`, sharing no code
//!   with the engine); `retention`, `sequence_next_node` and `sessionize`
//!   equal short naive definitions.
//! - **Invariance**: the result does not depend on the order rows arrive in,
//!   on how they are split across partial states, on the shape of the combine
//!   tree, on combining into a fresh default target, or on finalizing twice.
//!   `sessionize` is the exception to the first: it reads rows in window
//!   order (ascending timestamps), so its test sorts first and checks the
//!   split, the tree, the fresh target and the double finalize.
//!
//! The pattern matcher is checked against its own oracle in
//! `pattern::reference_nfa`; the invariance of the `sequence_*` states is
//! checked here.

use crate::common::event::Event;
use crate::retention::RetentionState;
use crate::sequence::SequenceState;
use crate::sequence_next_node::{Base, Direction, NextNodeEvent, SequenceNextNodeState};
use crate::sessionize::SessionizeBoundaryState;
use crate::window_funnel::{FunnelMode, WindowFunnelState};
use proptest::collection::vec;
use proptest::prelude::*;
use std::collections::HashSet;
use std::sync::Arc;

// ── Deterministic randomness for tree shapes and shuffles ────────────────

/// `SplitMix64`. The seed comes from proptest, so a failing case replays.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }

    fn coin(&mut self) -> bool {
        self.next() & 1 == 0
    }

    fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            items.swap(i, self.below(i + 1));
        }
    }
}

/// Merges `parts` pairwise in a random order and orientation, as `DuckDB`'s
/// segment tree and parallel aggregation may.
fn random_tree<T>(rng: &mut Rng, mut parts: Vec<T>, merge: impl Fn(&mut T, &T)) -> T {
    while parts.len() > 1 {
        let a = parts.swap_remove(rng.below(parts.len()));
        let b = parts.swap_remove(rng.below(parts.len()));
        let (mut into, from) = if rng.coin() { (a, b) } else { (b, a) };
        merge(&mut into, &from);
        parts.push(into);
    }
    parts.pop().expect("at least one part")
}

// ── window_funnel against an exhaustive reference ────────────────────────

/// One chain of events through the funnel, as the reference tracks it.
#[derive(Clone, PartialEq, Eq)]
struct RefChain {
    first: i64,
    last: i64,
    used: Vec<usize>,
    path: Vec<i64>,
}

/// The five behaviour switches of `window_funnel`'s modes. `dedup` is
/// `FunnelMode::STRICT_DEDUPLICATION` (SQL `'strict_deduplication'`); `increase` is either
/// of the two increasing-timestamp flags, which behave alike.
#[derive(Clone, Copy)]
#[allow(clippy::struct_excessive_bools)] // one bool per mode switch, by design
struct Flags {
    dedup: bool,
    order: bool,
    increase: bool,
    once: bool,
    reentry: bool,
}

impl Flags {
    fn from_bits(bits: u8) -> Self {
        Self {
            dedup: bits & FunnelMode::STRICT_DEDUPLICATION.bits() != 0,
            order: bits & FunnelMode::STRICT_ORDER.bits() != 0,
            increase: bits
                & (FunnelMode::STRICT_INCREASE.bits() | FunnelMode::TIMESTAMP_DEDUP.bits())
                != 0,
            once: bits & FunnelMode::STRICT_ONCE.bits() != 0,
            reentry: bits & FunnelMode::ALLOW_REENTRY.bits() != 0,
        }
    }
}

/// Exhaustive `windowFunnel`: `ClickHouse`'s one-entry-per-true-condition scan
/// in `(timestamp, conditions)` order, but keeping *every* chain per level
/// instead of the latest one. Returns the levels reached and the chains at
/// the top level when the scan stopped.
fn funnel_reference(
    events: &[Event],
    steps: usize,
    window: i64,
    flags: Flags,
) -> (usize, Vec<RefChain>) {
    let mut events = events.to_vec();
    events.sort_by_key(|e| (e.timestamp_us, e.conditions));
    if events.is_empty() {
        return (0, Vec::new());
    }
    let mut levels: Vec<Vec<RefChain>> = vec![Vec::new(); steps];
    let mut entered = false;
    let mut i = 0;
    'scan: while i < events.len() {
        let ts = events[i].timestamp_us;
        let mut j = i;
        while j < events.len() && events[j].timestamp_us == ts {
            j += 1;
        }
        // A row with no true condition breaks the chain under strict_order.
        if flags.order && entered && events[i..j].iter().any(|e| e.conditions == 0) {
            break 'scan;
        }
        for c in 0..steps {
            for (e, event) in events.iter().enumerate().take(j).skip(i) {
                if event.conditions >> c & 1 == 0 {
                    continue;
                }
                if c == 0 {
                    levels[0].push(RefChain {
                        first: ts,
                        last: ts,
                        used: vec![e],
                        path: vec![ts],
                    });
                    entered = true;
                    continue;
                }
                if flags.dedup && !levels[c].is_empty() {
                    break 'scan;
                }
                if flags.order && entered && levels[c - 1].is_empty() {
                    if flags.reentry {
                        continue;
                    }
                    break 'scan;
                }
                let mut added = Vec::new();
                for chain in &levels[c - 1] {
                    if i128::from(ts) - i128::from(chain.first) > i128::from(window) {
                        continue;
                    }
                    if flags.increase && chain.last >= ts {
                        continue;
                    }
                    if flags.once && chain.used.contains(&e) {
                        continue;
                    }
                    let mut next = chain.clone();
                    next.last = ts;
                    next.used.push(e);
                    next.path.push(ts);
                    added.push(next);
                }
                let extended = !added.is_empty();
                for chain in added {
                    if !levels[c].contains(&chain) {
                        levels[c].push(chain);
                    }
                }
                if extended && c + 1 == steps {
                    return (steps, levels[steps - 1].clone());
                }
            }
        }
        i = j;
    }
    let reached = levels.iter().take_while(|level| !level.is_empty()).count();
    let top = if reached == 0 {
        Vec::new()
    } else {
        levels[reached - 1].clone()
    };
    (reached, top)
}

/// Mode bits: any subset of the five switches, with `allow_reentry` only
/// beside `strict_order` (the combination `ClickHouse` accepts).
fn mode_bits() -> impl Strategy<Value = u8> {
    (0u8..32, any::<bool>()).prop_map(|(switches, reentry)| {
        // The low five bits are STRICT_DEDUPLICATION, STRICT_ORDER, TIMESTAMP_DEDUP
        // (`timestamp_dedup`), STRICT_INCREASE and STRICT_ONCE.
        let mut bits = switches;
        if bits & FunnelMode::STRICT_ORDER.bits() != 0 && reentry {
            bits |= FunnelMode::ALLOW_REENTRY.bits();
        }
        bits
    })
}

/// A funnel case: step count, events (ties and 1 µs offsets included), window
/// and mode.
fn funnel_case() -> impl Strategy<Value = (usize, Vec<Event>, i64, u8)> {
    (1usize..=5).prop_flat_map(|steps| {
        let event = (0i64..7, 0i64..2, 0u32..(1 << steps))
            .prop_map(|(sec, nudge, conditions)| Event::new(sec * 1_000_000 + nudge, conditions));
        (
            Just(steps),
            vec(event, 0..=10),
            prop::sample::select(vec![
                0i64, 1, 1_000_000, 2_000_000, 3_000_000, 5_000_000, 1_000_001,
            ]),
            mode_bits(),
        )
    })
}

fn funnel_state(window: i64, mode: u8) -> WindowFunnelState {
    let mut state = WindowFunnelState::new();
    state.window_size_us = window;
    state.window_set = true;
    state.mode = FunnelMode::from_bits(mode);
    state.mode_set = true;
    state
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(3000))]

    /// `finalize` and `finalize_events` equal the exhaustive reference, and the
    /// reported chain is one the reference found. When several chains tie on
    /// the entry timestamp, the engine reports the one that completed latest.
    #[test]
    fn funnel_matches_exhaustive_reference((steps, events, window, mode) in funnel_case()) {
        let mut state = funnel_state(window, mode);
        for &event in &events {
            state.update(event, steps);
        }
        let reached = state.finalize();
        let path = state.finalize_events();

        let (want, top) = funnel_reference(&events, steps, window, Flags::from_bits(mode));
        prop_assert_eq!(reached, want as i64);
        prop_assert_eq!(path.len(), want);
        if want > 0 {
            let latest_entry = top.iter().map(|c| c.first).max().unwrap();
            let candidates: Vec<&RefChain> =
                top.iter().filter(|c| c.first == latest_entry).collect();
            prop_assert!(
                candidates.iter().any(|c| c.path == path),
                "reported chain {:?} is not among the reference's {:?}",
                path,
                candidates.iter().map(|c| &c.path).collect::<Vec<_>>()
            );
            let distinct: HashSet<_> = candidates.iter().map(|c| &c.path).collect();
            if distinct.len() > 1 {
                let latest_finish = candidates.iter().map(|c| c.last).max().unwrap();
                prop_assert_eq!(*path.last().unwrap(), latest_finish);
            }
        }
    }

    /// Row order, partitioning, combine-tree shape, a fresh default target and
    /// repeated finalization never change the result.
    #[test]
    fn funnel_is_invariant_under_order_and_combine(
        (steps, events, window, mode) in funnel_case(),
        seed in any::<u64>(),
    ) {
        let mut rng = Rng(seed);
        let mut whole = funnel_state(window, mode);
        for &event in &events {
            whole.update(event, steps);
        }
        let want_steps = whole.clone().finalize();
        let want_path = whole.clone().finalize_events();

        let mut again = whole.clone();
        prop_assert_eq!(again.finalize(), again.finalize());
        prop_assert_eq!(again.finalize_events(), again.finalize_events());

        let mut shuffled = events;
        rng.shuffle(&mut shuffled);
        let k = 1 + rng.below(4);
        let mut parts: Vec<WindowFunnelState> = (0..k).map(|_| funnel_state(window, mode)).collect();
        for &event in &shuffled {
            let part = rng.below(k);
            parts[part].update(event, steps);
        }
        // Parts DuckDB initialised but never fed carry no configuration.
        for part in &mut parts {
            if part.events.is_empty() && rng.coin() {
                *part = WindowFunnelState::new();
            }
        }
        for _ in 0..rng.below(3) {
            parts.push(WindowFunnelState::new());
        }
        let mut tree = random_tree(&mut rng, parts, WindowFunnelState::combine_in_place);
        if rng.coin() {
            let mut fresh = WindowFunnelState::new();
            fresh.combine_in_place(&tree);
            tree = fresh;
        }
        prop_assert_eq!(tree.clone().finalize(), want_steps);
        prop_assert_eq!(tree.finalize_events(), want_path);
    }
}

// ── sequence_match / sequence_count / sequence_match_events ──────────────

fn pattern() -> impl Strategy<Value = String> {
    let token = prop_oneof![
        3 => (1u8..=3).prop_map(|n| format!("(?{n})")),
        1 => Just(".".to_string()),
        2 => Just(".*".to_string()),
        2 => (0usize..6, 0u8..3).prop_map(|(op, n)| {
            let op = [">=", "<=", ">", "<", "==", "!="][op];
            format!("(?t{op}{n})")
        }),
    ];
    (any::<bool>(), 1u8..=3, vec(token, 0..5)).prop_map(|(lead, first, rest)| {
        // A time constraint needs a step before it, so the pattern starts
        // with a step (optionally after `.*`).
        let mut text = String::new();
        if lead {
            text.push_str(".*");
        }
        text.push_str("(?");
        text.push_str(&first.to_string());
        text.push(')');
        text.extend(rest);
        text
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(3000))]

    #[test]
    fn sequence_is_invariant_under_order_and_combine(
        text in pattern(),
        raw in vec((0i64..6, 0i64..2, 0u32..8), 0..12),
        seed in any::<u64>(),
    ) {
        let mut rng = Rng(seed);
        let events: Vec<Event> = raw
            .iter()
            .map(|&(sec, half, conditions)| Event::new(sec * 1_000_000 + half * 500_000, conditions))
            .collect();
        let make = || {
            let mut state = SequenceState::new();
            state.set_pattern(&text);
            state
        };
        let run = |state: &mut SequenceState| {
            (
                state.finalize_match().unwrap(),
                state.finalize_count().unwrap(),
                state.finalize_events().unwrap(),
            )
        };
        let mut whole = make();
        for &event in &events {
            whole.update(event);
        }
        let want = run(&mut whole.clone());
        let mut again = whole.clone();
        prop_assert_eq!(run(&mut again), run(&mut again));

        let mut shuffled = events;
        rng.shuffle(&mut shuffled);
        let k = 1 + rng.below(3);
        let mut parts: Vec<SequenceState> = (0..k).map(|_| make()).collect();
        for &event in &shuffled {
            let part = rng.below(k);
            parts[part].update(event);
        }
        for part in &mut parts {
            if part.events.is_empty() && rng.coin() {
                *part = SequenceState::new();
            }
        }
        let mut tree = random_tree(&mut rng, parts, SequenceState::combine_in_place);
        if rng.coin() {
            let mut fresh = SequenceState::new();
            fresh.combine_in_place(&tree);
            tree = fresh;
        }
        if tree.pattern_str.is_none() {
            // Every part was a default state: no row ever carried the pattern.
            tree.set_pattern(&text);
        }
        prop_assert_eq!(run(&mut tree), want);
    }
}

// ── retention ────────────────────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(3000))]

    /// `retention` is `[a && any(row[i])]` where `a` is "some row has the first
    /// condition", and empty for no rows; every combine order agrees.
    #[test]
    fn retention_matches_definition_and_combines(
        (width, rows) in (2usize..=32).prop_flat_map(|width| (Just(width), vec(vec(any::<bool>(), width), 0..6))),
        seed in any::<u64>(),
    ) {
        let mut rng = Rng(seed);
        let want: Vec<bool> = if rows.is_empty() {
            Vec::new()
        } else {
            let first = rows.iter().any(|row| row[0]);
            (0..width).map(|i| first && rows.iter().any(|row| row[i])).collect()
        };
        let mut whole = RetentionState::new();
        for row in &rows {
            whole.update(row);
        }
        prop_assert_eq!(&whole.finalize(), &want);

        if !rows.is_empty() {
            let parts: Vec<RetentionState> = rows
                .iter()
                .map(|row| {
                    let mut part = RetentionState::new();
                    part.update(row);
                    part
                })
                .collect();
            let mut tree = random_tree(&mut rng, parts, |a, b| *a = a.combine(b));
            if rng.coin() {
                tree = RetentionState::new().combine(&tree);
            }
            prop_assert_eq!(tree.finalize(), want);
        }
    }
}

// ── sequence_next_node ───────────────────────────────────────────────────

type RawNextEvent = (i64, usize, bool, u32);

/// The definition from `CLAUDE.md`: one anchor chosen by `base`, a chain of
/// consecutive sorted events satisfying conditions 1..=`steps`, and the value
/// of the event just past the chain.
fn next_node_reference(
    events: &[RawNextEvent],
    values: &[Option<&str>],
    steps: u32,
    direction: Direction,
    base: Base,
) -> Option<String> {
    let mut sorted: Vec<(i64, Option<&str>, bool, u32)> = events
        .iter()
        .map(|&(ts, v, base_condition, conditions)| (ts, values[v], base_condition, conditions))
        .collect();
    sorted.sort();
    let len = sorted.len() as i64;
    let anchor = match base {
        Base::Head => (len > 0 && sorted[0].2).then_some(0),
        Base::Tail => (len > 0 && sorted[len as usize - 1].2).then(|| len - 1),
        Base::FirstMatch => {
            (0..len).find(|&i| sorted[i as usize].2 && sorted[i as usize].3 & 1 == 1)
        }
        Base::LastMatch => (0..len)
            .rev()
            .find(|&i| sorted[i as usize].2 && sorted[i as usize].3 & 1 == 1),
    }?;
    let step = if direction == Direction::Forward {
        1
    } else {
        -1
    };
    for k in 0..i64::from(steps) {
        let pos = anchor + step * k;
        if pos < 0 || pos >= len || sorted[pos as usize].3 >> k & 1 == 0 {
            return None;
        }
    }
    let next = anchor + step * i64::from(steps);
    if next < 0 || next >= len {
        return None;
    }
    sorted[next as usize].1.map(str::to_owned)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1500))]

    #[test]
    fn next_node_matches_definition_and_combines(
        steps in 1u32..=3,
        events in vec((0i64..4, 0usize..4, prop::bool::weighted(2.0 / 3.0), 0u32..8), 0..9),
        seed in any::<u64>(),
    ) {
        let mut rng = Rng(seed);
        let values: [Option<&str>; 4] = [None, Some("a"), Some("b"), Some("c")];
        // Keep the conditions inside the registered step count.
        let events: Vec<RawNextEvent> = events
            .into_iter()
            .map(|(ts, v, b, c)| (ts * 1000, v, b, c & ((1 << steps) - 1)))
            .collect();
        let to_event = |raw: &RawNextEvent| {
            NextNodeEvent::new(raw.0, values[raw.1].map(Arc::from), raw.2, raw.3)
        };
        for direction in [Direction::Forward, Direction::Backward] {
            for base in [Base::Head, Base::Tail, Base::FirstMatch, Base::LastMatch] {
                let make = || {
                    let mut state = SequenceNextNodeState::new();
                    state.direction = Some(direction);
                    state.base = Some(base);
                    state.num_steps = steps as usize;
                    state
                };
                let mut whole = make();
                for raw in &events {
                    whole.update(to_event(raw));
                }
                let got = whole.finalize();
                prop_assert_eq!(
                    &got,
                    &next_node_reference(&events, &values, steps, direction, base),
                    "{:?} {:?}", direction, base
                );

                let mut shuffled = events.clone();
                rng.shuffle(&mut shuffled);
                let k = 1 + rng.below(3);
                let mut parts: Vec<SequenceNextNodeState> = (0..k).map(|_| make()).collect();
                for raw in &shuffled {
                    let part = rng.below(k);
                    parts[part].update(to_event(raw));
                }
                for part in &mut parts {
                    if part.has_no_events() && rng.coin() {
                        *part = SequenceNextNodeState::new();
                    }
                }
                let mut tree = random_tree(&mut rng, parts, SequenceNextNodeState::combine_in_place);
                if rng.coin() {
                    let mut fresh = SequenceNextNodeState::new();
                    fresh.combine_in_place(&tree);
                    tree = fresh;
                }
                if tree.direction.is_none() {
                    tree.direction = Some(direction);
                }
                if tree.base.is_none() {
                    tree.base = Some(base);
                }
                if tree.num_steps == 0 {
                    tree.num_steps = steps as usize;
                }
                prop_assert_eq!(tree.finalize(), got, "{:?} {:?}", direction, base);
            }
        }
    }
}

// ── sessionize ───────────────────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(3000))]

    /// Adjacent segments merged in any order give `1 + (rows whose gap to the
    /// previous row exceeds that row's own threshold)`.
    #[test]
    fn sessionize_segments_combine_to_the_definition(
        rows in vec((0i64..40, prop::sample::select(vec![0i64, 1_000_000, 3_000_000, 9_000_000])), 1..14),
        cuts in vec(any::<bool>(), 14),
        seed in any::<u64>(),
    ) {
        let mut rng = Rng(seed);
        let mut rows: Vec<(i64, i64)> = rows.into_iter().map(|(t, gap)| (t * 500_000, gap)).collect();
        rows.sort_by_key(|row| row.0);
        let want = 1 + (1..rows.len()).filter(|&i| rows[i].0 - rows[i - 1].0 > rows[i].1).count() as i64;

        let feed = |range: std::ops::Range<usize>| {
            let mut state = SessionizeBoundaryState::new();
            for &(ts, threshold) in &rows[range] {
                state.threshold_us = threshold;
                state.update(ts);
            }
            state
        };
        prop_assert_eq!(feed(0..rows.len()).finalize(), want);

        let mut edges = vec![0];
        edges.extend((1..rows.len()).filter(|&i| cuts[i]));
        edges.push(rows.len());
        let mut chunks: Vec<SessionizeBoundaryState> =
            edges.windows(2).map(|w| feed(w[0]..w[1])).collect();
        while chunks.len() > 1 {
            let i = rng.below(chunks.len() - 1);
            let right = chunks.remove(i + 1);
            chunks[i] = chunks[i].combine(&right);
        }
        let mut tree = chunks.pop().unwrap();
        if rng.coin() {
            tree = SessionizeBoundaryState::new().combine(&tree);
        }
        prop_assert_eq!(tree.finalize(), want);
    }
}
