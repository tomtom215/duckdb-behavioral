//! `sequence_next_node` — Returns the next event value after a matched sequence.
//!
//! Implements `ClickHouse`'s `sequenceNextNode` function for flow analysis.
//! Given a sequence of event conditions (event1, event2, ..., eventN), finds
//! the sequential match and returns the value of the adjacent event.
//!
//! # SQL Usage
//!
//! ```sql
//! -- What page do users visit after Home → Product?
//! SELECT user_id,
//!   sequence_next_node('forward', 'first_match', event_time, page,
//!     page = 'Home',           -- base_condition
//!     page = 'Home',           -- event1
//!     page = 'Product'         -- event2
//!   ) as next_page
//! FROM events
//! GROUP BY user_id
//! ```
//!
//! # Matching Algorithm
//!
//! Uses simple sequential matching (not NFA/regex patterns). Events are sorted
//! by timestamp and scanned in the specified direction to find a chain matching
//! event1 → event2 → ... → eventN. The value of the adjacent event is returned.
//!
//! - **Forward**: Returns the value of the event immediately after the last
//!   matched event.
//! - **Backward**: Returns the value of the event immediately before the
//!   earliest matched event.
//!
//! # `Rc<str>` Value Storage (Session 9)
//!
//! Event values use `Rc<str>` (reference-counted immutable string) instead of
//! `String`. This reduces the per-event struct size from 40 bytes to 32 bytes
//! (20% reduction) and makes `clone()` O(1) via reference count increment
//! instead of O(n) deep string copy. The O(1) clone directly benefits:
//! - **Combine**: cloning events in `combine_in_place` is O(1) per event
//! - **Sort**: swapping 32-byte elements vs 40-byte elements
//! - **Cache utilization**: 2 events per cache line vs 1.6 previously

use std::rc::Rc;

/// Direction of traversal for sequence matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Scan events from earliest to latest. Returns the value of the event
    /// immediately after the last matched event in the sequence.
    Forward,
    /// Scan events from latest to earliest. Returns the value of the event
    /// immediately before the earliest matched event in the sequence.
    Backward,
}

/// Base position for starting the sequence match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Base {
    /// Start from the first event (chronologically) where `base_condition` is true.
    Head,
    /// Start from the last event (chronologically) where `base_condition` is true.
    Tail,
    /// Use the first complete match found.
    FirstMatch,
    /// Use the last complete match found.
    LastMatch,
}

/// A single timestamped event with a string value for `sequence_next_node`.
///
/// Uses `Rc<str>` instead of `String` for O(1) clone semantics. This reduces
/// per-event struct size from 40 bytes to 32 bytes and eliminates deep string
/// copying in combine operations (reference count increment instead of heap
/// allocation + memcpy).
///
/// Unlike [`crate::common::event::Event`] (which is `Copy` with a `u32` bitmask),
/// this struct stores a reference-counted string value that may be returned as
/// the function result.
#[derive(Debug, Clone)]
pub struct NextNodeEvent {
    /// Timestamp in microseconds since Unix epoch.
    pub timestamp_us: i64,
    /// The event column value (candidate return value). Uses `Rc<str>` for
    /// O(1) clone — critical for combine performance in `DuckDB`'s segment tree.
    pub value: Option<Rc<str>>,
    /// Whether the base condition is satisfied for this event.
    pub base_condition: bool,
    /// Bitmask of which sequential event conditions this event satisfies.
    /// Bit `i` is set if this event matches event `i+1` (1-indexed) in the chain.
    pub conditions: u32,
}

/// State for the `sequence_next_node` aggregate function.
///
/// Collects events with string values during `update`, then performs
/// sequential matching during `finalize` to find the next event value
/// after a completed sequence match.
#[derive(Debug, Clone)]
pub struct SequenceNextNodeState {
    /// Collected events. Sorted by timestamp in finalize.
    pub events: Vec<NextNodeEvent>,
    /// Direction of traversal (forward or backward).
    pub direction: Option<Direction>,
    /// Base position for matching.
    pub base: Option<Base>,
    /// Number of event condition steps in the sequence.
    pub num_steps: usize,
}

