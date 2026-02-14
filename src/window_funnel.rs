//! `window_funnel` — Aggregate function for conversion funnel analysis.
//!
//! Searches for the longest chain of events `cond1 -> cond2 -> ... -> condN`
//! where each subsequent event occurs within `window_size` of the **first**
//! event in the chain. Returns an integer 0..N indicating the max step reached.
//!
//! This matches `ClickHouse` `windowFunnel()` semantics.
//!
//! # SQL Usage
//!
//! ```sql
//! SELECT user_id,
//!   window_funnel(INTERVAL '1 hour', event_time,
//!     event_type = 'page_view',
//!     event_type = 'add_to_cart',
//!     event_type = 'checkout',
//!     event_type = 'purchase'
//!   ) as furthest_step
//! FROM events
//! GROUP BY user_id
//! ```
//!
//! # Modes
//!
//! Modes are combinable via bitwise OR, matching `ClickHouse` semantics where
//! multiple mode strings can be specified simultaneously. Each mode adds an
//! independent constraint on top of the default greedy forward scan.
//!
//! - **Default** (0x00): Steps can be non-consecutive. Multiple entries into
//!   step 1 are tried. The longest chain wins.
//! - **Strict** (0x01): If condition `i` fires, condition `i-1` must not fire
//!   again before condition `i+1`. Prevents backwards movement in the funnel.
//! - **Strict Order** (0x02): Events must satisfy conditions in exact sequential
//!   order with no irrelevant events matching earlier conditions in between.
//! - **Strict Deduplication** (0x04): Events with identical timestamps are
//!   counted only once per condition.
//! - **Strict Increase** (0x08): Requires strictly increasing timestamps between
//!   matched funnel steps. Same-timestamp events cannot advance the funnel.
//! - **Strict Once** (0x10): Each event can advance the funnel by at most one
//!   step, even if it satisfies multiple conditions.
//! - **Allow Reentry** (0x20): If the entry condition fires again mid-chain,
//!   the funnel resets from that new entry point.

use crate::common::event::{sort_events, Event};

/// Funnel matching mode as a bitmask, controlling how strictly the event
/// sequence is enforced.
///
/// Modes are combinable: multiple modes can be active simultaneously. Each
/// mode adds an independent constraint on top of the default greedy scan.
/// This matches `ClickHouse` semantics where mode strings are additive.
///
/// # Bitmask Layout
///
/// ```text
/// Bit 0 (0x01): STRICT
/// Bit 1 (0x02): STRICT_ORDER
/// Bit 2 (0x04): STRICT_DEDUPLICATION
/// Bit 3 (0x08): STRICT_INCREASE
/// Bit 4 (0x10): STRICT_ONCE
/// Bit 5 (0x20): ALLOW_REENTRY
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FunnelMode(u8);

impl FunnelMode {
    /// Default mode: no constraints beyond the basic greedy scan.
    pub const DEFAULT: Self = Self(0);

    /// If condition `i` fires, condition `i-1` must not fire again before
    /// condition `i+1`. Prevents backwards movement.
    pub const STRICT: Self = Self(0x01);

    /// Events must satisfy conditions in exact sequential order. No out-of-order
    /// conditions allowed between matched steps.
    pub const STRICT_ORDER: Self = Self(0x02);

    /// Events with identical timestamps are deduplicated per condition.
    pub const STRICT_DEDUPLICATION: Self = Self(0x04);

    /// Requires strictly increasing timestamps between matched funnel steps.
    /// Same-timestamp events cannot advance the funnel.
    pub const STRICT_INCREASE: Self = Self(0x08);

    /// Each event can advance the funnel by at most one step.
    pub const STRICT_ONCE: Self = Self(0x10);

    /// If the entry condition fires again mid-chain, the funnel resets
    /// from that new entry point.
    pub const ALLOW_REENTRY: Self = Self(0x20);

    /// Creates a `FunnelMode` from a raw bitmask.
    #[must_use]
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits)
    }

    /// Returns the raw bitmask value.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Returns true if the given flag is set.
    #[must_use]
    pub const fn has(self, flag: Self) -> bool {
        self.0 & flag.0 == flag.0
    }

    /// Returns a new mode with the given flag set.
    #[must_use]
    pub const fn with(self, flag: Self) -> Self {
        Self(self.0 | flag.0)
    }

    /// Returns true if this is the default mode (no flags set).
    #[must_use]
    pub const fn is_default(self) -> bool {
        self.0 == 0
    }

    /// Parses a mode string into a single flag bit.
    ///
    /// Returns `None` for unrecognized mode strings.
    #[must_use]
    pub fn parse_mode_str(s: &str) -> Option<Self> {
        match s {
            "strict" => Some(Self::STRICT),
            "strict_order" => Some(Self::STRICT_ORDER),
            "strict_deduplication" => Some(Self::STRICT_DEDUPLICATION),
            "strict_increase" => Some(Self::STRICT_INCREASE),
            "strict_once" => Some(Self::STRICT_ONCE),
            "allow_reentry" => Some(Self::ALLOW_REENTRY),
            _ => None,
        }
    }

    /// Parses a comma-separated mode string into a combined `FunnelMode`.
    ///
    /// Accepts strings like `"strict_increase, strict_once"`. Whitespace around
    /// mode names is trimmed. Empty strings produce `DEFAULT` (no flags).
    ///
    /// Returns `Err` with the unrecognized mode name if any token is invalid.
    pub fn parse_modes(s: &str) -> Result<Self, String> {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Ok(Self::DEFAULT);
        }
        let mut result = Self::DEFAULT;
        for token in trimmed.split(',') {
            let mode_name = token.trim();
            if mode_name.is_empty() {
                continue;
            }
            match Self::parse_mode_str(mode_name) {
                Some(flag) => result = result.with(flag),
                None => return Err(mode_name.to_string()),
            }
        }
        Ok(result)
    }
}

impl std::fmt::Display for FunnelMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_default() {
            return write!(f, "default");
        }
        let mut first = true;
        let flags = [
            (Self::STRICT, "strict"),
            (Self::STRICT_ORDER, "strict_order"),
            (Self::STRICT_DEDUPLICATION, "strict_deduplication"),
            (Self::STRICT_INCREASE, "strict_increase"),
            (Self::STRICT_ONCE, "strict_once"),
            (Self::ALLOW_REENTRY, "allow_reentry"),
        ];
        for (flag, name) in flags {
            if self.has(flag) {
                if !first {
                    write!(f, "+")?;
                }
                write!(f, "{name}")?;
                first = false;
            }
        }
        Ok(())
    }
}

/// State for the `window_funnel` aggregate function.
///
/// Collects timestamped events during `update`, then processes them in `finalize`
/// using a greedy forward scan algorithm.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct WindowFunnelState {
    /// Collected events (timestamp + conditions bitmask). Sorted in finalize.
    pub events: Vec<Event>,
    /// Window size in microseconds.
    pub window_size_us: i64,
    /// Number of funnel steps (conditions).
    pub num_conditions: usize,
    /// Funnel mode (combinable bitmask).
    pub mode: FunnelMode,
}

