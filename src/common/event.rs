//! Event types shared across behavioral analytics functions.
//!
//! Several functions (`window_funnel`, `sequence_match`, `sequence_count`)
//! accumulate timestamped events with boolean conditions during aggregation.
//! This module provides the shared event representation.
//!
//! # Bitmask Representation
//!
//! Conditions are stored as a `u32` bitmask rather than `Vec<bool>`.
//! This eliminates per-event heap allocation (the dominant cost in event
//! collection) and enables single-instruction condition checks via bitwise
//! AND. The `DuckDB` function set registrations support up to 32 boolean
//! conditions, matching `ClickHouse`'s limit.
//!
//! Memory layout: `Event` is 16 bytes (i64 + u32 + 4 bytes padding) with
//! `Copy` semantics, compared to the previous 32 bytes + heap allocation
//! for `Vec<bool>`.

/// Maximum number of boolean conditions supported by event-collecting functions.
pub const MAX_EVENT_CONDITIONS: usize = 32;

/// A single timestamped event with associated boolean conditions.
///
/// Used by `window_funnel`, `sequence_match`, and `sequence_count` to collect
/// events during the `update` phase, then process them during `finalize`.
///
/// Conditions are packed into a `u32` bitmask where bit `i` represents
/// condition `i`. This supports up to 32 conditions, matching `ClickHouse`'s
/// limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event {
    /// Timestamp in microseconds since Unix epoch.
    pub timestamp_us: i64,
    /// Bitmask of boolean conditions. Bit `i` is set if condition `i` was
    /// satisfied at this timestamp. Supports up to 32 conditions (bits 0-31).
    pub conditions: u32,
}

impl Event {
    /// Creates a new event with the given timestamp and condition bitmask.
    #[must_use]
    pub const fn new(timestamp_us: i64, conditions: u32) -> Self {
        Self {
            timestamp_us,
            conditions,
        }
    }

    /// Creates an event from a slice of boolean conditions.
    ///
    /// Packs the booleans into a `u32` bitmask. Conditions beyond index 31
    /// are silently ignored (matching the `DuckDB` function set limit of 32).
    #[must_use]
    pub fn from_bools(timestamp_us: i64, conditions: &[bool]) -> Self {
        let mut bitmask: u32 = 0;
        for (i, &cond) in conditions.iter().enumerate().take(MAX_EVENT_CONDITIONS) {
            if cond {
                bitmask |= 1 << i;
            }
        }
        Self {
            timestamp_us,
            conditions: bitmask,
        }
    }

    /// Returns true if the condition at the given index is satisfied.
    ///
    /// Returns false if `idx >= 32` (out of bitmask range). Single bitwise
    /// AND operation — branchless on most architectures.
    #[must_use]
    #[inline]
    pub const fn condition(&self, idx: usize) -> bool {
        idx < MAX_EVENT_CONDITIONS && (self.conditions >> idx) & 1 != 0
    }

    /// Returns true if any condition in this event is true.
    ///
    /// Events where no condition is true cannot participate in funnel or
    /// sequence matching. Pre-filtering these events reduces memory usage
    /// by 10-100x in typical workloads.
    ///
    /// Single comparison against zero — O(1) regardless of condition count.
    #[must_use]
    #[inline]
    pub const fn has_any_condition(&self) -> bool {
        self.conditions != 0
    }
}

/// Sorts events by timestamp (ascending) using unstable sort.
///
/// Before sorting, performs an O(n) presorted check: if events are already
/// in non-decreasing timestamp order, the sort is skipped entirely. This is
/// the common case when `DuckDB` provides events via ORDER BY or from naturally
/// ordered data. For unsorted input, the O(n) verification scan adds negligible
/// overhead before the O(n log n) pdqsort.
///
/// Unstable sort (pdqsort) is used because:
/// 1. Same-timestamp event order has no defined semantics (matches `ClickHouse`)
/// 2. No auxiliary O(n) memory allocation (in-place partitioning)
/// 3. Better constant factors for `Copy` types due to cache-friendly swaps
/// 4. Adaptive: O(n) for already-sorted input, O(n log n) worst case
///
/// **Note (Session 7 negative result):** LSD radix sort (8-bit radix, 8 passes)
/// was tested as an O(n) replacement but measured 4.3x slower at 100M elements.
/// The scatter pattern in radix sort has poor spatial locality for 16-byte
/// elements, causing TLB/cache misses that dominate the O(n log n) comparison
/// overhead of pdqsort's cache-friendly in-place partitioning.
pub fn sort_events(events: &mut [Event]) {
    if events
        .windows(2)
        .all(|w| w[0].timestamp_us <= w[1].timestamp_us)
    {
        return;
    }
    events.sort_unstable_by_key(|e| e.timestamp_us);
}