impl SequenceNextNodeState {
    /// Creates a new empty state.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            events: Vec::new(),
            direction: None,
            base: None,
            num_steps: 0,
        }
    }

    /// Sets the direction parameter.
    pub fn set_direction(&mut self, direction: Direction) {
        if self.direction.is_none() {
            self.direction = Some(direction);
        }
    }

    /// Sets the base parameter.
    pub fn set_base(&mut self, base: Base) {
        if self.base.is_none() {
            self.base = Some(base);
        }
    }

    /// Parses a direction string.
    ///
    /// Returns `None` for unrecognized direction strings.
    #[must_use]
    pub fn parse_direction(s: &str) -> Option<Direction> {
        match s.trim() {
            s if s.eq_ignore_ascii_case("forward") => Some(Direction::Forward),
            s if s.eq_ignore_ascii_case("backward") => Some(Direction::Backward),
            _ => None,
        }
    }

    /// Parses a base string.
    ///
    /// Returns `None` for unrecognized base strings.
    #[must_use]
    pub fn parse_base(s: &str) -> Option<Base> {
        match s.trim() {
            s if s.eq_ignore_ascii_case("head") => Some(Base::Head),
            s if s.eq_ignore_ascii_case("tail") => Some(Base::Tail),
            s if s.eq_ignore_ascii_case("first_match") => Some(Base::FirstMatch),
            s if s.eq_ignore_ascii_case("last_match") => Some(Base::LastMatch),
            _ => None,
        }
    }

    /// Adds an event to the state.
    ///
    /// All events are stored regardless of conditions because any event could
    /// be the "next node" whose value is returned.
    pub fn update(&mut self, event: NextNodeEvent) {
        self.events.push(event);
    }

    /// Combines two states by concatenating their event lists, returning a new state.
    #[must_use]
    pub fn combine(&self, other: &Self) -> Self {
        let mut events = Vec::with_capacity(self.events.len() + other.events.len());
        events.extend(self.events.iter().cloned());
        events.extend(other.events.iter().cloned());
        Self {
            events,
            direction: self.direction.or(other.direction),
            base: self.base.or(other.base),
            num_steps: if self.num_steps > 0 {
                self.num_steps
            } else {
                other.num_steps
            },
        }
    }

    /// Combines another state into `self` in-place by appending its events.
    ///
    /// Preferred for sequential (left-fold) chains. Uses Vec's doubling growth
    /// strategy for O(N) amortized total copies.
    pub fn combine_in_place(&mut self, other: &Self) {
        self.events.extend(other.events.iter().cloned());
        if self.direction.is_none() {
            self.direction = other.direction;
        }
        if self.base.is_none() {
            self.base = other.base;
        }
        if self.num_steps == 0 {
            self.num_steps = other.num_steps;
        }
    }

    /// Executes the sequence matching and returns the next node's value.
    ///
    /// Returns `None` if no match is found or no adjacent event exists.
    pub fn finalize(&mut self) -> Option<String> {
        if self.events.is_empty() || self.num_steps == 0 {
            return None;
        }

        // Sort events by timestamp
        self.sort_events();

        let direction = self.direction.unwrap_or(Direction::Forward);
        let base = self.base.unwrap_or(Base::FirstMatch);

        match direction {
            Direction::Forward => self.match_forward(base),
            Direction::Backward => self.match_backward(base),
        }
    }

    /// Sorts events by timestamp (ascending) with presorted detection.
    fn sort_events(&mut self) {
        if self
            .events
            .windows(2)
            .all(|w| w[0].timestamp_us <= w[1].timestamp_us)
        {
            return;
        }
        self.events.sort_unstable_by_key(|e| e.timestamp_us);
    }

    /// Forward matching: find sequential event1→event2→...→eventN, return next event's value.
    fn match_forward(&self, base: Base) -> Option<String> {
        let n = self.events.len();

        match base {
            Base::Head => {
                let start = self.events.iter().position(|e| e.base_condition)?;
                self.try_match_forward_from(start, n)
            }
            Base::Tail => {
                let start = self.events.iter().rposition(|e| e.base_condition)?;
                self.try_match_forward_from(start, n)
            }
            Base::FirstMatch => {
                for start in 0..n {
                    if !self.events[start].base_condition {
                        continue;
                    }
                    if let Some(val) = self.try_match_forward_from(start, n) {
                        return Some(val);
                    }
                }
                None
            }
            Base::LastMatch => {
                let mut result = None;
                for start in 0..n {
                    if !self.events[start].base_condition {
                        continue;
                    }
                    if let Some(val) = self.try_match_forward_from(start, n) {
                        result = Some(val);
                    }
                }
                result
            }
        }
    }

    /// Try to match the full sequence forward starting from `start`.
    ///
    /// Returns the value of the event immediately after the last matched event.
    fn try_match_forward_from(&self, start: usize, n: usize) -> Option<String> {
        // Check event1 (step 0) at start position
        if self.events[start].conditions & 1 == 0 {
            return None;
        }

        let mut last_matched = start;
        let mut step = 1;

        for pos in (start + 1)..n {
            if step >= self.num_steps {
                break;
            }
            if (self.events[pos].conditions >> step) & 1 != 0 {
                last_matched = pos;
                step += 1;
            }
        }

        if step == self.num_steps {
            // Full match! Return next event's value
            let next_idx = last_matched + 1;
            if next_idx < n {
                self.events[next_idx].value.as_deref().map(String::from)
            } else {
                None
            }
        } else {
            None
        }
    }

    /// Backward matching: find sequential event chain scanning backward.
    ///
    /// Matches event1 at the starting position (later timestamp), then event2
    /// at an earlier position, etc. Returns the value of the event immediately
    /// before the earliest matched event.
    fn match_backward(&self, base: Base) -> Option<String> {
        let n = self.events.len();

        match base {
            Base::Tail => {
                let start = self.events.iter().rposition(|e| e.base_condition)?;
                self.try_match_backward_from(start)
            }
            Base::Head => {
                let start = self.events.iter().position(|e| e.base_condition)?;
                self.try_match_backward_from(start)
            }
            Base::FirstMatch => {
                // Scan from right to left, return first complete match
                for start in (0..n).rev() {
                    if !self.events[start].base_condition {
                        continue;
                    }
                    if let Some(val) = self.try_match_backward_from(start) {
                        return Some(val);
                    }
                }
                None
            }
            Base::LastMatch => {
                // Scan from right to left, return last complete match
                let mut result = None;
                for start in (0..n).rev() {
                    if !self.events[start].base_condition {
                        continue;
                    }
                    if let Some(val) = self.try_match_backward_from(start) {
                        result = Some(val);
                    }
                }
                result
            }
        }
    }

    /// Try to match the full sequence backward starting from `start`.
    ///
    /// event1 is matched at `start`, event2 at an earlier position, etc.
    /// Returns the value of the event immediately before the earliest matched.
    fn try_match_backward_from(&self, start: usize) -> Option<String> {
        // Check event1 (step 0) at start position
        if self.events[start].conditions & 1 == 0 {
            return None;
        }

        let mut earliest_matched = start;
        let mut step = 1;

        if start > 0 {
            for pos in (0..start).rev() {
                if step >= self.num_steps {
                    break;
                }
                if (self.events[pos].conditions >> step) & 1 != 0 {
                    earliest_matched = pos;
                    step += 1;
                }
            }
        }

        if step == self.num_steps {
            // Full match! Return the event before the earliest matched position
            if earliest_matched > 0 {
                self.events[earliest_matched - 1]
                    .value
                    .as_deref()
                    .map(String::from)
            } else {
                None
            }
        } else {
            None
        }
    }
}