impl WindowFunnelState {
    /// Creates a new empty state.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            events: Vec::new(),
            window_size_us: 0,
            num_conditions: 0,
            mode: FunnelMode::DEFAULT,
        }
    }

    /// Adds an event to the state.
    ///
    /// Only events where at least one condition is true are stored.
    /// Events where all conditions are false cannot participate in any funnel
    /// and are filtered to reduce memory usage.
    ///
    /// `num_conditions` is the total number of funnel steps. This is passed
    /// explicitly because the `Event` bitmask does not carry length information.
    pub fn update(&mut self, event: Event, num_conditions: usize) {
        self.num_conditions = num_conditions;
        if event.has_any_condition() {
            self.events.push(event);
        }
    }

    /// Combines two states by concatenating their event lists, returning a new state.
    ///
    /// Events do not need to be in sorted order during combine because
    /// `finalize()` sorts them before scanning.
    #[must_use]
    pub fn combine(&self, other: &Self) -> Self {
        let mut events = Vec::with_capacity(self.events.len() + other.events.len());
        events.extend_from_slice(&self.events);
        events.extend_from_slice(&other.events);
        // Propagate window_size and mode from whichever state has them set,
        // matching combine_in_place behavior for DuckDB's zero-initialized targets.
        let window_size_us = if self.window_size_us != 0 {
            self.window_size_us
        } else {
            other.window_size_us
        };
        let mode = if self.mode.is_default() {
            other.mode
        } else {
            self.mode
        };
        Self {
            events,
            window_size_us,
            num_conditions: self.num_conditions.max(other.num_conditions),
            mode,
        }
    }

    /// Combines another state into `self` in-place by appending its events.
    ///
    /// This is the preferred combine method for sequential (left-fold) chains.
    /// By extending `self.events` in-place, Vec's doubling growth strategy
    /// provides O(N) amortized total copies for a chain of N single-event
    /// combines, compared to O(N²) when allocating a new Vec per combine.
    pub fn combine_in_place(&mut self, other: &Self) {
        self.events.extend_from_slice(&other.events);
        self.num_conditions = self.num_conditions.max(other.num_conditions);
        // Propagate window_size and mode from whichever state has them set.
        // DuckDB's segment tree creates fresh (zero-initialized) target states
        // and combines source states into them, so these fields must be propagated.
        if self.window_size_us == 0 && other.window_size_us != 0 {
            self.window_size_us = other.window_size_us;
        }
        if self.mode.is_default() && !other.mode.is_default() {
            self.mode = other.mode;
        }
    }

    /// Computes the maximum funnel step reached.
    ///
    /// Algorithm:
    /// 1. Sort events by timestamp
    /// 2. For each event matching condition 0 (funnel entry):
    ///    a. Greedily scan forward within the window
    ///    b. Try to match conditions 1, 2, ..., N in order
    ///    c. Track the maximum step reached
    /// 3. Return the global maximum across all entry points
    ///
    /// Time complexity: O(n * k) where n = events, k = conditions.
    /// In practice, much faster due to early termination.
    #[must_use]
    pub fn finalize(&mut self) -> i64 {
        if self.events.is_empty() || self.num_conditions == 0 {
            return 0;
        }

        sort_events(&mut self.events);
        let mut max_step: i64 = 0;

        for i in 0..self.events.len() {
            // Only start from events matching condition 0
            if !self.events[i].condition(0) {
                continue;
            }

            let entry_ts = self.events[i].timestamp_us;
            let step = self.scan_funnel(i, entry_ts);
            max_step = max_step.max(step);

            // Early termination: can't do better than matching all conditions
            if max_step == self.num_conditions as i64 {
                break;
            }
        }

        max_step
    }

    /// Scans forward from an entry point trying to match funnel steps.
    ///
    /// Each active mode flag adds an independent constraint check. Constraints
    /// are evaluated in order: `STRICT`, `STRICT_ORDER`, `STRICT_DEDUPLICATION`,
    /// `STRICT_INCREASE`. If any constraint fails, the event is handled per
    /// that constraint's semantics (break, return, continue, or skip).
    fn scan_funnel(&self, start_idx: usize, entry_ts: i64) -> i64 {
        let mut current_step: usize = 1; // Already matched step 0
        let mut prev_matched_ts = entry_ts;

        for j in (start_idx + 1)..self.events.len() {
            let event = &self.events[j];

            // Check window: event must be within window_size of the ENTRY event
            if event.timestamp_us - entry_ts > self.window_size_us {
                break;
            }

            // --- Mode: ALLOW_REENTRY ---
            // If entry condition fires again mid-chain, reset the funnel
            if self.mode.has(FunnelMode::ALLOW_REENTRY) && current_step > 1 && event.condition(0) {
                current_step = 1;
                prev_matched_ts = event.timestamp_us;
                // Continue scanning from this new entry; don't also try to
                // match the next step on this same event
                continue;
            }

            // --- Mode: STRICT ---
            if self.mode.has(FunnelMode::STRICT)
                && current_step > 0
                && event.condition(current_step - 1)
                && !event.condition(current_step)
            {
                break;
            }

            // --- Mode: STRICT_ORDER ---
            if self.mode.has(FunnelMode::STRICT_ORDER) {
                let mut earlier_fired = false;
                for k in 0..current_step {
                    if event.condition(k) {
                        earlier_fired = true;
                        break;
                    }
                }
                if earlier_fired {
                    return current_step as i64;
                }
            }

            // --- Mode: STRICT_DEDUPLICATION ---
            if self.mode.has(FunnelMode::STRICT_DEDUPLICATION)
                && event.timestamp_us == prev_matched_ts
                && event.condition(current_step)
            {
                continue;
            }

            // --- Mode: STRICT_INCREASE ---
            if self.mode.has(FunnelMode::STRICT_INCREASE)
                && event.condition(current_step)
                && event.timestamp_us <= prev_matched_ts
            {
                continue;
            }

            // Check if this event matches the next expected condition.
            // In default mode, a single event can advance multiple steps
            // (e.g., an event satisfying both cond2 and cond3 advances 2 steps).
            // STRICT_ONCE limits this to at most 1 step per event.
            while event.condition(current_step) {
                current_step += 1;
                prev_matched_ts = event.timestamp_us;

                // Matched all conditions
                if current_step >= self.num_conditions {
                    return self.num_conditions as i64;
                }

                // --- Mode: STRICT_ONCE ---
                // Each event advances at most one step
                if self.mode.has(FunnelMode::STRICT_ONCE) {
                    break;
                }
            }
        }

        current_step as i64
    }
}

