//! `retention` — Aggregate function for cohort retention analysis.
//!
//! Takes N boolean conditions and returns a `BOOLEAN[]` array of length N.
//! `result[0]` is true if condition 0 was ever true for any row in the group.
//! `result[i]` (for i > 0) is true if BOTH condition 0 AND condition i were
//! true for some row(s) in the group (not necessarily the same row).
//!
//! This matches `ClickHouse` `retention()` semantics exactly.
//!
//! # SQL Usage
//!
//! ```sql
//! SELECT cohort_month,
//!   retention(
//!     activity_date = cohort_month,
//!     activity_date = cohort_month + INTERVAL '1 month',
//!     activity_date = cohort_month + INTERVAL '2 months'
//!   ) as retained
//! FROM user_activity
//! GROUP BY user_id, cohort_month
//! ```

/// Maximum number of conditions supported by retention.
pub const MAX_CONDITIONS: usize = 32;

/// State for the retention aggregate function.
///
/// Tracks which conditions have been satisfied by any row in the group.
/// During `finalize`, applies the anchor condition (condition 0) requirement:
/// if condition 0 was never true, all results are false.
#[derive(Debug, Clone)]
pub struct RetentionState {
    /// Bitmask of conditions that were true for at least one row.
    /// Bit `i` is set if condition `i` was true for some row.
    pub conditions_met: u32,
    /// Number of conditions (set during first update).
    pub num_conditions: usize,
}

impl RetentionState {
    /// Creates a new empty state.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            conditions_met: 0,
            num_conditions: 0,
        }
    }

    /// Updates the state with a row of condition values.
    ///
    /// Each condition is OR'd into the bitmask: if condition `i` is true
    /// for this row, bit `i` is set.
    #[inline]
    pub fn update(&mut self, conditions: &[bool]) {
        self.num_conditions = conditions.len();
        for (i, &cond) in conditions.iter().enumerate() {
            if cond && i < MAX_CONDITIONS {
                self.conditions_met |= 1 << i;
            }
        }
    }

    /// Combines two states by OR-ing their bitmasks.
    ///
    /// This is correct because retention only cares whether each condition
    /// was satisfied by ANY row — it doesn't matter which row.
    #[must_use]
    #[inline]
    pub fn combine(&self, other: &Self) -> Self {
        Self {
            conditions_met: self.conditions_met | other.conditions_met,
            num_conditions: self.num_conditions.max(other.num_conditions),
        }
    }

    /// Produces the final retention result.
    ///
    /// Returns a `Vec<bool>` of length `num_conditions` where:
    /// - `result[0]` = condition 0 was ever true (anchor condition)
    /// - `result[i]` = condition 0 AND condition i were both ever true
    ///
    /// If the anchor condition (condition 0) was never true, all values
    /// are false — you can't retain a user who never appeared in the cohort.
    #[must_use]
    pub fn finalize(&self) -> Vec<bool> {
        let anchor_met = self.conditions_met & 1 != 0;
        (0..self.num_conditions)
            .map(|i| {
                if !anchor_met {
                    false
                } else if i == 0 {
                    true
                } else if i >= MAX_CONDITIONS {
                    // Conditions beyond u32 capacity are always false
                    false
                } else {
                    self.conditions_met & (1 << i) != 0
                }
            })
            .collect()
    }
}