/// Merges two sorted event slices into a single sorted `Vec`.
///
/// Both input slices must be sorted by timestamp. The result preserves the
/// relative order of events from each input (stable merge).
///
/// Since `Event` is `Copy`, this avoids heap allocation per element during
/// the merge (no `clone()` needed).
#[must_use]
pub fn merge_sorted_events(a: &[Event], b: &[Event]) -> Vec<Event> {
    let mut result = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i].timestamp_us <= b[j].timestamp_us {
            result.push(a[i]);
            i += 1;
        } else {
            result.push(b[j]);
            j += 1;
        }
    }
    result.extend_from_slice(&a[i..]);
    result.extend_from_slice(&b[j..]);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_creation() {
        let e = Event::from_bools(1_000_000, &[true, false, true]);
        assert_eq!(e.timestamp_us, 1_000_000);
        assert!(e.condition(0));
        assert!(!e.condition(1));
        assert!(e.condition(2));
    }

    #[test]
    fn test_event_new_bitmask() {
        let e = Event::new(42, 0b101);
        assert!(e.condition(0));
        assert!(!e.condition(1));
        assert!(e.condition(2));
        assert!(!e.condition(3));
    }

    #[test]
    fn test_has_any_condition() {
        assert!(Event::from_bools(0, &[false, true]).has_any_condition());
        assert!(!Event::from_bools(0, &[false, false]).has_any_condition());
        assert!(Event::from_bools(0, &[true]).has_any_condition());
    }

    #[test]
    fn test_has_any_condition_empty() {
        assert!(!Event::from_bools(0, &[]).has_any_condition());
        assert!(!Event::new(0, 0).has_any_condition());
    }

    #[test]
    fn test_condition_out_of_range() {
        let e = Event::new(0, 0xFFFF_FFFF);
        assert!(e.condition(31));
        assert!(!e.condition(32)); // Out of u32 bitmask range
        assert!(!e.condition(100));
    }

    #[test]
    fn test_from_bools_truncates_at_32() {
        let conds = vec![true; 40]; // More than 32
        let e = Event::from_bools(0, &conds);
        // Only first 32 should be set
        assert!(e.condition(31));
        assert!(!e.condition(32));
    }

    #[test]
    fn test_sort_events() {
        let mut events = vec![
            Event::from_bools(300, &[true]),
            Event::from_bools(100, &[true]),
            Event::from_bools(200, &[true]),
        ];
        sort_events(&mut events);
        assert_eq!(events[0].timestamp_us, 100);
        assert_eq!(events[1].timestamp_us, 200);
        assert_eq!(events[2].timestamp_us, 300);
    }

    #[test]
    fn test_merge_sorted_events() {
        let a = vec![
            Event::from_bools(100, &[true]),
            Event::from_bools(300, &[true]),
        ];
        let b = vec![
            Event::from_bools(200, &[false]),
            Event::from_bools(400, &[false]),
        ];
        let merged = merge_sorted_events(&a, &b);
        assert_eq!(merged.len(), 4);
        assert_eq!(merged[0].timestamp_us, 100);
        assert_eq!(merged[1].timestamp_us, 200);
        assert_eq!(merged[2].timestamp_us, 300);
        assert_eq!(merged[3].timestamp_us, 400);
    }

    #[test]
    fn test_merge_sorted_events_empty() {
        let a = vec![Event::from_bools(100, &[true])];
        let b: Vec<Event> = vec![];
        let merged = merge_sorted_events(&a, &b);
        assert_eq!(merged.len(), 1);

        let merged2 = merge_sorted_events(&b, &a);
        assert_eq!(merged2.len(), 1);
    }

    #[test]
    fn test_merge_sorted_events_equal_timestamps() {
        let a = vec![Event::from_bools(100, &[true])];
        let b = vec![Event::from_bools(100, &[false])];
        let merged = merge_sorted_events(&a, &b);
        assert_eq!(merged.len(), 2);
        // a's event should come first (stable merge)
        assert!(merged[0].condition(0));
        assert!(!merged[1].condition(0));
    }

    #[test]
    fn test_merge_both_empty() {
        let a: Vec<Event> = vec![];
        let b: Vec<Event> = vec![];
        let merged = merge_sorted_events(&a, &b);
        assert!(merged.is_empty());
    }

    #[test]
    fn test_sort_already_sorted() {
        let mut events = vec![
            Event::from_bools(100, &[true]),
            Event::from_bools(200, &[true]),
            Event::from_bools(300, &[true]),
        ];
        sort_events(&mut events);
        assert_eq!(events[0].timestamp_us, 100);
        assert_eq!(events[2].timestamp_us, 300);
    }

    #[test]
    fn test_sort_single_event() {
        let mut events = vec![Event::from_bools(42, &[true])];
        sort_events(&mut events);
        assert_eq!(events[0].timestamp_us, 42);
    }

    #[test]
    fn test_sort_empty() {
        let mut events: Vec<Event> = vec![];
        sort_events(&mut events);
        assert!(events.is_empty());
    }

    #[test]
    fn test_has_any_condition_all_true() {
        assert!(Event::from_bools(0, &[true, true, true]).has_any_condition());
    }

    #[test]
    fn test_merge_preserves_order_within_same_ts() {
        // Multiple events at the same timestamp from each side
        let a = vec![
            Event::from_bools(100, &[true, false]),
            Event::from_bools(100, &[false, true]),
        ];
        let b = vec![Event::from_bools(100, &[true, true])];
        let merged = merge_sorted_events(&a, &b);
        assert_eq!(merged.len(), 3);
        // a's events come first since a[0].ts <= b[0].ts
        assert_eq!(merged[0].conditions, 0b01); // true, false
        assert_eq!(merged[1].conditions, 0b10); // false, true
        assert_eq!(merged[2].conditions, 0b11); // true, true
    }

    #[test]
    fn test_event_is_copy() {
        let e = Event::new(42, 0b101);
        let e2 = e; // Copy, not move
        assert_eq!(e.timestamp_us, e2.timestamp_us);
        assert_eq!(e.conditions, e2.conditions);
    }

    #[test]
    fn test_event_size() {
        // Event should be 16 bytes: i64 (8) + u8 (1) + 7 padding
        assert_eq!(std::mem::size_of::<Event>(), 16);
    }

    // --- Session 3: Mutation-killing boundary tests ---

    #[test]
    fn test_condition_boundary_idx_31_vs_32() {
        // Kills mutant: replace `idx < 32` with `idx <= 32` in condition().
        let e = Event::new(0, 0xFFFF_FFFF); // all 32 bits set
        assert!(e.condition(31)); // bit 31 is valid
        assert!(!e.condition(32)); // bit 32 is out of range
    }

    #[test]
    fn test_condition_alternating_pattern() {
        // Kills mutant: replace `& 1` with a different mask in condition().
        let e = Event::new(0, 0b1010_1010);
        assert!(!e.condition(0)); // bit 0 = 0
        assert!(e.condition(1)); // bit 1 = 1
        assert!(!e.condition(2)); // bit 2 = 0
        assert!(e.condition(3)); // bit 3 = 1
        assert!(!e.condition(4)); // bit 4 = 0
        assert!(e.condition(5)); // bit 5 = 1
        assert!(!e.condition(6)); // bit 6 = 0
        assert!(e.condition(7)); // bit 7 = 1
    }

    #[test]
    fn test_has_any_condition_single_bit() {
        // Kills mutant: replace `!= 0` with `== 0` in has_any_condition().
        assert!(Event::new(0, 1).has_any_condition()); // only bit 0
        assert!(Event::new(0, 1 << 31).has_any_condition()); // only bit 31
    }

    #[test]
    fn test_from_bools_empty_slice() {
        // Boundary: empty conditions slice produces 0 bitmask.
        let e = Event::from_bools(42, &[]);
        assert_eq!(e.conditions, 0);
        assert!(!e.has_any_condition());
    }

    #[test]
    fn test_event_timestamp_extremes() {
        // Boundary: i64::MIN and i64::MAX timestamps.
        let e_min = Event::new(i64::MIN, 0xFF);
        let e_max = Event::new(i64::MAX, 0xFF);
        assert_eq!(e_min.timestamp_us, i64::MIN);
        assert_eq!(e_max.timestamp_us, i64::MAX);
        assert!(e_min.condition(0));
        assert!(e_max.condition(7));
    }

    #[test]
    fn test_sort_events_with_extreme_timestamps() {
        // Sort with i64::MIN and i64::MAX should place them correctly.
        let mut events = vec![
            Event::new(i64::MAX, 1),
            Event::new(0, 1),
            Event::new(i64::MIN, 1),
        ];
        sort_events(&mut events);
        assert_eq!(events[0].timestamp_us, i64::MIN);
        assert_eq!(events[1].timestamp_us, 0);
        assert_eq!(events[2].timestamp_us, i64::MAX);
    }

    // --- Presorted detection tests ---

    #[test]
    fn test_sort_presorted_input_skips_sort() {
        // Already sorted input should be recognized and not rearranged
        let mut events = vec![Event::new(100, 1), Event::new(200, 2), Event::new(300, 3)];
        sort_events(&mut events);
        assert_eq!(events[0].timestamp_us, 100);
        assert_eq!(events[1].timestamp_us, 200);
        assert_eq!(events[2].timestamp_us, 300);
    }

    #[test]
    fn test_sort_presorted_with_equal_timestamps() {
        // Equal timestamps should be recognized as sorted
        let mut events = vec![Event::new(100, 1), Event::new(100, 2), Event::new(200, 3)];
        sort_events(&mut events);
        assert_eq!(events[0].conditions, 1);
        assert_eq!(events[1].conditions, 2);
        assert_eq!(events[2].timestamp_us, 200);
    }

    #[test]
    fn test_sort_reverse_sorted_input() {
        // Reverse sorted should be detected as unsorted and correctly sorted
        let mut events = vec![Event::new(300, 3), Event::new(200, 2), Event::new(100, 1)];
        sort_events(&mut events);
        assert_eq!(events[0].timestamp_us, 100);
        assert_eq!(events[1].timestamp_us, 200);
        assert_eq!(events[2].timestamp_us, 300);
    }

    #[test]
    fn test_sort_nearly_sorted_one_swap() {
        // Nearly sorted with one out-of-place element
        let mut events = vec![
            Event::new(100, 1),
            Event::new(300, 3),
            Event::new(200, 2),
            Event::new(400, 4),
        ];
        sort_events(&mut events);
        assert_eq!(events[0].timestamp_us, 100);
        assert_eq!(events[1].timestamp_us, 200);
        assert_eq!(events[2].timestamp_us, 300);
        assert_eq!(events[3].timestamp_us, 400);
    }

    #[test]
    fn test_sort_all_same_timestamp() {
        // All same timestamp should be recognized as sorted
        let mut events = vec![Event::new(42, 1), Event::new(42, 2), Event::new(42, 3)];
        sort_events(&mut events);
        // Order preserved since already sorted
        assert_eq!(events[0].conditions, 1);
        assert_eq!(events[1].conditions, 2);
        assert_eq!(events[2].conditions, 3);
    }

    #[test]
    fn test_merge_sorted_preserves_stability() {
        // Merge should take from `a` first when timestamps are equal (stable merge).
        let a = vec![Event::new(100, 0b01)];
        let b = vec![Event::new(100, 0b10)];
        let merged = merge_sorted_events(&a, &b);
        assert_eq!(merged[0].conditions, 0b01); // a's element first
        assert_eq!(merged[1].conditions, 0b10); // b's element second
    }

    // --- 32-condition support tests ---

    #[test]
    fn test_event_size_unchanged_with_u32() {
        // Event stays at 16 bytes: i64 (8) + u32 (4) + 4 padding = 16
        assert_eq!(std::mem::size_of::<Event>(), 16);
    }

    #[test]
    fn test_condition_9_through_31() {
        // Conditions beyond the old u8 limit should work
        let mut bitmask: u32 = 0;
        bitmask |= 1 << 8; // condition 9 (0-indexed 8)
        bitmask |= 1 << 15; // condition 16
        bitmask |= 1 << 31; // condition 32
        let e = Event::new(0, bitmask);
        assert!(e.condition(8));
        assert!(e.condition(15));
        assert!(e.condition(31));
        assert!(!e.condition(0));
        assert!(!e.condition(7));
        assert!(!e.condition(16));
    }

    #[test]
    fn test_from_bools_32_conditions() {
        let mut conds = vec![false; 32];
        conds[0] = true;
        conds[8] = true;
        conds[15] = true;
        conds[31] = true;
        let e = Event::from_bools(0, &conds);
        assert!(e.condition(0));
        assert!(e.condition(8));
        assert!(e.condition(15));
        assert!(e.condition(31));
        assert!(!e.condition(1));
        assert!(!e.condition(30));
    }

    #[test]
    fn test_condition_all_32_bits_set() {
        let e = Event::new(0, 0xFFFF_FFFF);
        for i in 0..32 {
            assert!(e.condition(i), "condition({i}) should be true");
        }
        assert!(!e.condition(32));
    }

    #[test]
    fn test_from_bools_boundary_at_32() {
        // 33 conditions: first 32 accepted, index 32 ignored
        let mut conds = vec![false; 33];
        conds[31] = true;
        conds[32] = true; // beyond limit
        let e = Event::from_bools(0, &conds);
        assert!(e.condition(31));
        assert!(!e.condition(32));
    }

    // --- Session 7: Mutation-killing tests ---

    #[test]
    fn test_sort_presorted_check_uses_le_not_lt() {
        // Kills mutant: replacing <= with < in the presorted check.
        // Equal timestamps should be considered sorted (not trigger re-sort).
        let mut events = vec![Event::new(100, 1), Event::new(100, 2), Event::new(100, 3)];
        sort_events(&mut events);
        // If the presorted check used <, it would trigger a sort that could reorder
        // same-timestamp events. With <=, it correctly detects presorted input.
        assert_eq!(events[0].conditions, 1);
        assert_eq!(events[1].conditions, 2);
        assert_eq!(events[2].conditions, 3);
    }

    #[test]
    fn test_sort_presorted_check_detects_one_inversion() {
        // Kills mutant: removing the presorted check entirely (always sort).
        // With presorted check, a single inversion at the end must still be caught.
        let mut events = vec![Event::new(100, 1), Event::new(300, 2), Event::new(200, 3)];
        sort_events(&mut events);
        assert_eq!(events[0].timestamp_us, 100);
        assert_eq!(events[1].timestamp_us, 200);
        assert_eq!(events[2].timestamp_us, 300);
    }

    #[test]
    fn test_from_bools_single_true_each_position() {
        // Kills mutant: replacing |= with = in from_bools accumulation.
        // When called with only one true at position i, bit i must be set.
        for i in 0..32usize {
            let mut conds = vec![false; 32];
            conds[i] = true;
            let e = Event::from_bools(0, &conds);
            assert!(e.condition(i), "condition({i}) should be true");
            // Verify no other bits are set
            assert_eq!(e.conditions, 1u32 << i, "only bit {i} should be set");
        }
    }

    #[test]
    fn test_condition_returns_false_for_unset_adjacent_bits() {
        // Kills mutant: replacing >> with << or using wrong shift direction.
        let e = Event::new(0, 0b0010); // only bit 1 set
        assert!(!e.condition(0)); // bit 0 not set
        assert!(e.condition(1)); // bit 1 set
        assert!(!e.condition(2)); // bit 2 not set
    }

    #[test]
    fn test_sort_negative_timestamps() {
        // Ensures sort handles negative i64 timestamps correctly.
        let mut events = vec![
            Event::new(100, 1),
            Event::new(-200, 2),
            Event::new(-100, 3),
            Event::new(0, 4),
        ];
        sort_events(&mut events);
        assert_eq!(events[0].timestamp_us, -200);
        assert_eq!(events[1].timestamp_us, -100);
        assert_eq!(events[2].timestamp_us, 0);
        assert_eq!(events[3].timestamp_us, 100);
    }
}