impl Default for WindowFunnelState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_event(ts: i64, conds: &[bool]) -> Event {
        Event::from_bools(ts, conds)
    }

    #[test]
    fn test_empty_state() {
        let mut state = WindowFunnelState::new();
        assert_eq!(state.finalize(), 0);
    }

    #[test]
    fn test_complete_funnel() {
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000; // 1 hour
        state.update(make_event(0, &[true, false, false]), 3); // step 0
        state.update(make_event(1_000_000, &[false, true, false]), 3); // step 1
        state.update(make_event(2_000_000, &[false, false, true]), 3); // step 2
        assert_eq!(state.finalize(), 3);
    }

    #[test]
    fn test_partial_funnel() {
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.update(make_event(0, &[true, false, false]), 3); // step 0
        state.update(make_event(1_000_000, &[false, true, false]), 3); // step 1
                                                                       // No step 2
        assert_eq!(state.finalize(), 2);
    }

    #[test]
    fn test_window_expiry() {
        let mut state = WindowFunnelState::new();
        state.window_size_us = 60_000_000; // 1 minute
        state.update(make_event(0, &[true, false, false]), 3);
        state.update(make_event(30_000_000, &[false, true, false]), 3); // 30s, within window
        state.update(make_event(120_000_000, &[false, false, true]), 3); // 120s, outside window
        assert_eq!(state.finalize(), 2); // Only reached step 1
    }

    #[test]
    fn test_no_entry_point() {
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.update(make_event(0, &[false, true, false]), 3); // No step 0
        state.update(make_event(1_000_000, &[false, false, true]), 3);
        assert_eq!(state.finalize(), 0);
    }

    #[test]
    fn test_multiple_entries_best_wins() {
        let mut state = WindowFunnelState::new();
        state.window_size_us = 60_000_000; // 1 minute
                                           // First entry: step 0, then window expires before step 1
        state.update(make_event(0, &[true, false, false]), 3);
        state.update(make_event(120_000_000, &[false, true, false]), 3); // too late
                                                                         // Second entry: step 0, step 1 within window
        state.update(make_event(200_000_000, &[true, false, false]), 3);
        state.update(make_event(230_000_000, &[false, true, false]), 3); // 30s, ok
        assert_eq!(state.finalize(), 2);
    }

    #[test]
    fn test_single_step_funnel() {
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.update(make_event(0, &[true]), 1);
        assert_eq!(state.finalize(), 1);
    }

    #[test]
    fn test_no_matching_events() {
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.num_conditions = 3;
        state.update(make_event(0, &[false, false, false]), 3);
        assert_eq!(state.finalize(), 0);
    }

    #[test]
    fn test_all_conditions_same_row() {
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        // All conditions true in a single event - step 0 matches,
        // but steps 1+ need SUBSEQUENT events
        state.update(make_event(0, &[true, true, true]), 3);
        // Only step 0 is reached since there are no subsequent events
        assert_eq!(state.finalize(), 1);
    }

    #[test]
    fn test_strict_mode() {
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.mode = FunnelMode::STRICT;
        state.update(make_event(0, &[true, false, false]), 3); // step 0
        state.update(make_event(1_000, &[true, false, false]), 3); // step 0 again!
        state.update(make_event(2_000, &[false, true, false]), 3); // step 1
                                                                   // In strict mode, step 0 fired again before step 1,
                                                                   // so the first entry's chain breaks at step 0.
                                                                   // But the second entry at t=1000 can match step 1 at t=2000.
        assert_eq!(state.finalize(), 2);
    }

    #[test]
    fn test_combine() {
        let mut a = WindowFunnelState::new();
        a.window_size_us = 3_600_000_000;
        a.update(make_event(0, &[true, false]), 2);

        let mut b = WindowFunnelState::new();
        b.window_size_us = 3_600_000_000;
        b.update(make_event(1_000_000, &[false, true]), 2);

        let mut combined = a.combine(&b);
        assert_eq!(combined.finalize(), 2);
    }

    #[test]
    fn test_events_unsorted_input() {
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        // Insert out of order — finalize should sort
        state.update(make_event(2_000_000, &[false, false, true]), 3);
        state.update(make_event(0, &[true, false, false]), 3);
        state.update(make_event(1_000_000, &[false, true, false]), 3);
        assert_eq!(state.finalize(), 3);
    }

    #[test]
    fn test_large_funnel() {
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000; // 1 hour
        let n = 8; // Test with 8 conditions
        for i in 0..n {
            let mut conds = vec![false; n];
            conds[i] = true;
            state.update(make_event((i as i64) * 1_000_000, &conds), n);
        }
        assert_eq!(state.finalize(), n as i64);
    }

    // --- StrictOrder mode tests ---

    #[test]
    fn test_strict_order_basic_success() {
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.mode = FunnelMode::STRICT_ORDER;
        state.update(make_event(0, &[true, false, false]), 3);
        state.update(make_event(1_000, &[false, true, false]), 3);
        state.update(make_event(2_000, &[false, false, true]), 3);
        assert_eq!(state.finalize(), 3);
    }

    #[test]
    fn test_strict_order_earlier_condition_breaks_chain() {
        // In StrictOrder, if any earlier condition fires between matched steps,
        // the chain breaks.
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.mode = FunnelMode::STRICT_ORDER;
        state.update(make_event(0, &[true, false, false]), 3);
        state.update(make_event(1_000, &[true, false, false]), 3); // cond[0] fires again
        state.update(make_event(2_000, &[false, true, false]), 3);
        // First entry at t=0: scanning at t=1000, cond[0] fires (earlier than current_step=1)
        // -> returns step 1. Second entry at t=1000: scanning at t=2000, cond[1] matches -> step 2.
        assert_eq!(state.finalize(), 2);
    }

    #[test]
    fn test_strict_order_irrelevant_events_dont_break() {
        // Events that don't match any earlier condition don't break the chain
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.mode = FunnelMode::STRICT_ORDER;
        state.update(make_event(0, &[true, false, false]), 3);
        // Event with no conditions set is filtered out by update()
        state.update(make_event(1_000, &[false, false, false]), 3);
        state.update(make_event(2_000, &[false, true, false]), 3);
        assert_eq!(state.finalize(), 2);
    }

    #[test]
    fn test_strict_order_empty() {
        let mut state = WindowFunnelState::new();
        state.mode = FunnelMode::STRICT_ORDER;
        assert_eq!(state.finalize(), 0);
    }

    // --- StrictDeduplication mode tests ---

    #[test]
    fn test_strict_dedup_basic_success() {
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.mode = FunnelMode::STRICT_DEDUPLICATION;
        state.update(make_event(0, &[true, false, false]), 3);
        state.update(make_event(1_000, &[false, true, false]), 3);
        state.update(make_event(2_000, &[false, false, true]), 3);
        assert_eq!(state.finalize(), 3);
    }

    #[test]
    fn test_strict_dedup_skips_same_timestamp() {
        // Events with identical timestamps for the next condition are skipped
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.mode = FunnelMode::STRICT_DEDUPLICATION;
        state.update(make_event(0, &[true, false]), 2); // step 0, prev_ts = 0
        state.update(make_event(0, &[false, true]), 2); // same ts=0, skipped
        assert_eq!(state.finalize(), 1);
    }

    #[test]
    fn test_strict_dedup_different_timestamps_ok() {
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.mode = FunnelMode::STRICT_DEDUPLICATION;
        state.update(make_event(0, &[true, false]), 2);
        state.update(make_event(1, &[false, true]), 2); // different ts
        assert_eq!(state.finalize(), 2);
    }

    #[test]
    fn test_strict_dedup_skips_then_matches_later() {
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.mode = FunnelMode::STRICT_DEDUPLICATION;
        state.update(make_event(0, &[true, false, false]), 3);
        state.update(make_event(0, &[false, true, false]), 3); // same ts, skipped
        state.update(make_event(1_000, &[false, true, false]), 3); // different ts, matches
        state.update(make_event(2_000, &[false, false, true]), 3);
        assert_eq!(state.finalize(), 3);
    }

    #[test]
    fn test_strict_dedup_empty() {
        let mut state = WindowFunnelState::new();
        state.mode = FunnelMode::STRICT_DEDUPLICATION;
        assert_eq!(state.finalize(), 0);
    }

    // --- Additional edge cases ---

    #[test]
    fn test_default_mode_is_default() {
        let state = WindowFunnelState::new();
        assert_eq!(state.mode, FunnelMode::DEFAULT);
    }

    #[test]
    fn test_zero_window_size_same_timestamp() {
        let mut state = WindowFunnelState::new();
        state.window_size_us = 0;
        state.update(make_event(0, &[true, false]), 2);
        state.update(make_event(0, &[false, true]), 2); // 0 - 0 = 0, not > 0, within window
        assert_eq!(state.finalize(), 2);
    }

    #[test]
    fn test_zero_window_any_gap_breaks() {
        let mut state = WindowFunnelState::new();
        state.window_size_us = 0;
        state.update(make_event(0, &[true, false]), 2);
        state.update(make_event(1, &[false, true]), 2); // 1us > 0
        assert_eq!(state.finalize(), 1);
    }

    #[test]
    fn test_combine_empty_states() {
        let a = WindowFunnelState::new();
        let b = WindowFunnelState::new();
        let mut combined = a.combine(&b);
        assert_eq!(combined.finalize(), 0);
    }

    #[test]
    fn test_combine_preserves_mode() {
        let mut a = WindowFunnelState::new();
        a.mode = FunnelMode::STRICT;
        a.window_size_us = 3_600_000_000;

        let b = WindowFunnelState::new();
        let combined = a.combine(&b);
        assert_eq!(combined.mode, FunnelMode::STRICT);
    }

    #[test]
    fn test_strict_mode_allows_forward_movement() {
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.mode = FunnelMode::STRICT;
        state.update(make_event(0, &[true, false, false]), 3);
        state.update(make_event(1_000, &[false, true, false]), 3);
        state.update(make_event(2_000, &[false, false, true]), 3);
        assert_eq!(state.finalize(), 3);
    }

    #[test]
    fn test_strict_mode_backward_step_breaks() {
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.mode = FunnelMode::STRICT;
        state.update(make_event(0, &[true, false, false]), 3);
        state.update(make_event(1_000, &[false, true, false]), 3); // step 1
        state.update(make_event(2_000, &[false, true, false]), 3); // step 1 fires again
        state.update(make_event(3_000, &[false, false, true]), 3); // step 2
                                                                   // At t=2000: cond[1] (current_step-1) fires but cond[2] doesn't -> break
        assert_eq!(state.finalize(), 2);
    }

    #[test]
    fn test_duplicate_timestamps_default_mode() {
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        // Multiple events at the same timestamp in default mode
        state.update(make_event(100, &[true, false]), 2);
        state.update(make_event(100, &[false, true]), 2);
        assert_eq!(state.finalize(), 2);
    }

    // --- Mutation testing coverage: combine_in_place ---

    #[test]
    fn test_combine_in_place_basic() {
        let mut a = WindowFunnelState::new();
        a.window_size_us = 3_600_000_000;
        a.update(make_event(0, &[true, false]), 2);

        let mut b = WindowFunnelState::new();
        b.window_size_us = 3_600_000_000;
        b.update(make_event(1_000_000, &[false, true]), 2);

        a.combine_in_place(&b);
        assert_eq!(a.events.len(), 2);
        assert_eq!(a.finalize(), 2);
    }

    #[test]
    fn test_combine_in_place_empty_other() {
        let mut a = WindowFunnelState::new();
        a.window_size_us = 3_600_000_000;
        a.update(make_event(0, &[true, false]), 2);

        let b = WindowFunnelState::new();
        a.combine_in_place(&b);
        assert_eq!(a.events.len(), 1);
    }

    // --- Mutation testing coverage: finalize edge cases ---

    #[test]
    fn test_finalize_events_but_zero_conditions() {
        // Covers: replace || with && in finalize
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        // Manually push an event without going through update (which sets num_conditions)
        state.events.push(Event::new(0, 1));
        state.num_conditions = 0;
        assert_eq!(state.finalize(), 0);
    }

    #[test]
    fn test_finalize_no_events_with_conditions() {
        // Covers: replace || with && in finalize
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.num_conditions = 3;
        assert_eq!(state.finalize(), 0);
    }

    // --- Mutation testing coverage: strict mode current_step > 0 ---

    #[test]
    fn test_strict_mode_refire_breaks_chain() {
        // Covers: strict mode condition check with current_step > 0
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.mode = FunnelMode::STRICT;
        // Entry, then cond[0] refires without cond[1], then cond[1]
        state.update(make_event(0, &[true, false, false]), 3);
        state.update(make_event(1_000, &[true, false, false]), 3); // cond[0] refires → break
        state.update(make_event(2_000, &[false, true, false]), 3); // cond[1]
                                                                   // From entry t=0: at t=1000 cond[0] fires without cond[1] → break → step 1
                                                                   // From entry t=1000: at t=2000 cond[1] fires → step 2
        assert_eq!(state.finalize(), 2);
    }

    // --- Session 3: Mutation-killing boundary tests ---

    #[test]
    fn test_window_boundary_exactly_at_limit_included() {
        // Kills mutant: replace `>` with `>=` in scan_funnel window check.
        // An event at exactly window_size_us should be INCLUDED (not > boundary).
        let mut state = WindowFunnelState::new();
        state.window_size_us = 1000;
        state.update(make_event(0, &[true, false]), 2);
        state.update(make_event(1000, &[false, true]), 2); // exactly at boundary
                                                           // 1000 - 0 = 1000, which is NOT > 1000, so included
        assert_eq!(state.finalize(), 2);
    }

    #[test]
    fn test_window_boundary_one_past_excluded() {
        // Complement of above: one microsecond past the boundary is excluded.
        let mut state = WindowFunnelState::new();
        state.window_size_us = 1000;
        state.update(make_event(0, &[true, false]), 2);
        state.update(make_event(1001, &[false, true]), 2); // one past boundary
                                                           // 1001 - 0 = 1001 > 1000, so excluded
        assert_eq!(state.finalize(), 1);
    }

    #[test]
    fn test_scan_funnel_returns_at_exact_num_conditions() {
        // Kills mutant: replace `>=` with `>` in current_step >= num_conditions check.
        // When current_step reaches exactly num_conditions, should return immediately.
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.update(make_event(0, &[true, false]), 2);
        state.update(make_event(1_000, &[false, true]), 2);
        // current_step becomes 2, num_conditions is 2 → exactly equal → return
        assert_eq!(state.finalize(), 2);
    }

    #[test]
    fn test_strict_dedup_timestamp_equality_not_inequality() {
        // Kills mutant: replace `==` with `!=` in StrictDeduplication timestamp check.
        // Same timestamp should be skipped; different timestamp should pass.
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.mode = FunnelMode::STRICT_DEDUPLICATION;
        state.update(make_event(100, &[true, false]), 2); // step 0, prev_ts=100
        state.update(make_event(100, &[false, true]), 2); // same ts → SKIP
        state.update(make_event(101, &[false, true]), 2); // different ts → match
        assert_eq!(state.finalize(), 2);
    }

    #[test]
    fn test_combine_in_place_num_conditions_max() {
        // Kills mutant: remove .max() in combine_in_place num_conditions update.
        let mut a = WindowFunnelState::new();
        a.window_size_us = 3_600_000_000;
        a.update(make_event(0, &[true, false, false]), 3);

        let mut b = WindowFunnelState::new();
        b.window_size_us = 3_600_000_000;
        b.update(make_event(1_000, &[false, true, false, false, false]), 5);

        a.combine_in_place(&b);
        // num_conditions should be max(3, 5) = 5, not 3
        assert_eq!(a.num_conditions, 5);
    }

    #[test]
    fn test_finalize_zero_conditions_returns_zero() {
        // Kills mutant: replace `||` with `&&` in finalize's early return.
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.events.push(Event::new(0, 1)); // has events
        state.num_conditions = 0; // but zero conditions
        assert_eq!(state.finalize(), 0);
    }

    #[test]
    fn test_max_step_uses_max_not_assignment() {
        // Kills mutant: replace max_step.max(step) with max_step = step.
        // First entry reaches step 2, second entry reaches step 3.
        // max_step should be 3, not the last value.
        let mut state = WindowFunnelState::new();
        state.window_size_us = 60_000_000; // 1 minute
                                           // First entry: reaches step 2 then window expires
        state.update(make_event(0, &[true, false, false]), 3);
        state.update(make_event(30_000_000, &[false, true, false]), 3);
        state.update(make_event(120_000_000, &[false, false, true]), 3); // outside window
                                                                         // Second entry: reaches step 3
        state.update(make_event(200_000_000, &[true, false, false]), 3);
        state.update(make_event(210_000_000, &[false, true, false]), 3);
        state.update(make_event(220_000_000, &[false, false, true]), 3);
        assert_eq!(state.finalize(), 3);
    }

    // --- FunnelMode bitmask tests ---

    #[test]
    fn test_funnel_mode_default_is_zero() {
        assert_eq!(FunnelMode::DEFAULT.bits(), 0);
        assert!(FunnelMode::DEFAULT.is_default());
    }

    #[test]
    fn test_funnel_mode_individual_flags() {
        assert_eq!(FunnelMode::STRICT.bits(), 0x01);
        assert_eq!(FunnelMode::STRICT_ORDER.bits(), 0x02);
        assert_eq!(FunnelMode::STRICT_DEDUPLICATION.bits(), 0x04);
        assert_eq!(FunnelMode::STRICT_INCREASE.bits(), 0x08);
        assert_eq!(FunnelMode::STRICT_ONCE.bits(), 0x10);
        assert_eq!(FunnelMode::ALLOW_REENTRY.bits(), 0x20);
    }

    #[test]
    fn test_funnel_mode_combinable() {
        let mode = FunnelMode::STRICT.with(FunnelMode::STRICT_INCREASE);
        assert!(mode.has(FunnelMode::STRICT));
        assert!(mode.has(FunnelMode::STRICT_INCREASE));
        assert!(!mode.has(FunnelMode::STRICT_ORDER));
        assert!(!mode.is_default());
    }

    #[test]
    fn test_funnel_mode_has_self() {
        // Each flag should contain itself
        let flags = [
            FunnelMode::STRICT,
            FunnelMode::STRICT_ORDER,
            FunnelMode::STRICT_DEDUPLICATION,
            FunnelMode::STRICT_INCREASE,
            FunnelMode::STRICT_ONCE,
            FunnelMode::ALLOW_REENTRY,
        ];
        for flag in flags {
            assert!(flag.has(flag));
        }
    }

    #[test]
    fn test_funnel_mode_from_bits_roundtrip() {
        let mode = FunnelMode::from_bits(0x13);
        assert!(mode.has(FunnelMode::STRICT));
        assert!(mode.has(FunnelMode::STRICT_ORDER));
        assert!(mode.has(FunnelMode::STRICT_ONCE));
        assert!(!mode.has(FunnelMode::STRICT_DEDUPLICATION));
        assert_eq!(mode.bits(), 0x13);
    }

    #[test]
    fn test_funnel_mode_parse_mode_str() {
        assert_eq!(
            FunnelMode::parse_mode_str("strict"),
            Some(FunnelMode::STRICT)
        );
        assert_eq!(
            FunnelMode::parse_mode_str("strict_order"),
            Some(FunnelMode::STRICT_ORDER)
        );
        assert_eq!(
            FunnelMode::parse_mode_str("strict_deduplication"),
            Some(FunnelMode::STRICT_DEDUPLICATION)
        );
        assert_eq!(
            FunnelMode::parse_mode_str("strict_increase"),
            Some(FunnelMode::STRICT_INCREASE)
        );
        assert_eq!(
            FunnelMode::parse_mode_str("strict_once"),
            Some(FunnelMode::STRICT_ONCE)
        );
        assert_eq!(
            FunnelMode::parse_mode_str("allow_reentry"),
            Some(FunnelMode::ALLOW_REENTRY)
        );
        assert_eq!(FunnelMode::parse_mode_str("unknown"), None);
        assert_eq!(FunnelMode::parse_mode_str(""), None);
    }

    #[test]
    fn test_funnel_mode_display() {
        assert_eq!(FunnelMode::DEFAULT.to_string(), "default");
        assert_eq!(FunnelMode::STRICT.to_string(), "strict");
        assert_eq!(
            FunnelMode::STRICT
                .with(FunnelMode::STRICT_INCREASE)
                .to_string(),
            "strict+strict_increase"
        );
    }

    #[test]
    fn test_funnel_mode_with_is_commutative() {
        let a = FunnelMode::STRICT.with(FunnelMode::STRICT_ORDER);
        let b = FunnelMode::STRICT_ORDER.with(FunnelMode::STRICT);
        assert_eq!(a, b);
    }

    // --- parse_modes tests ---

    #[test]
    fn test_parse_modes_empty_string() {
        assert_eq!(FunnelMode::parse_modes("").unwrap(), FunnelMode::DEFAULT);
    }

    #[test]
    fn test_parse_modes_whitespace_only() {
        assert_eq!(FunnelMode::parse_modes("  ").unwrap(), FunnelMode::DEFAULT);
    }

    #[test]
    fn test_parse_modes_single() {
        assert_eq!(
            FunnelMode::parse_modes("strict").unwrap(),
            FunnelMode::STRICT
        );
    }

    #[test]
    fn test_parse_modes_two_comma_separated() {
        let mode = FunnelMode::parse_modes("strict_increase, strict_once").unwrap();
        assert!(mode.has(FunnelMode::STRICT_INCREASE));
        assert!(mode.has(FunnelMode::STRICT_ONCE));
        assert!(!mode.has(FunnelMode::STRICT));
    }

    #[test]
    fn test_parse_modes_no_whitespace() {
        let mode = FunnelMode::parse_modes("strict,strict_order").unwrap();
        assert!(mode.has(FunnelMode::STRICT));
        assert!(mode.has(FunnelMode::STRICT_ORDER));
    }

    #[test]
    fn test_parse_modes_extra_whitespace() {
        let mode = FunnelMode::parse_modes("  strict_increase ,  strict_once  ").unwrap();
        assert!(mode.has(FunnelMode::STRICT_INCREASE));
        assert!(mode.has(FunnelMode::STRICT_ONCE));
    }

    #[test]
    fn test_parse_modes_all_modes() {
        let mode = FunnelMode::parse_modes(
            "strict, strict_order, strict_deduplication, strict_increase, strict_once, allow_reentry",
        )
        .unwrap();
        assert!(mode.has(FunnelMode::STRICT));
        assert!(mode.has(FunnelMode::STRICT_ORDER));
        assert!(mode.has(FunnelMode::STRICT_DEDUPLICATION));
        assert!(mode.has(FunnelMode::STRICT_INCREASE));
        assert!(mode.has(FunnelMode::STRICT_ONCE));
        assert!(mode.has(FunnelMode::ALLOW_REENTRY));
    }

    #[test]
    fn test_parse_modes_invalid_returns_err() {
        let err = FunnelMode::parse_modes("strict, invalid_mode").unwrap_err();
        assert_eq!(err, "invalid_mode");
    }

    #[test]
    fn test_parse_modes_trailing_comma() {
        // Trailing comma produces an empty token which is skipped
        let mode = FunnelMode::parse_modes("strict,").unwrap();
        assert_eq!(mode, FunnelMode::STRICT);
    }

    #[test]
    fn test_parse_modes_duplicate_mode() {
        // Duplicate mode is idempotent (OR of same bit)
        let mode = FunnelMode::parse_modes("strict, strict").unwrap();
        assert_eq!(mode, FunnelMode::STRICT);
    }

    // --- strict_increase mode tests ---

    #[test]
    fn test_strict_increase_same_timestamp_stops_funnel() {
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.mode = FunnelMode::STRICT_INCREASE;
        state.update(make_event(0, &[true, false, false]), 3);
        state.update(make_event(0, &[false, true, false]), 3); // same ts → skipped
        assert_eq!(state.finalize(), 1);
    }

    #[test]
    fn test_strict_increase_increasing_timestamps_ok() {
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.mode = FunnelMode::STRICT_INCREASE;
        state.update(make_event(0, &[true, false, false]), 3);
        state.update(make_event(1, &[false, true, false]), 3); // 1 > 0
        state.update(make_event(2, &[false, false, true]), 3); // 2 > 1
        assert_eq!(state.finalize(), 3);
    }

    #[test]
    fn test_strict_increase_mixed_same_and_increasing() {
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.mode = FunnelMode::STRICT_INCREASE;
        state.update(make_event(0, &[true, false, false]), 3);
        state.update(make_event(1000, &[false, true, false]), 3); // increasing, matches
        state.update(make_event(1000, &[false, false, true]), 3); // same ts → skipped
        assert_eq!(state.finalize(), 2);
    }

    #[test]
    fn test_strict_increase_skips_then_matches_later() {
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.mode = FunnelMode::STRICT_INCREASE;
        state.update(make_event(0, &[true, false, false]), 3);
        state.update(make_event(0, &[false, true, false]), 3); // same ts → skipped
        state.update(make_event(1000, &[false, true, false]), 3); // increasing → matches
        state.update(make_event(2000, &[false, false, true]), 3);
        assert_eq!(state.finalize(), 3);
    }

    #[test]
    fn test_strict_increase_all_same_timestamp_entry_only() {
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.mode = FunnelMode::STRICT_INCREASE;
        state.update(make_event(0, &[true, false]), 2);
        state.update(make_event(0, &[false, true]), 2);
        assert_eq!(state.finalize(), 1);
    }

    #[test]
    fn test_strict_increase_by_one_microsecond() {
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.mode = FunnelMode::STRICT_INCREASE;
        state.update(make_event(0, &[true, false]), 2);
        state.update(make_event(1, &[false, true]), 2); // 1us increase
        assert_eq!(state.finalize(), 2);
    }

    #[test]
    fn test_strict_increase_empty() {
        let mut state = WindowFunnelState::new();
        state.mode = FunnelMode::STRICT_INCREASE;
        assert_eq!(state.finalize(), 0);
    }

    // --- strict_once mode tests ---

    #[test]
    fn test_strict_once_multi_condition_event_advances_only_one() {
        // Without strict_once, an event matching cond1 AND cond2 can advance 2 steps.
        // With strict_once, it advances only 1 step per event.
        let mut state_default = WindowFunnelState::new();
        state_default.window_size_us = 3_600_000_000;
        state_default.update(make_event(0, &[true, false, false]), 3);
        state_default.update(make_event(1000, &[false, true, true]), 3); // cond1+cond2
                                                                         // Default: matches step 1 (cond[1]), then step 2 (cond[2]) on same event
        let default_result = state_default.finalize();

        let mut state_once = WindowFunnelState::new();
        state_once.window_size_us = 3_600_000_000;
        state_once.mode = FunnelMode::STRICT_ONCE;
        state_once.update(make_event(0, &[true, false, false]), 3);
        state_once.update(make_event(1000, &[false, true, true]), 3); // cond1+cond2
        let once_result = state_once.finalize();

        // strict_once should prevent advancing more than 1 step per event
        assert!(once_result <= default_result);
        assert_eq!(once_result, 2); // step 0 (entry) + step 1 (from event at 1000)
    }

    #[test]
    fn test_strict_once_sequential_single_conditions() {
        // When each event satisfies only one condition, strict_once has no effect
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.mode = FunnelMode::STRICT_ONCE;
        state.update(make_event(0, &[true, false, false]), 3);
        state.update(make_event(1000, &[false, true, false]), 3);
        state.update(make_event(2000, &[false, false, true]), 3);
        assert_eq!(state.finalize(), 3);
    }

    #[test]
    fn test_strict_once_triple_condition_event() {
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.mode = FunnelMode::STRICT_ONCE;
        state.update(make_event(0, &[true, true, true, true]), 4); // all conditions on entry
        state.update(make_event(1000, &[false, true, true, true]), 4);
        // Entry matches step 0. Next event: strict_once means only step 1 advances.
        // Need another event for step 2 and 3.
        state.update(make_event(2000, &[false, false, true, true]), 4);
        state.update(make_event(3000, &[false, false, false, true]), 4);
        assert_eq!(state.finalize(), 4);
    }

    #[test]
    fn test_strict_once_empty() {
        let mut state = WindowFunnelState::new();
        state.mode = FunnelMode::STRICT_ONCE;
        assert_eq!(state.finalize(), 0);
    }

    // --- allow_reentry mode tests ---

    #[test]
    fn test_allow_reentry_longer_chain_from_reentry() {
        // Reentry should find a longer chain
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.mode = FunnelMode::ALLOW_REENTRY;
        state.update(make_event(0, &[true, false, false]), 3); // entry 1
        state.update(make_event(1000, &[false, true, false]), 3); // step 1
                                                                  // Entry fires again: reset chain
        state.update(make_event(2000, &[true, false, false]), 3); // reentry
        state.update(make_event(3000, &[false, true, false]), 3); // step 1 (from reentry)
        state.update(make_event(4000, &[false, false, true]), 3); // step 2 (from reentry)
        assert_eq!(state.finalize(), 3);
    }

    #[test]
    fn test_allow_reentry_no_second_entry() {
        // Without reentry trigger, behaves like default
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.mode = FunnelMode::ALLOW_REENTRY;
        state.update(make_event(0, &[true, false, false]), 3);
        state.update(make_event(1000, &[false, true, false]), 3);
        state.update(make_event(2000, &[false, false, true]), 3);
        assert_eq!(state.finalize(), 3);
    }

    #[test]
    fn test_allow_reentry_multiple_reentries() {
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.mode = FunnelMode::ALLOW_REENTRY;
        state.update(make_event(0, &[true, false]), 2); // entry 1
        state.update(make_event(1000, &[true, false]), 2); // reentry 1
        state.update(make_event(2000, &[true, false]), 2); // reentry 2
        state.update(make_event(3000, &[false, true]), 2); // step 1
        assert_eq!(state.finalize(), 2);
    }

    #[test]
    fn test_allow_reentry_resets_from_correct_point() {
        let mut state = WindowFunnelState::new();
        state.window_size_us = 50_000; // 50us window
        state.mode = FunnelMode::ALLOW_REENTRY;
        // First entry: window expires before step 1
        state.update(make_event(0, &[true, false]), 2);
        state.update(make_event(100_000, &[true, false]), 2); // reentry at 100ms
        state.update(make_event(120_000, &[false, true]), 2); // 20us after reentry, in window
        assert_eq!(state.finalize(), 2);
    }

    #[test]
    fn test_allow_reentry_empty() {
        let mut state = WindowFunnelState::new();
        state.mode = FunnelMode::ALLOW_REENTRY;
        assert_eq!(state.finalize(), 0);
    }

    // --- Combined mode tests ---

    #[test]
    fn test_strict_plus_strict_increase() {
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.mode = FunnelMode::STRICT.with(FunnelMode::STRICT_INCREASE);
        state.update(make_event(0, &[true, false, false]), 3);
        state.update(make_event(1000, &[false, true, false]), 3);
        state.update(make_event(2000, &[false, false, true]), 3);
        assert_eq!(state.finalize(), 3);
    }

    #[test]
    fn test_strict_order_plus_strict_increase_both_enforce() {
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.mode = FunnelMode::STRICT_ORDER.with(FunnelMode::STRICT_INCREASE);
        // strict_increase blocks same-ts, strict_order blocks earlier conditions
        state.update(make_event(0, &[true, false, false]), 3);
        state.update(make_event(0, &[false, true, false]), 3); // same ts → strict_increase skips
        assert_eq!(state.finalize(), 1);
    }

    #[test]
    fn test_strict_dedup_plus_strict_increase() {
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.mode = FunnelMode::STRICT_DEDUPLICATION.with(FunnelMode::STRICT_INCREASE);
        state.update(make_event(0, &[true, false]), 2);
        state.update(make_event(0, &[false, true]), 2); // both modes block same-ts
        assert_eq!(state.finalize(), 1);
    }

    #[test]
    fn test_strict_once_plus_strict_increase() {
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.mode = FunnelMode::STRICT_ONCE.with(FunnelMode::STRICT_INCREASE);
        state.update(make_event(0, &[true, false, false]), 3);
        state.update(make_event(1000, &[false, true, true]), 3); // strict_once: only step 1
        state.update(make_event(2000, &[false, false, true]), 3);
        assert_eq!(state.finalize(), 3);
    }

    #[test]
    fn test_allow_reentry_plus_strict_order() {
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.mode = FunnelMode::ALLOW_REENTRY.with(FunnelMode::STRICT_ORDER);
        state.update(make_event(0, &[true, false, false]), 3);
        state.update(make_event(1000, &[false, true, false]), 3);
        // Reentry fires, resets chain
        state.update(make_event(2000, &[true, false, false]), 3);
        state.update(make_event(3000, &[false, true, false]), 3);
        state.update(make_event(4000, &[false, false, true]), 3);
        assert_eq!(state.finalize(), 3);
    }

    #[test]
    fn test_all_modes_combined() {
        // Stress test: all modes at once with clean sequential data
        let mode = FunnelMode::STRICT
            .with(FunnelMode::STRICT_ORDER)
            .with(FunnelMode::STRICT_DEDUPLICATION)
            .with(FunnelMode::STRICT_INCREASE)
            .with(FunnelMode::STRICT_ONCE);
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.mode = mode;
        state.update(make_event(0, &[true, false, false]), 3);
        state.update(make_event(1000, &[false, true, false]), 3);
        state.update(make_event(2000, &[false, false, true]), 3);
        assert_eq!(state.finalize(), 3);
    }

    // --- Session 11: DuckDB zero-initialized target combine tests ---
    // DuckDB's segment tree creates fresh zero-initialized target states
    // and combines source states into them via combine_in_place. These tests
    // verify that ALL configuration fields are propagated correctly.

    #[test]
    fn test_combine_in_place_zero_target_propagates_window_size() {
        // Simulate DuckDB: fresh target + configured source
        let mut target = WindowFunnelState::new(); // zero-initialized
        let mut source = WindowFunnelState::new();
        source.window_size_us = 3_600_000_000;
        source.update(make_event(0, &[true, false]), 2);
        source.update(make_event(1_000_000, &[false, true]), 2);

        target.combine_in_place(&source);
        assert_eq!(target.window_size_us, 3_600_000_000);
        assert_eq!(target.finalize(), 2);
    }

    #[test]
    fn test_combine_in_place_zero_target_propagates_mode() {
        let mut target = WindowFunnelState::new();
        let mut source = WindowFunnelState::new();
        source.window_size_us = 3_600_000_000;
        source.mode = FunnelMode::STRICT_INCREASE;
        source.update(make_event(0, &[true, false]), 2);
        source.update(make_event(1000, &[false, true]), 2);

        target.combine_in_place(&source);
        assert_eq!(target.mode, FunnelMode::STRICT_INCREASE);
        assert_eq!(target.window_size_us, 3_600_000_000);
        assert_eq!(target.finalize(), 2);
    }

    #[test]
    fn test_combine_in_place_zero_target_propagates_num_conditions() {
        let mut target = WindowFunnelState::new();
        let mut source = WindowFunnelState::new();
        source.window_size_us = 3_600_000_000;
        source.update(make_event(0, &[true, false, false, false, false]), 5);

        target.combine_in_place(&source);
        assert_eq!(target.num_conditions, 5);
    }

    #[test]
    fn test_combine_in_place_zero_target_chain_finalize() {
        // Chain: zero target + source1 + source2 → finalize
        let mut target = WindowFunnelState::new();
        let mut s1 = WindowFunnelState::new();
        s1.window_size_us = 3_600_000_000;
        s1.mode = FunnelMode::STRICT;
        s1.update(make_event(0, &[true, false, false]), 3);

        let mut s2 = WindowFunnelState::new();
        s2.window_size_us = 3_600_000_000;
        s2.update(make_event(1000, &[false, true, false]), 3);
        s2.update(make_event(2000, &[false, false, true]), 3);

        target.combine_in_place(&s1);
        target.combine_in_place(&s2);
        assert_eq!(target.window_size_us, 3_600_000_000);
        assert_eq!(target.mode, FunnelMode::STRICT);
        assert_eq!(target.finalize(), 3);
    }

    #[test]
    fn test_combine_in_place_existing_window_not_overwritten() {
        // If target already has window_size, it should NOT be overwritten
        let mut target = WindowFunnelState::new();
        target.window_size_us = 1_000_000; // 1 second
        let mut source = WindowFunnelState::new();
        source.window_size_us = 3_600_000_000; // 1 hour

        target.combine_in_place(&source);
        // Target's window_size should be preserved (first-write-wins)
        assert_eq!(target.window_size_us, 1_000_000);
    }
}