impl Default for RetentionState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_state() {
        let state = RetentionState::new();
        assert!(state.finalize().is_empty());
    }

    #[test]
    fn test_single_condition_true() {
        let mut state = RetentionState::new();
        state.update(&[true]);
        assert_eq!(state.finalize(), vec![true]);
    }

    #[test]
    fn test_single_condition_false() {
        let mut state = RetentionState::new();
        state.update(&[false]);
        assert_eq!(state.finalize(), vec![false]);
    }

    #[test]
    fn test_anchor_not_met() {
        let mut state = RetentionState::new();
        // Anchor (cond 0) is never true, so all results should be false
        state.update(&[false, true, true]);
        assert_eq!(state.finalize(), vec![false, false, false]);
    }

    #[test]
    fn test_full_retention() {
        let mut state = RetentionState::new();
        state.update(&[true, true, true, true]);
        assert_eq!(state.finalize(), vec![true, true, true, true]);
    }

    #[test]
    fn test_partial_retention() {
        let mut state = RetentionState::new();
        // Row 1: cond0 true (anchor met)
        state.update(&[true, false, false, false]);
        // Row 2: cond1 true
        state.update(&[false, true, false, false]);
        // Row 3: cond3 true (cond2 still false)
        state.update(&[false, false, false, true]);
        assert_eq!(state.finalize(), vec![true, true, false, true]);
    }

    #[test]
    fn test_retention_across_multiple_rows() {
        let mut state = RetentionState::new();
        // Typical cohort retention pattern:
        // Day 0: user was active (anchor)
        state.update(&[true, false, false]);
        // Day 1: user was active
        state.update(&[false, true, false]);
        // Day 2: user was NOT active (no update with cond2=true)
        assert_eq!(state.finalize(), vec![true, true, false]);
    }

    #[test]
    fn test_combine() {
        let mut a = RetentionState::new();
        a.update(&[true, false, false]);

        let mut b = RetentionState::new();
        b.update(&[false, true, false]);

        let combined = a.combine(&b);
        assert_eq!(combined.finalize(), vec![true, true, false]);
    }

    #[test]
    fn test_combine_empty() {
        let a = RetentionState::new();
        let mut b = RetentionState::new();
        b.update(&[true, true]);
        let combined = a.combine(&b);
        assert_eq!(combined.finalize(), vec![true, true]);
    }

    #[test]
    fn test_combine_both_empty() {
        let a = RetentionState::new();
        let b = RetentionState::new();
        let combined = a.combine(&b);
        assert!(combined.finalize().is_empty());
    }

    #[test]
    fn test_combine_is_commutative() {
        let mut a = RetentionState::new();
        a.update(&[true, false, true]);

        let mut b = RetentionState::new();
        b.update(&[false, true, false]);

        let ab = a.combine(&b);
        let ba = b.combine(&a);
        assert_eq!(ab.finalize(), ba.finalize());
    }

    #[test]
    fn test_combine_is_associative() {
        let mut a = RetentionState::new();
        a.update(&[true, false, false]);
        let mut b = RetentionState::new();
        b.update(&[false, true, false]);
        let mut c = RetentionState::new();
        c.update(&[false, false, true]);

        let ab_c = a.combine(&b).combine(&c);
        let a_bc = a.combine(&b.combine(&c));
        assert_eq!(ab_c.finalize(), a_bc.finalize());
    }

    #[test]
    fn test_many_conditions() {
        let mut state = RetentionState::new();
        let mut conds = vec![false; 32];
        conds[0] = true;
        conds[15] = true;
        conds[31] = true;
        state.update(&conds);
        let result = state.finalize();
        assert!(result[0]);
        assert!(result[15]);
        assert!(result[31]);
        assert!(!result[1]);
    }

    #[test]
    fn test_null_conditions_treated_as_false() {
        // In DuckDB, NULLs will be converted to false before reaching update()
        let mut state = RetentionState::new();
        state.update(&[true, false, false]); // cond1 and cond2 were NULL → false
        assert_eq!(state.finalize(), vec![true, false, false]);
    }

    #[test]
    fn test_conditions_beyond_32_silently_ignored() {
        // Conditions beyond MAX_CONDITIONS (32) are silently ignored because
        // the bitmask is u32. This test documents the behavior.
        let mut state = RetentionState::new();
        let mut conds = vec![false; 33];
        conds[0] = true; // anchor
        conds[32] = true; // beyond u32 capacity
        state.update(&conds);
        let result = state.finalize();
        assert!(result[0]); // anchor is met
                            // Condition 32 is silently ignored (bit 32 doesn't fit in u32)
        assert!(!result[32]);
    }

    #[test]
    fn test_conditions_at_max_boundary() {
        // Condition 31 (the last one in u32) should work
        let mut state = RetentionState::new();
        let mut conds = vec![false; 32];
        conds[0] = true;
        conds[31] = true;
        state.update(&conds);
        let result = state.finalize();
        assert!(result[0]);
        assert!(result[31]);
    }

    #[test]
    fn test_idempotent_updates() {
        // Updating with the same conditions multiple times should not change result
        let mut state = RetentionState::new();
        state.update(&[true, true, false]);
        state.update(&[true, true, false]);
        state.update(&[true, true, false]);
        assert_eq!(state.finalize(), vec![true, true, false]);
    }

    #[test]
    fn test_combine_or_not_xor() {
        // Mutation test coverage: combine uses OR (|), not XOR (^).
        // When both states have the same condition set, OR preserves it, XOR unsets it.
        let mut a = RetentionState::new();
        a.update(&[true, true, false]);

        let mut b = RetentionState::new();
        b.update(&[true, true, false]); // same conditions as a

        let combined = a.combine(&b);
        // OR: conditions_met stays set. XOR: conditions_met becomes 0.
        assert_eq!(combined.finalize(), vec![true, true, false]);
    }

    #[test]
    fn test_single_condition_anchor_only() {
        let mut state = RetentionState::new();
        state.update(&[true]);
        assert_eq!(state.finalize(), vec![true]);
    }

    #[test]
    fn test_all_false() {
        let mut state = RetentionState::new();
        state.update(&[false, false, false]);
        assert_eq!(state.finalize(), vec![false, false, false]);
    }

    // --- Session 3: Mutation-killing boundary tests ---

    #[test]
    fn test_combine_identical_states_or_not_xor() {
        // Kills mutant: replace `|` with `^` in combine.
        // When both states have the SAME bits set, OR preserves them, XOR clears them.
        let mut a = RetentionState::new();
        a.update(&[true, true, true]);
        let mut b = RetentionState::new();
        b.update(&[true, true, true]);
        let combined = a.combine(&b);
        // XOR would give conditions_met = 0, finalize = [false, false, false]
        assert_eq!(combined.finalize(), vec![true, true, true]);
    }

    #[test]
    fn test_anchor_check_only_bit_zero() {
        // Kills mutant: replace `& 1` with `& 0xFF` or another mask in finalize.
        // Only bit 0 should serve as anchor, not any bit.
        let mut state = RetentionState::new();
        state.update(&[false, true, true, true]);
        let result = state.finalize();
        // Anchor (bit 0) NOT set, so ALL results must be false
        assert!(!result[0]);
        assert!(!result[1]);
        assert!(!result[2]);
        assert!(!result[3]);
    }

    #[test]
    fn test_finalize_i_zero_special_case() {
        // Kills mutant: replace `i == 0` with `i != 0` in finalize.
        // When anchor is met, result[0] should be true unconditionally.
        // When anchor is NOT met, result[0] should be false.
        let mut state = RetentionState::new();
        state.update(&[true, false]);
        let result = state.finalize();
        assert!(result[0]); // anchor met → result[0] = true
        assert!(!result[1]); // bit 1 not set

        let mut state2 = RetentionState::new();
        state2.update(&[false, true]);
        let result2 = state2.finalize();
        assert!(!result2[0]); // anchor NOT met → result[0] = false
    }

    #[test]
    fn test_finalize_i_at_max_conditions_boundary() {
        // Kills mutant: replace `i >= MAX_CONDITIONS` with `i > MAX_CONDITIONS`.
        // Condition at exactly index 32 should be false (i >= 32 → true → false).
        let mut state = RetentionState::new();
        let mut conds = vec![false; 33];
        conds[0] = true;
        conds[31] = true; // last valid u32 bit
        conds[32] = true; // at MAX_CONDITIONS boundary
        state.update(&conds);
        let result = state.finalize();
        assert!(result[31]); // within u32 capacity
        assert!(!result[32]); // at boundary → false
    }

    #[test]
    fn test_update_respects_max_conditions_guard() {
        // Kills mutant: remove `i < MAX_CONDITIONS` check in update.
        // Setting bit 32 in a u32 would cause 1 << 32 = 1 (wraps around on some platforms).
        let mut state = RetentionState::new();
        let mut conds = vec![false; 33];
        conds[0] = true;
        conds[32] = true;
        state.update(&conds);
        // Bit 32 must NOT wrap around to set bit 0 again
        // conditions_met should be 1 (only bit 0), not 1 | (1 << 32)
        assert_eq!(state.conditions_met, 1);
    }

    // --- Session 11: DuckDB zero-initialized target combine tests ---

    #[test]
    fn test_combine_zero_target_propagates_conditions() {
        // DuckDB's segment tree: fresh target + configured source
        let target = RetentionState::new(); // zero-initialized
        let mut source = RetentionState::new();
        source.update(&[true, true, false]);

        let combined = target.combine(&source);
        assert_eq!(combined.num_conditions, 3);
        assert_eq!(combined.finalize(), vec![true, true, false]);
    }

    #[test]
    fn test_combine_zero_target_chain_finalize() {
        // Chain: zero target + s1 + s2 → finalize
        let target = RetentionState::new();
        let mut s1 = RetentionState::new();
        s1.update(&[true, false, false]);
        let mut s2 = RetentionState::new();
        s2.update(&[false, true, false]);
        let mut s3 = RetentionState::new();
        s3.update(&[false, false, true]);

        let combined = target.combine(&s1).combine(&s2).combine(&s3);
        assert_eq!(combined.finalize(), vec![true, true, true]);
    }

    #[test]
    fn test_combine_zero_target_num_conditions_propagates() {
        let target = RetentionState::new();
        let mut source = RetentionState::new();
        source.num_conditions = 5;
        source.conditions_met = 0b10001; // bits 0 and 4

        let combined = target.combine(&source);
        assert_eq!(combined.num_conditions, 5);
        assert_eq!(combined.conditions_met, 0b10001);
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn combine_is_commutative(
            a_conds in prop::collection::vec(prop::bool::ANY, 2..=8usize),
        ) {
            let n = a_conds.len();
            // Generate b_conds from a by rotating
            let mut b_conds = vec![false; n];
            for i in 0..n {
                b_conds[i] = a_conds[(i + 1) % n];
            }

            let mut a = RetentionState::new();
            a.update(&a_conds);
            let mut b = RetentionState::new();
            b.update(&b_conds);

            let ab = a.combine(&b);
            let ba = b.combine(&a);
            prop_assert_eq!(ab.finalize(), ba.finalize());
        }

        #[test]
        fn combine_with_empty_is_identity(
            conds in prop::collection::vec(prop::bool::ANY, 1..=8usize),
        ) {
            let mut s = RetentionState::new();
            s.update(&conds);
            let empty = RetentionState::new();

            let se = s.combine(&empty);
            prop_assert_eq!(se.conditions_met, s.conditions_met);
            prop_assert_eq!(se.num_conditions, s.num_conditions);
        }

        #[test]
        fn update_is_idempotent(
            conds in prop::collection::vec(prop::bool::ANY, 1..=8usize),
        ) {
            let mut once = RetentionState::new();
            once.update(&conds);

            let mut twice = RetentionState::new();
            twice.update(&conds);
            twice.update(&conds);

            prop_assert_eq!(once.finalize(), twice.finalize());
        }

        #[test]
        fn anchor_false_means_all_false(
            tail in prop::collection::vec(prop::bool::ANY, 1..=7usize),
        ) {
            let mut conds = vec![false]; // anchor is always false
            conds.extend_from_slice(&tail);

            let mut state = RetentionState::new();
            state.update(&conds);
            let result = state.finalize();
            prop_assert!(result.iter().all(|&v| !v));
        }

        // --- 32-condition property tests ---

        #[test]
        fn combine_commutative_wide_conditions(
            a_conds in prop::collection::vec(prop::bool::ANY, 9..=32usize),
        ) {
            let n = a_conds.len();
            let mut b_conds = vec![false; n];
            for i in 0..n {
                b_conds[i] = a_conds[(i + 1) % n];
            }

            let mut a = RetentionState::new();
            a.update(&a_conds);
            let mut b = RetentionState::new();
            b.update(&b_conds);

            let ab = a.combine(&b);
            let ba = b.combine(&a);
            prop_assert_eq!(ab.finalize(), ba.finalize());
        }

        #[test]
        fn update_idempotent_wide_conditions(
            conds in prop::collection::vec(prop::bool::ANY, 9..=32usize),
        ) {
            let mut once = RetentionState::new();
            once.update(&conds);

            let mut twice = RetentionState::new();
            twice.update(&conds);
            twice.update(&conds);

            prop_assert_eq!(once.finalize(), twice.finalize());
        }

        #[test]
        fn anchor_false_all_false_wide(
            tail in prop::collection::vec(prop::bool::ANY, 8..=31usize),
        ) {
            let mut conds = vec![false]; // anchor always false
            conds.extend_from_slice(&tail);

            let mut state = RetentionState::new();
            state.update(&conds);
            let result = state.finalize();
            prop_assert!(result.iter().all(|&v| !v));
        }

        #[test]
        fn condition_31_preserved_through_combine(
            val_a in prop::bool::ANY,
            val_b in prop::bool::ANY,
        ) {
            // Test that bit 31 (the highest valid condition) survives combine
            let mut conds_a = vec![true; 32]; // anchor=true so bit 31 is visible
            conds_a[31] = val_a;
            let mut conds_b = vec![true; 32];
            conds_b[31] = val_b;

            let mut a = RetentionState::new();
            a.update(&conds_a);
            let mut b = RetentionState::new();
            b.update(&conds_b);

            let combined = a.combine(&b);
            let result = combined.finalize();
            // Bit 31 should be true if either a or b had it true (OR semantics)
            prop_assert_eq!(result[31], val_a || val_b);
        }
    }
}