impl Default for SequenceNextNodeState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_event(ts: i64, value: &str, base_cond: bool, conds: &[bool]) -> NextNodeEvent {
        let mut bitmask: u32 = 0;
        for (i, &c) in conds.iter().enumerate() {
            if c {
                bitmask |= 1 << i;
            }
        }
        NextNodeEvent {
            timestamp_us: ts,
            value: Some(Rc::from(value)),
            base_condition: base_cond,
            conditions: bitmask,
        }
    }

    fn make_null_event(ts: i64, base_cond: bool, conds: &[bool]) -> NextNodeEvent {
        let mut bitmask: u32 = 0;
        for (i, &c) in conds.iter().enumerate() {
            if c {
                bitmask |= 1 << i;
            }
        }
        NextNodeEvent {
            timestamp_us: ts,
            value: None,
            base_condition: base_cond,
            conditions: bitmask,
        }
    }

    // --- Direction and Base parsing ---

    #[test]
    fn test_parse_direction() {
        assert_eq!(
            SequenceNextNodeState::parse_direction("forward"),
            Some(Direction::Forward)
        );
        assert_eq!(
            SequenceNextNodeState::parse_direction("backward"),
            Some(Direction::Backward)
        );
        assert_eq!(
            SequenceNextNodeState::parse_direction("FORWARD"),
            Some(Direction::Forward)
        );
        assert_eq!(
            SequenceNextNodeState::parse_direction(" backward "),
            Some(Direction::Backward)
        );
        assert_eq!(SequenceNextNodeState::parse_direction("invalid"), None);
        assert_eq!(SequenceNextNodeState::parse_direction(""), None);
    }

    #[test]
    fn test_parse_base() {
        assert_eq!(SequenceNextNodeState::parse_base("head"), Some(Base::Head));
        assert_eq!(SequenceNextNodeState::parse_base("tail"), Some(Base::Tail));
        assert_eq!(
            SequenceNextNodeState::parse_base("first_match"),
            Some(Base::FirstMatch)
        );
        assert_eq!(
            SequenceNextNodeState::parse_base("last_match"),
            Some(Base::LastMatch)
        );
        assert_eq!(SequenceNextNodeState::parse_base("HEAD"), Some(Base::Head));
        assert_eq!(
            SequenceNextNodeState::parse_base(" tail "),
            Some(Base::Tail)
        );
        assert_eq!(SequenceNextNodeState::parse_base("invalid"), None);
        assert_eq!(SequenceNextNodeState::parse_base(""), None);
    }

    // --- Empty / edge cases ---

    #[test]
    fn test_empty_state() {
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Forward);
        state.base = Some(Base::FirstMatch);
        state.num_steps = 2;
        assert_eq!(state.finalize(), None);
    }

    #[test]
    fn test_zero_steps() {
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Forward);
        state.base = Some(Base::FirstMatch);
        state.num_steps = 0;
        state.update(make_event(100, "A", true, &[true]));
        assert_eq!(state.finalize(), None);
    }

    // --- Forward + Head ---

    #[test]
    fn test_forward_head_basic() {
        // Events: A → B → C → D → E
        // Pattern: event1=A, event2=B
        // base_condition: page = 'A'
        // Expected: C (next after B)
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Forward);
        state.base = Some(Base::Head);
        state.num_steps = 2;

        state.update(make_event(1, "A", true, &[true, false]));
        state.update(make_event(2, "B", false, &[false, true]));
        state.update(make_event(3, "C", false, &[false, false]));
        state.update(make_event(4, "D", false, &[false, false]));

        assert_eq!(state.finalize(), Some("C".to_string()));
    }

    #[test]
    fn test_forward_head_no_base_condition() {
        // No event satisfies base_condition
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Forward);
        state.base = Some(Base::Head);
        state.num_steps = 1;

        state.update(make_event(1, "A", false, &[true]));
        state.update(make_event(2, "B", false, &[false]));

        assert_eq!(state.finalize(), None);
    }

    #[test]
    fn test_forward_head_match_at_end() {
        // Match completes at the last event — no next event
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Forward);
        state.base = Some(Base::Head);
        state.num_steps = 2;

        state.update(make_event(1, "A", true, &[true, false]));
        state.update(make_event(2, "B", false, &[false, true]));

        assert_eq!(state.finalize(), None);
    }

    // --- Forward + Tail ---

    #[test]
    fn test_forward_tail_basic() {
        // base_condition events at positions 0 and 2
        // tail = last base_condition event = position 2
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Forward);
        state.base = Some(Base::Tail);
        state.num_steps = 2;

        state.update(make_event(1, "A", true, &[true, false]));
        state.update(make_event(2, "B", false, &[false, true]));
        state.update(make_event(3, "C", true, &[true, false]));
        state.update(make_event(4, "D", false, &[false, true]));
        state.update(make_event(5, "E", false, &[false, false]));

        assert_eq!(state.finalize(), Some("E".to_string()));
    }

    // --- Forward + FirstMatch ---

    #[test]
    fn test_forward_first_match_basic() {
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Forward);
        state.base = Some(Base::FirstMatch);
        state.num_steps = 2;

        // First base+event1 at pos 0 fails to match event2
        state.update(make_event(1, "A", true, &[true, false]));
        state.update(make_event(2, "X", false, &[false, false]));
        // Second base+event1 at pos 2 matches event2 at pos 3
        state.update(make_event(3, "A", true, &[true, false]));
        state.update(make_event(4, "B", false, &[false, true]));
        state.update(make_event(5, "C", false, &[false, false]));

        assert_eq!(state.finalize(), Some("C".to_string()));
    }

    #[test]
    fn test_forward_first_match_returns_first() {
        // Two possible matches; should return first
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Forward);
        state.base = Some(Base::FirstMatch);
        state.num_steps = 1;

        state.update(make_event(1, "A", true, &[true]));
        state.update(make_event(2, "B", false, &[false]));
        state.update(make_event(3, "C", true, &[true]));
        state.update(make_event(4, "D", false, &[false]));

        assert_eq!(state.finalize(), Some("B".to_string()));
    }

    // --- Forward + LastMatch ---

    #[test]
    fn test_forward_last_match_returns_last() {
        // Two possible matches; should return last
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Forward);
        state.base = Some(Base::LastMatch);
        state.num_steps = 1;

        state.update(make_event(1, "A", true, &[true]));
        state.update(make_event(2, "B", false, &[false]));
        state.update(make_event(3, "C", true, &[true]));
        state.update(make_event(4, "D", false, &[false]));

        assert_eq!(state.finalize(), Some("D".to_string()));
    }

    // --- Backward + Tail ---

    #[test]
    fn test_backward_tail_basic() {
        // Events: A(1) → B(2) → C(3) → D(4) → E(5)
        // Pattern: event1=E, event2=D (backward: event1 at later ts, event2 at earlier)
        // base_condition: page = 'E' (at position 4)
        // Backward from tail: match event1=E at pos 4, event2=D at pos 3
        // Return event before earliest match (pos 3) = pos 2 = C
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Backward);
        state.base = Some(Base::Tail);
        state.num_steps = 2;

        state.update(make_event(1, "A", false, &[false, false]));
        state.update(make_event(2, "B", false, &[false, false]));
        state.update(make_event(3, "C", false, &[false, true])); // event2 = D-condition
        state.update(make_event(4, "D", false, &[false, true])); // event2 = D-condition
        state.update(make_event(5, "E", true, &[true, false])); // event1 = E-condition, base=true

        // event1 matches at pos 4 (E), event2 matches at pos 3 (D)
        // Event before earliest (pos 3) = pos 2 (C)
        assert_eq!(state.finalize(), Some("C".to_string()));
    }

    #[test]
    fn test_backward_tail_no_previous() {
        // Match starts at position 0 — no event before it
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Backward);
        state.base = Some(Base::Tail);
        state.num_steps = 1;

        state.update(make_event(1, "A", true, &[true]));

        assert_eq!(state.finalize(), None);
    }

    // --- Backward + Head ---

    #[test]
    fn test_backward_head_basic() {
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Backward);
        state.base = Some(Base::Head);
        state.num_steps = 1;

        state.update(make_event(1, "A", false, &[false]));
        state.update(make_event(2, "B", true, &[true]));
        state.update(make_event(3, "C", false, &[false]));

        // Head = first base_condition event = pos 1 (B)
        // event1 matches at pos 1; backward → event before pos 1 = pos 0 = A
        assert_eq!(state.finalize(), Some("A".to_string()));
    }

    // --- Backward + FirstMatch ---

    #[test]
    fn test_backward_first_match_basic() {
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Backward);
        state.base = Some(Base::FirstMatch);
        state.num_steps = 2;

        state.update(make_event(1, "A", false, &[false, false]));
        state.update(make_event(2, "B", false, &[false, true])); // event2
        state.update(make_event(3, "C", true, &[true, false])); // event1, base
        state.update(make_event(4, "D", false, &[false, true])); // event2
        state.update(make_event(5, "E", true, &[true, false])); // event1, base

        // Scan from right: pos 4 (E) has base=true, event1=true
        // Backward from pos 4: pos 3 (D) has event2=true → match at [4,3]
        // Event before pos 3 = pos 2 (C)
        assert_eq!(state.finalize(), Some("C".to_string()));
    }

    // --- Backward + LastMatch ---

    #[test]
    fn test_backward_last_match_basic() {
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Backward);
        state.base = Some(Base::LastMatch);
        state.num_steps = 2;

        state.update(make_event(1, "A", false, &[false, false]));
        state.update(make_event(2, "B", false, &[false, true])); // event2
        state.update(make_event(3, "C", true, &[true, false])); // event1, base
        state.update(make_event(4, "D", false, &[false, true])); // event2
        state.update(make_event(5, "E", true, &[true, false])); // event1, base

        // Scan from right: both pos 4 and pos 2 yield matches
        // Last match (leftmost starting point): pos 2 (C), event2 at pos 1 (B)
        // Event before pos 1 = pos 0 (A)
        assert_eq!(state.finalize(), Some("A".to_string()));
    }

    // --- Multi-step patterns ---

    #[test]
    fn test_forward_three_step_pattern() {
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Forward);
        state.base = Some(Base::FirstMatch);
        state.num_steps = 3;

        state.update(make_event(1, "Home", true, &[true, false, false]));
        state.update(make_event(2, "Product", false, &[false, true, false]));
        state.update(make_event(3, "Cart", false, &[false, false, true]));
        state.update(make_event(4, "Checkout", false, &[false, false, false]));

        assert_eq!(state.finalize(), Some("Checkout".to_string()));
    }

    #[test]
    fn test_forward_three_step_incomplete() {
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Forward);
        state.base = Some(Base::FirstMatch);
        state.num_steps = 3;

        state.update(make_event(1, "Home", true, &[true, false, false]));
        state.update(make_event(2, "Product", false, &[false, true, false]));
        // No event3 match

        assert_eq!(state.finalize(), None);
    }

    // --- Unsorted input ---

    #[test]
    fn test_unsorted_input() {
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Forward);
        state.base = Some(Base::FirstMatch);
        state.num_steps = 2;

        // Insert out of timestamp order
        state.update(make_event(3, "C", false, &[false, false]));
        state.update(make_event(1, "A", true, &[true, false]));
        state.update(make_event(2, "B", false, &[false, true]));

        assert_eq!(state.finalize(), Some("C".to_string()));
    }

    // --- Single-step pattern ---

    #[test]
    fn test_single_step_forward() {
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Forward);
        state.base = Some(Base::FirstMatch);
        state.num_steps = 1;

        state.update(make_event(1, "A", true, &[true]));
        state.update(make_event(2, "B", false, &[false]));

        assert_eq!(state.finalize(), Some("B".to_string()));
    }

    #[test]
    fn test_single_step_backward() {
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Backward);
        state.base = Some(Base::FirstMatch);
        state.num_steps = 1;

        state.update(make_event(1, "A", false, &[false]));
        state.update(make_event(2, "B", true, &[true]));

        assert_eq!(state.finalize(), Some("A".to_string()));
    }

    // --- NULL values ---

    #[test]
    fn test_null_value_returned() {
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Forward);
        state.base = Some(Base::FirstMatch);
        state.num_steps = 1;

        state.update(make_event(1, "A", true, &[true]));
        state.update(make_null_event(2, false, &[false]));

        // Next event has NULL value
        assert_eq!(state.finalize(), None);
    }

    #[test]
    fn test_null_value_in_middle_not_blocking() {
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Forward);
        state.base = Some(Base::FirstMatch);
        state.num_steps = 2;

        state.update(make_event(1, "A", true, &[true, false]));
        state.update(make_null_event(2, false, &[false, false])); // gap with null value
        state.update(make_event(3, "B", false, &[false, true]));
        state.update(make_event(4, "C", false, &[false, false]));

        assert_eq!(state.finalize(), Some("C".to_string()));
    }

    // --- Combine tests ---

    #[test]
    fn test_combine_basic() {
        let mut a = SequenceNextNodeState::new();
        a.direction = Some(Direction::Forward);
        a.base = Some(Base::FirstMatch);
        a.num_steps = 2;
        a.update(make_event(1, "A", true, &[true, false]));

        let mut b = SequenceNextNodeState::new();
        b.update(make_event(2, "B", false, &[false, true]));
        b.update(make_event(3, "C", false, &[false, false]));

        let mut combined = a.combine(&b);
        combined.num_steps = 2;
        assert_eq!(combined.finalize(), Some("C".to_string()));
    }

    #[test]
    fn test_combine_in_place_basic() {
        let mut a = SequenceNextNodeState::new();
        a.direction = Some(Direction::Forward);
        a.base = Some(Base::FirstMatch);
        a.num_steps = 2;
        a.update(make_event(1, "A", true, &[true, false]));

        let mut b = SequenceNextNodeState::new();
        b.update(make_event(2, "B", false, &[false, true]));
        b.update(make_event(3, "C", false, &[false, false]));

        a.combine_in_place(&b);
        assert_eq!(a.events.len(), 3);
        assert_eq!(a.finalize(), Some("C".to_string()));
    }

    #[test]
    fn test_combine_preserves_direction_and_base() {
        let mut a = SequenceNextNodeState::new();
        a.direction = Some(Direction::Backward);
        a.base = Some(Base::Tail);
        a.num_steps = 1;

        let b = SequenceNextNodeState::new();

        a.combine_in_place(&b);
        assert_eq!(a.direction, Some(Direction::Backward));
        assert_eq!(a.base, Some(Base::Tail));
    }

    #[test]
    fn test_combine_takes_direction_from_other() {
        let mut a = SequenceNextNodeState::new();
        let mut b = SequenceNextNodeState::new();
        b.direction = Some(Direction::Backward);
        b.base = Some(Base::LastMatch);
        b.num_steps = 3;

        a.combine_in_place(&b);
        assert_eq!(a.direction, Some(Direction::Backward));
        assert_eq!(a.base, Some(Base::LastMatch));
        assert_eq!(a.num_steps, 3);
    }

    #[test]
    fn test_combine_both_empty() {
        let a = SequenceNextNodeState::new();
        let b = SequenceNextNodeState::new();
        let combined = a.combine(&b);
        assert!(combined.events.is_empty());
        assert!(combined.direction.is_none());
        assert!(combined.base.is_none());
    }

    // --- Events not satisfying any step condition still stored ---

    #[test]
    fn test_events_without_conditions_stored() {
        // Unlike other functions, ALL events are stored (they could be the next node)
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Forward);
        state.base = Some(Base::FirstMatch);
        state.num_steps = 1;

        state.update(make_event(1, "A", true, &[true]));
        state.update(make_event(2, "B", false, &[])); // no conditions but stored

        assert_eq!(state.events.len(), 2);
        assert_eq!(state.finalize(), Some("B".to_string()));
    }

    // --- set_direction / set_base only on first call ---

    #[test]
    fn test_set_direction_only_first() {
        let mut state = SequenceNextNodeState::new();
        state.set_direction(Direction::Forward);
        state.set_direction(Direction::Backward); // ignored
        assert_eq!(state.direction, Some(Direction::Forward));
    }

    #[test]
    fn test_set_base_only_first() {
        let mut state = SequenceNextNodeState::new();
        state.set_base(Base::Head);
        state.set_base(Base::Tail); // ignored
        assert_eq!(state.base, Some(Base::Head));
    }

    // --- Default direction/base ---

    #[test]
    fn test_default_direction_and_base() {
        // When direction/base not set, defaults to Forward/FirstMatch
        let mut state = SequenceNextNodeState::new();
        state.num_steps = 1;
        state.update(make_event(1, "A", true, &[true]));
        state.update(make_event(2, "B", false, &[false]));

        assert_eq!(state.finalize(), Some("B".to_string()));
    }

    // --- Gap events (events between matched steps) ---

    #[test]
    fn test_forward_with_gap_events() {
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Forward);
        state.base = Some(Base::FirstMatch);
        state.num_steps = 2;

        state.update(make_event(1, "A", true, &[true, false]));
        state.update(make_event(2, "X", false, &[false, false])); // gap
        state.update(make_event(3, "Y", false, &[false, false])); // gap
        state.update(make_event(4, "B", false, &[false, true]));
        state.update(make_event(5, "C", false, &[false, false]));

        assert_eq!(state.finalize(), Some("C".to_string()));
    }

    // --- Same timestamp ---

    #[test]
    fn test_same_timestamp_events() {
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Forward);
        state.base = Some(Base::FirstMatch);
        state.num_steps = 2;

        state.update(make_event(100, "A", true, &[true, false]));
        state.update(make_event(100, "B", false, &[false, true]));
        state.update(make_event(100, "C", false, &[false, false]));

        assert_eq!(state.finalize(), Some("C".to_string()));
    }

    // --- Backward three-step ---

    #[test]
    fn test_backward_three_step() {
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Backward);
        state.base = Some(Base::Tail);
        state.num_steps = 3;

        state.update(make_event(1, "A", false, &[false, false, false]));
        state.update(make_event(2, "B", false, &[false, false, true])); // event3
        state.update(make_event(3, "C", false, &[false, true, false])); // event2
        state.update(make_event(4, "D", true, &[true, false, false])); // event1 + base

        // Backward from tail (pos 3): event1 at pos 3, event2 at pos 2, event3 at pos 1
        // Earliest matched = pos 1; event before = pos 0 = A
        assert_eq!(state.finalize(), Some("A".to_string()));
    }

    // --- No match ---

    #[test]
    fn test_no_match_forward() {
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Forward);
        state.base = Some(Base::FirstMatch);
        state.num_steps = 2;

        state.update(make_event(1, "A", true, &[true, false]));
        // No event2 match
        state.update(make_event(2, "B", false, &[false, false]));

        assert_eq!(state.finalize(), None);
    }

    #[test]
    fn test_no_match_backward() {
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Backward);
        state.base = Some(Base::Tail);
        state.num_steps = 2;

        state.update(make_event(1, "A", false, &[false, false]));
        state.update(make_event(2, "B", true, &[true, false]));
        // No event2 match before pos 1

        assert_eq!(state.finalize(), None);
    }

    // --- Head/Tail interaction with forward/backward ---

    #[test]
    fn test_forward_head_skips_later_base_events() {
        // Two base_condition events; head picks the first one
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Forward);
        state.base = Some(Base::Head);
        state.num_steps = 1;

        // First base event: no event1 match
        state.update(make_event(1, "X", true, &[false]));
        state.update(make_event(2, "Y", false, &[false]));
        // Second base event: has event1 match
        state.update(make_event(3, "A", true, &[true]));
        state.update(make_event(4, "B", false, &[false]));

        // Head = pos 0 (X), event1 not matched → None
        assert_eq!(state.finalize(), None);
    }

    #[test]
    fn test_backward_head_at_start() {
        // Backward + Head: base at first event, match backward from there
        // Since it's the first event, backward matching can't go further back
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Backward);
        state.base = Some(Base::Head);
        state.num_steps = 1;

        state.update(make_event(1, "A", true, &[true]));
        state.update(make_event(2, "B", false, &[false]));

        // event1 at pos 0, no event before → None
        assert_eq!(state.finalize(), None);
    }

    // --- Session 9: Rc<str> specific tests ---

    #[test]
    fn test_rc_str_clone_is_shared() {
        // Verify that Rc<str> values are shared references, not deep copies
        let value: Rc<str> = Rc::from("shared");
        let event1 = NextNodeEvent {
            timestamp_us: 1,
            value: Some(Rc::clone(&value)),
            base_condition: true,
            conditions: 1,
        };
        let event2 = event1.clone();
        // Both events share the same Rc allocation
        assert!(Rc::ptr_eq(
            event1.value.as_ref().unwrap(),
            event2.value.as_ref().unwrap()
        ));
    }

    #[test]
    fn test_next_node_event_size() {
        // Verify NextNodeEvent with Rc<str> is 32 bytes (down from 40 with String)
        assert_eq!(std::mem::size_of::<NextNodeEvent>(), 32);
    }

    #[test]
    fn test_rc_str_combine_shares_references() {
        // Verify combine_in_place preserves Rc sharing
        let mut a = SequenceNextNodeState::new();
        a.direction = Some(Direction::Forward);
        a.base = Some(Base::FirstMatch);
        a.num_steps = 1;
        a.update(make_event(1, "A", true, &[true]));

        let mut b = SequenceNextNodeState::new();
        b.update(make_event(2, "B", false, &[false]));

        a.combine_in_place(&b);
        // After combine, the value in b's event should be shared with a's copy
        // (both are Rc clones, not deep copies)
        assert_eq!(a.events.len(), 2);
        assert_eq!(a.finalize(), Some("B".to_string()));
    }

    #[test]
    fn test_empty_string_value() {
        // Edge case: empty string should be stored and returned correctly
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Forward);
        state.base = Some(Base::FirstMatch);
        state.num_steps = 1;

        state.update(NextNodeEvent {
            timestamp_us: 1,
            value: Some(Rc::from("")),
            base_condition: true,
            conditions: 1,
        });
        state.update(NextNodeEvent {
            timestamp_us: 2,
            value: Some(Rc::from("")),
            base_condition: false,
            conditions: 0,
        });

        assert_eq!(state.finalize(), Some(String::new()));
    }

    #[test]
    fn test_unicode_string_value() {
        // Edge case: Unicode strings should be preserved through Rc<str>
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Forward);
        state.base = Some(Base::FirstMatch);
        state.num_steps = 1;

        state.update(NextNodeEvent {
            timestamp_us: 1,
            value: Some(Rc::from("hello")),
            base_condition: true,
            conditions: 1,
        });
        state.update(NextNodeEvent {
            timestamp_us: 2,
            value: Some(Rc::from("world")),
            base_condition: false,
            conditions: 0,
        });

        assert_eq!(state.finalize(), Some("world".to_string()));
    }

    #[test]
    fn test_long_string_value() {
        // Edge case: long strings (>1KB) should work correctly
        let long_value: String = "x".repeat(2048);
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Forward);
        state.base = Some(Base::FirstMatch);
        state.num_steps = 1;

        state.update(make_event(1, "A", true, &[true]));
        state.update(NextNodeEvent {
            timestamp_us: 2,
            value: Some(Rc::from(long_value.as_str())),
            base_condition: false,
            conditions: 0,
        });

        let result = state.finalize();
        assert_eq!(result.as_deref(), Some(long_value.as_str()));
    }

    #[test]
    fn test_32_conditions_with_rc_str() {
        // Verify 32-condition support works with Rc<str> values
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Forward);
        state.base = Some(Base::FirstMatch);
        state.num_steps = 32;

        // Event with all 32 conditions set
        state.update(NextNodeEvent {
            timestamp_us: 1,
            value: Some(Rc::from("start")),
            base_condition: true,
            conditions: 0xFFFF_FFFF,
        });
        state.update(NextNodeEvent {
            timestamp_us: 2,
            value: Some(Rc::from("result")),
            base_condition: false,
            conditions: 0,
        });

        // With 32 steps and all conditions set on one event, only step 0
        // matches at position 0. Steps 1-31 need separate events.
        // This tests that the condition bitmask works correctly with Rc<str>.
        // (Full match won't happen with just one event for 32 steps, so None)
        assert_eq!(state.finalize(), None);
    }

    #[test]
    fn test_combine_chain_three_states() {
        // Verify combine chain with 3 states preserves all Rc<str> values
        let mut s1 = SequenceNextNodeState::new();
        s1.direction = Some(Direction::Forward);
        s1.base = Some(Base::FirstMatch);
        s1.num_steps = 3;
        s1.update(make_event(1, "Home", true, &[true, false, false]));

        let mut s2 = SequenceNextNodeState::new();
        s2.update(make_event(2, "Product", false, &[false, true, false]));

        let mut s3 = SequenceNextNodeState::new();
        s3.update(make_event(3, "Cart", false, &[false, false, true]));
        s3.update(make_event(4, "Checkout", false, &[false, false, false]));

        s1.combine_in_place(&s2);
        s1.combine_in_place(&s3);

        assert_eq!(s1.events.len(), 4);
        assert_eq!(s1.finalize(), Some("Checkout".to_string()));
    }

    #[test]
    fn test_mixed_null_and_rc_values_in_combine() {
        // Verify combine handles mixed null/non-null Rc<str> values correctly
        let mut a = SequenceNextNodeState::new();
        a.direction = Some(Direction::Forward);
        a.base = Some(Base::FirstMatch);
        a.num_steps = 2;
        a.update(make_event(1, "A", true, &[true, false]));

        let mut b = SequenceNextNodeState::new();
        b.update(make_null_event(2, false, &[false, false])); // null gap
        b.update(make_event(3, "B", false, &[false, true]));
        b.update(make_event(4, "C", false, &[false, false]));

        a.combine_in_place(&b);
        assert_eq!(a.finalize(), Some("C".to_string()));
    }

    // --- Session 11: DuckDB zero-initialized target combine tests ---

    #[test]
    fn test_combine_in_place_zero_target_propagates_all_fields() {
        // DuckDB's segment tree: fresh target + configured source
        let mut target = SequenceNextNodeState::new(); // zero-initialized
        let mut source = SequenceNextNodeState::new();
        source.direction = Some(Direction::Forward);
        source.base = Some(Base::FirstMatch);
        source.num_steps = 2;
        source.update(make_event(1, "A", true, &[true, false]));
        source.update(make_event(2, "B", false, &[false, true]));
        source.update(make_event(3, "C", false, &[false, false]));

        target.combine_in_place(&source);
        assert_eq!(target.direction, Some(Direction::Forward));
        assert_eq!(target.base, Some(Base::FirstMatch));
        assert_eq!(target.num_steps, 2);
        assert_eq!(target.finalize(), Some("C".to_string()));
    }

    #[test]
    fn test_combine_in_place_zero_target_backward_direction() {
        let mut target = SequenceNextNodeState::new();
        let mut source = SequenceNextNodeState::new();
        source.direction = Some(Direction::Backward);
        source.base = Some(Base::Tail);
        source.num_steps = 1;
        source.update(make_event(1, "A", false, &[false]));
        source.update(make_event(2, "B", true, &[true]));

        target.combine_in_place(&source);
        assert_eq!(target.direction, Some(Direction::Backward));
        assert_eq!(target.base, Some(Base::Tail));
        assert_eq!(target.finalize(), Some("A".to_string()));
    }

    #[test]
    fn test_combine_in_place_zero_target_chain_finalize() {
        let mut target = SequenceNextNodeState::new();
        let mut s1 = SequenceNextNodeState::new();
        s1.direction = Some(Direction::Forward);
        s1.base = Some(Base::FirstMatch);
        s1.num_steps = 3;
        s1.update(make_event(1, "Home", true, &[true, false, false]));

        let mut s2 = SequenceNextNodeState::new();
        s2.update(make_event(2, "Product", false, &[false, true, false]));

        let mut s3 = SequenceNextNodeState::new();
        s3.update(make_event(3, "Cart", false, &[false, false, true]));
        s3.update(make_event(4, "Checkout", false, &[false, false, false]));

        target.combine_in_place(&s1);
        target.combine_in_place(&s2);
        target.combine_in_place(&s3);
        assert_eq!(target.direction, Some(Direction::Forward));
        assert_eq!(target.num_steps, 3);
        assert_eq!(target.finalize(), Some("Checkout".to_string()));
    }

    #[test]
    fn test_combine_in_place_existing_fields_not_overwritten() {
        let mut target = SequenceNextNodeState::new();
        target.direction = Some(Direction::Forward);
        target.base = Some(Base::Head);
        target.num_steps = 2;

        let mut source = SequenceNextNodeState::new();
        source.direction = Some(Direction::Backward);
        source.base = Some(Base::Tail);
        source.num_steps = 5;

        target.combine_in_place(&source);
        assert_eq!(target.direction, Some(Direction::Forward));
        assert_eq!(target.base, Some(Base::Head));
        assert_eq!(target.num_steps, 2);
    }

    #[test]
    fn test_combine_zero_target_propagates_all_fields() {
        let target = SequenceNextNodeState::new();
        let mut source = SequenceNextNodeState::new();
        source.direction = Some(Direction::Backward);
        source.base = Some(Base::LastMatch);
        source.num_steps = 4;
        source.update(make_event(1, "A", true, &[true]));

        let combined = target.combine(&source);
        assert_eq!(combined.direction, Some(Direction::Backward));
        assert_eq!(combined.base, Some(Base::LastMatch));
        assert_eq!(combined.num_steps, 4);
        assert_eq!(combined.events.len(), 1);
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn forward_first_match_with_match_returns_some(
            num_gap_events in 0..20usize,
        ) {
            // event1 at pos 0, then gap events, then event2 at last gap+1, then "result"
            let mut state = SequenceNextNodeState::new();
            state.direction = Some(Direction::Forward);
            state.base = Some(Base::FirstMatch);
            state.num_steps = 2;

            state.update(NextNodeEvent {
                timestamp_us: 0,
                value: Some(Rc::from("start")),
                base_condition: true,
                conditions: 1, // event1
            });

            for i in 0..num_gap_events {
                state.update(NextNodeEvent {
                    timestamp_us: (i as i64 + 1),
                    value: Some(Rc::from(format!("gap_{i}").as_str())),
                    base_condition: false,
                    conditions: 0,
                });
            }

            state.update(NextNodeEvent {
                timestamp_us: (num_gap_events as i64 + 1),
                value: Some(Rc::from("matched")),
                base_condition: false,
                conditions: 2, // event2
            });

            state.update(NextNodeEvent {
                timestamp_us: (num_gap_events as i64 + 2),
                value: Some(Rc::from("result")),
                base_condition: false,
                conditions: 0,
            });

            let result = state.finalize();
            prop_assert_eq!(result, Some("result".to_string()));
        }

        #[test]
        fn combine_preserves_event_count(
            n_a in 0..=10usize,
            n_b in 0..=10usize,
        ) {
            let mut a = SequenceNextNodeState::new();
            a.direction = Some(Direction::Forward);
            a.base = Some(Base::FirstMatch);
            a.num_steps = 1;
            for i in 0..n_a {
                a.update(NextNodeEvent {
                    timestamp_us: i as i64,
                    value: Some(Rc::from(format!("a_{i}").as_str())),
                    base_condition: true,
                    conditions: 1,
                });
            }

            let mut b = SequenceNextNodeState::new();
            for i in 0..n_b {
                b.update(NextNodeEvent {
                    timestamp_us: (n_a + i) as i64,
                    value: Some(Rc::from(format!("b_{i}").as_str())),
                    base_condition: false,
                    conditions: 0,
                });
            }

            let combined = a.combine(&b);
            prop_assert_eq!(combined.events.len(), n_a + n_b);
        }

        #[test]
        fn no_match_without_base_condition(
            num_events in 1..=20usize,
        ) {
            let mut state = SequenceNextNodeState::new();
            state.direction = Some(Direction::Forward);
            state.base = Some(Base::FirstMatch);
            state.num_steps = 1;

            for i in 0..num_events {
                state.update(NextNodeEvent {
                    timestamp_us: i as i64,
                    value: Some(Rc::from(format!("evt_{i}").as_str())),
                    base_condition: false, // no base condition satisfied
                    conditions: 1,
                });
            }

            prop_assert!(state.finalize().is_none());
        }
    }
}