#[cfg(test)]
mod proptests {
    use super::*;

    use proptest::prelude::*;

    proptest! {
        #[test]
        fn finalize_bounded_by_num_conditions(
            num_events in 1..=50usize,
            num_conditions in 2..=8usize,
        ) {
            let mut state = WindowFunnelState::new();
            state.window_size_us = i64::MAX;
            for i in 0..num_events {
                let bitmask = 1u32 << (i % num_conditions);
                state.update(Event::new(i as i64, bitmask), num_conditions);
            }
            let result = state.finalize();
            prop_assert!(result >= 0);
            prop_assert!(result <= num_conditions as i64);
        }

        #[test]
        fn empty_state_returns_zero(
            num_conditions in 0..=8usize,
        ) {
            let mut state = WindowFunnelState::new();
            state.num_conditions = num_conditions;
            prop_assert_eq!(state.finalize(), 0);
        }

        #[test]
        fn combine_preserves_events(
            n_a in 0..=20usize,
            n_b in 0..=20usize,
        ) {
            let mut a = WindowFunnelState::new();
            a.window_size_us = 3_600_000_000;
            for i in 0..n_a {
                a.update(Event::new(i as i64, 1u32), 2);
            }

            let mut b = WindowFunnelState::new();
            b.window_size_us = 3_600_000_000;
            for i in 0..n_b {
                b.update(Event::new((n_a + i) as i64, 2u32), 2);
            }

            let combined = a.combine(&b);
            prop_assert_eq!(combined.events.len(), n_a + n_b);
        }

        #[test]
        fn funnel_monotonic_with_more_events(
            window_us in 1_000_000i64..10_000_000_000,
            num_conditions in 2..=5usize,
        ) {
            // A complete funnel sequence should always reach num_conditions
            let mut state = WindowFunnelState::new();
            state.window_size_us = window_us;
            for i in 0..num_conditions {
                let bitmask = 1u32 << i;
                state.update(Event::new(i as i64, bitmask), num_conditions);
            }
            let result = state.finalize();
            prop_assert_eq!(result, num_conditions as i64);
        }

        // --- 32-condition property tests ---

        #[test]
        fn finalize_bounded_wide_conditions(
            num_events in 1..=50usize,
            num_conditions in 9..=32usize,
        ) {
            let mut state = WindowFunnelState::new();
            state.window_size_us = i64::MAX;
            for i in 0..num_events {
                let bitmask = 1u32 << (i % num_conditions);
                state.update(Event::new(i as i64, bitmask), num_conditions);
            }
            let result = state.finalize();
            prop_assert!(result >= 0);
            prop_assert!(result <= num_conditions as i64);
        }

        #[test]
        fn complete_funnel_wide_conditions(
            num_conditions in 9..=32usize,
        ) {
            // A complete sequence of conditions 0..n should reach n steps
            let mut state = WindowFunnelState::new();
            state.window_size_us = i64::MAX;
            for i in 0..num_conditions {
                let bitmask = 1u32 << i;
                state.update(Event::new(i as i64, bitmask), num_conditions);
            }
            let result = state.finalize();
            prop_assert_eq!(result, num_conditions as i64);
        }

        #[test]
        fn combine_preserves_events_wide(
            n_a in 0..=20usize,
            n_b in 0..=20usize,
            num_conditions in 9..=32usize,
        ) {
            let mut a = WindowFunnelState::new();
            a.window_size_us = i64::MAX;
            for i in 0..n_a {
                let bitmask = 1u32 << (i % num_conditions);
                a.update(Event::new(i as i64, bitmask), num_conditions);
            }

            let mut b = WindowFunnelState::new();
            b.window_size_us = i64::MAX;
            for i in 0..n_b {
                let bitmask = 1u32 << ((n_a + i) % num_conditions);
                b.update(Event::new((n_a + i) as i64, bitmask), num_conditions);
            }

            let combined = a.combine(&b);
            prop_assert_eq!(combined.events.len(), n_a + n_b);
        }
    }
}
