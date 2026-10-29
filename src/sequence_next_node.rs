// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

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
//! Mirrors `ClickHouse`'s `sequenceNextNode` exactly (verified against
//! `AggregateFunctionSequenceNextNode.cpp`):
//!
//! 1. Events are sorted by `(timestamp, value, base_condition, conditions)` —
//!    the value tie-break matches `ClickHouse`'s comparator, and the rest make
//!    the order total, so results are deterministic under parallel combines.
//! 2. A single base (anchor) index is selected per [`Base`]: `head`/`tail`
//!    are the literal first/last events (which must satisfy the base
//!    condition), `first_match`/`last_match` scan for an event satisfying
//!    both the base condition and `event1`.
//! 3. The chain must match **consecutive** events: `eventK` at `base + k`
//!    (forward) or `base - k` (backward). Interleaved non-matching events
//!    break the chain, and a failed chain is NOT retried at other anchors.
//! 4. On a full match the value at `base ± num_steps` is returned; `NULL`
//!    when that index falls off either end.
//!
//! # Value storage
//!
//! Every event keeps its `VARCHAR` value, since any event can be the one
//! returned. A state stores the bytes of all its events' values back to back
//! in one buffer (`values`) and each event holds a `(start, len)` range into
//! it, so an event is a 24-byte `Copy` record and a group makes two
//! allocations however many events it has. The buffer and the event list grow
//! with `try_reserve`, so memory exhaustion is a reported error, not an abort.
//!
//! This replaced one `Arc<str>` per event (one heap allocation each: about 64
//! bytes of allocator overhead and header for a 10-byte page name, an abort
//! when `malloc` failed, and one leaked allocation per event when `DuckDB`
//! abandons a grouped scan without destroying its states). The earlier "string
//! pool" experiment in `PERF.md` that lost to `Arc<str>` kept one `String`
//! per entry; a byte buffer has no per-value allocation to pay for.

use std::sync::Arc;

/// Direction of traversal for sequence matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Match the chain at ascending positions from the anchor. Returns the
    /// value of the event immediately after the chain.
    Forward,
    /// Match the chain at descending positions from the anchor. Returns the
    /// value of the event immediately before the chain.
    Backward,
}

/// Base position for starting the sequence match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Base {
    /// Anchor at the literal first event (chronologically). That event must
    /// itself satisfy `base_condition`, otherwise there is no match.
    Head,
    /// Anchor at the literal last event (chronologically). That event must
    /// itself satisfy `base_condition`, otherwise there is no match.
    Tail,
    /// Anchor at the first event satisfying both `base_condition` and `event1`.
    FirstMatch,
    /// Anchor at the last event satisfying both `base_condition` and `event1`.
    LastMatch,
}

/// A timestamped event with a string value, as one input row.
///
/// [`SequenceNextNodeState::update`] takes this convenient owned form (tests
/// and benchmarks build it); the aggregate itself calls
/// [`SequenceNextNodeState::try_push`] with a borrowed `&str` and allocates
/// nothing per event.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct NextNodeEvent {
    /// Timestamp in microseconds since Unix epoch.
    pub timestamp_us: i64,
    /// The event column value (candidate return value); `None` for `NULL`.
    pub value: Option<Arc<str>>,
    /// Whether the base condition is satisfied for this event.
    pub base_condition: bool,
    /// Bitmask of which sequential event conditions this event satisfies.
    /// Bit `i` is set if this event matches event `i+1` (1-indexed) in the chain.
    pub conditions: u32,
}

impl NextNodeEvent {
    /// Creates a new event with the given timestamp, value, base condition, and conditions bitmask.
    #[must_use]
    pub fn new(
        timestamp_us: i64,
        value: Option<Arc<str>>,
        base_condition: bool,
        conditions: u32,
    ) -> Self {
        Self {
            timestamp_us,
            value,
            base_condition,
            conditions,
        }
    }
}

/// Why a state could not store another event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreError {
    /// The allocator could not grow the event list or the value buffer.
    OutOfMemory,
    /// The group's values need more than `u32::MAX` bytes (4 GiB) of buffer.
    TooLarge,
}

/// `len` marking a `NULL` value (a real value is at most `u32::MAX - 1` bytes).
const NO_VALUE: u32 = u32::MAX;

/// One stored event. Its value, if any, is `values[start..start + len]`.
#[derive(Debug, Clone, Copy)]
struct StoredEvent {
    timestamp_us: i64,
    start: u32,
    len: u32,
    conditions: u32,
    base_condition: bool,
}

impl StoredEvent {
    const fn has_value(self) -> bool {
        self.len != NO_VALUE
    }
}

/// State for the `sequence_next_node` aggregate function.
///
/// Collects events with string values during `update`, then performs
/// sequential matching during `finalize` to find the next event value
/// after a completed sequence match.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SequenceNextNodeState {
    /// Collected events. Sorted by `(timestamp, value, ..)` in finalize.
    events: Vec<StoredEvent>,
    /// The events' values, back to back (see the module docs).
    values: Vec<u8>,
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
            values: Vec::new(),
            direction: None,
            base: None,
            num_steps: 0,
        }
    }

    /// Number of collected events.
    #[must_use]
    pub fn event_count(&self) -> usize {
        self.events.len()
    }

    /// Whether no event has been collected.
    #[must_use]
    pub fn has_no_events(&self) -> bool {
        self.events.is_empty()
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

    /// The bytes of `event`'s value, or `None` for a `NULL` value.
    fn value_of(&self, event: StoredEvent) -> Option<&[u8]> {
        event
            .has_value()
            .then(|| &self.values[event.start as usize..][..event.len as usize])
    }

    /// Adds an event to the state.
    ///
    /// All events are stored regardless of conditions because any event could
    /// be the "next node" whose value is returned. A value equal to the
    /// previous event's is stored once.
    ///
    /// # Errors
    ///
    /// [`StoreError`] if the event list or the value buffer cannot grow.
    pub fn try_push(
        &mut self,
        timestamp_us: i64,
        value: Option<&str>,
        base_condition: bool,
        conditions: u32,
    ) -> Result<(), StoreError> {
        self.events
            .try_reserve(1)
            .map_err(|_| StoreError::OutOfMemory)?;
        let (start, len) = match value {
            None => (0, NO_VALUE),
            Some(text) => match self.events.last().copied() {
                // Repeated values are common (a page visited again, a status
                // that holds): share the previous event's bytes.
                Some(previous) if self.value_of(previous) == Some(text.as_bytes()) => {
                    (previous.start, previous.len)
                }
                _ => {
                    let len = u32::try_from(text.len()).map_err(|_| StoreError::TooLarge)?;
                    let start =
                        u32::try_from(self.values.len()).map_err(|_| StoreError::TooLarge)?;
                    if len == NO_VALUE || start.checked_add(len).is_none() {
                        return Err(StoreError::TooLarge);
                    }
                    self.values
                        .try_reserve(text.len())
                        .map_err(|_| StoreError::OutOfMemory)?;
                    self.values.extend_from_slice(text.as_bytes());
                    (start, len)
                }
            },
        };
        self.events.push(StoredEvent {
            timestamp_us,
            start,
            len,
            conditions,
            base_condition,
        });
        Ok(())
    }

    /// Adds an event given as a [`NextNodeEvent`].
    ///
    /// # Panics
    ///
    /// If the state cannot grow; the aggregate uses [`try_push`](Self::try_push)
    /// and reports that as a SQL error instead.
    #[allow(clippy::needless_pass_by_value)] // the owned row is the convenient form for callers
    pub fn update(&mut self, event: NextNodeEvent) {
        self.try_push(
            event.timestamp_us,
            event.value.as_deref(),
            event.base_condition,
            event.conditions,
        )
        .expect("sequence_next_node state could not grow");
    }

    /// Combines two states by concatenating their event lists, returning a new state.
    ///
    /// # Panics
    ///
    /// If the combined state cannot grow (see [`update`](Self::update)).
    #[must_use]
    pub fn combine(&self, other: &Self) -> Self {
        let mut combined = self.clone();
        combined.combine_in_place(other);
        combined
    }

    /// Combines another state into `self` in-place by appending its events.
    ///
    /// Preferred for sequential (left-fold) chains. Uses Vec's doubling growth
    /// strategy for O(N) amortized total copies.
    ///
    /// # Panics
    ///
    /// If `self` cannot grow; the aggregate uses
    /// [`try_combine_in_place`](Self::try_combine_in_place).
    pub fn combine_in_place(&mut self, other: &Self) {
        self.try_combine_in_place(other)
            .expect("sequence_next_node state could not grow");
    }

    /// Like [`combine_in_place`](Self::combine_in_place), but reports a
    /// failure to grow instead of panicking. `self` is unchanged on error
    /// (its configuration is propagated only once the events are stored).
    ///
    /// # Errors
    ///
    /// [`StoreError`] if the event list or the value buffer cannot grow.
    pub fn try_combine_in_place(&mut self, other: &Self) -> Result<(), StoreError> {
        // `other`'s values are appended after `self`'s, so its events' ranges
        // move by the buffer length `self` has now.
        let shift = u32::try_from(self.values.len()).map_err(|_| StoreError::TooLarge)?;
        // The combined buffer must stay addressable by a `u32` offset.
        u32::try_from(other.values.len())
            .ok()
            .and_then(|extra| shift.checked_add(extra))
            .ok_or(StoreError::TooLarge)?;
        self.events
            .try_reserve(other.events.len())
            .map_err(|_| StoreError::OutOfMemory)?;
        self.values
            .try_reserve(other.values.len())
            .map_err(|_| StoreError::OutOfMemory)?;
        self.values.extend_from_slice(&other.values);
        self.events
            .extend(other.events.iter().map(|&event| StoredEvent {
                start: if event.has_value() {
                    event.start + shift
                } else {
                    0
                },
                ..event
            }));
        if self.direction.is_none() {
            self.direction = other.direction;
        }
        if self.base.is_none() {
            self.base = other.base;
        }
        if self.num_steps == 0 {
            self.num_steps = other.num_steps;
        }
        Ok(())
    }

    /// Executes the sequence matching and returns the next node's value.
    ///
    /// Mirrors `ClickHouse`'s `sequenceNextNode` exactly: a single base
    /// (anchor) index is selected per [`Base`], the event chain must match at
    /// **consecutive** sorted positions starting at the anchor (`eventK` at
    /// `base ± k`), and the returned value is the node immediately after
    /// (forward) / before (backward) the chain. If the anchor's chain does not
    /// match, the result is `None` — there is no retry at other anchors.
    ///
    /// Returns `None` if no match is found or no adjacent event exists.
    pub fn finalize(&mut self) -> Option<String> {
        if self.events.is_empty() || self.num_steps == 0 {
            return None;
        }

        // Sort events by (timestamp, value)
        self.sort_events();

        let direction = self.direction.unwrap_or(Direction::Forward);
        let base = self.base.unwrap_or(Base::FirstMatch);

        let base_idx = self.base_index(base)?;
        self.next_node_value(base_idx, direction)
    }

    /// Sorts events by `(timestamp, value, base_condition, conditions)`
    /// ascending, with presorted detection.
    ///
    /// `ClickHouse` stable-sorts by `(timestamp, value)` and leaves events
    /// equal on both in arrival order, so its result can change with row
    /// order and thread count. Breaking those ties by the conditions makes
    /// the order total, so the result never depends on arrival order. `None`
    /// values order before all strings, and strings by their bytes (which is
    /// `str` order).
    fn sort_events(&mut self) {
        let Self { events, values, .. } = self;
        let key = |e: &StoredEvent| {
            (
                e.timestamp_us,
                e.has_value()
                    .then(|| &values[e.start as usize..][..e.len as usize]),
                e.base_condition,
                e.conditions,
            )
        };
        if events.windows(2).all(|w| key(&w[0]) <= key(&w[1])) {
            return;
        }
        events.sort_unstable_by(|a, b| key(a).cmp(&key(b)));
    }

    /// Selects the base (anchor) index, mirroring `ClickHouse`'s
    /// `getBaseIndex`:
    ///
    /// - `head` / `tail`: the literal first/last event in sorted order, which
    ///   must itself satisfy the base condition — otherwise no match.
    /// - `first_match` / `last_match`: the first/last event satisfying both
    ///   the base condition **and** `event1`.
    fn base_index(&self, base: Base) -> Option<usize> {
        let n = self.events.len();
        match base {
            Base::Head => self.events[0].base_condition.then_some(0),
            Base::Tail => self.events[n - 1].base_condition.then_some(n - 1),
            Base::FirstMatch => (0..n)
                .find(|&i| self.events[i].base_condition && self.events[i].conditions & 1 != 0),
            Base::LastMatch => (0..n)
                .rev()
                .find(|&i| self.events[i].base_condition && self.events[i].conditions & 1 != 0),
        }
    }

    /// The value of the event at sorted position `index`, as an owned string.
    fn value_string(&self, index: usize) -> Option<String> {
        self.value_of(self.events[index])
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
    }

    /// Matches the event chain at consecutive positions and returns the
    /// adjacent node's value, mirroring `ClickHouse`'s `getNextNodeIndex`:
    /// `eventK` must hold at position `base + k` (forward) or `base - k`
    /// (backward); on a full match the value at `base ± num_steps` is
    /// returned, or `None` when the chain runs past either end.
    fn next_node_value(&self, base_idx: usize, direction: Direction) -> Option<String> {
        let n = self.events.len();
        match direction {
            Direction::Forward => {
                for step in 0..self.num_steps {
                    let pos = base_idx + step;
                    if pos >= n || (self.events[pos].conditions >> step) & 1 == 0 {
                        return None;
                    }
                }
                let next = base_idx + self.num_steps;
                if next < n {
                    self.value_string(next)
                } else {
                    None
                }
            }
            Direction::Backward => {
                for step in 0..self.num_steps {
                    if step > base_idx {
                        return None;
                    }
                    let pos = base_idx - step;
                    if (self.events[pos].conditions >> step) & 1 == 0 {
                        return None;
                    }
                }
                if base_idx >= self.num_steps {
                    self.value_string(base_idx - self.num_steps)
                } else {
                    None
                }
            }
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

    #[test]
    fn ties_on_timestamp_and_value_do_not_depend_on_arrival_order() {
        // Events tying on (timestamp, value) but differing in whether they
        // match event1. ClickHouse keeps such ties in arrival order (its
        // result for these rows is 'a' or NULL depending on row order), and
        // the old (timestamp, value) key did the same. The total sort key
        // orders the event1 match after the non-match whatever the arrival
        // order, so the head anchor never satisfies event1: NULL both ways.
        let run = |first_matches: bool| {
            let mut state = SequenceNextNodeState::new();
            state.set_direction(Direction::Forward);
            state.set_base(Base::Head);
            state.num_steps = 1;
            state.update(make_event(0, "a", true, &[first_matches]));
            state.update(make_event(0, "a", true, &[!first_matches]));
            state.update(make_event(1_000_000, "z", true, &[false]));
            state.finalize()
        };
        assert_eq!(run(true), None);
        assert_eq!(run(false), None);
    }

    fn make_event(ts: i64, value: &str, base_cond: bool, conds: &[bool]) -> NextNodeEvent {
        let mut bitmask: u32 = 0;
        for (i, &c) in conds.iter().enumerate() {
            if c {
                bitmask |= 1 << i;
            }
        }
        NextNodeEvent {
            timestamp_us: ts,
            value: Some(Arc::from(value)),
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

    // --- Value buffer ---

    fn forward_head_state() -> SequenceNextNodeState {
        let mut state = SequenceNextNodeState::new();
        state.set_direction(Direction::Forward);
        state.set_base(Base::Head);
        state.num_steps = 1;
        state
    }

    #[test]
    fn empty_string_value_is_distinct_from_null() {
        let mut empty = forward_head_state();
        empty.update(make_event(1, "A", true, &[true]));
        empty.update(make_event(2, "", false, &[false]));
        assert_eq!(empty.finalize(), Some(String::new()));

        let mut null = forward_head_state();
        null.update(make_event(1, "A", true, &[true]));
        null.update(make_null_event(2, false, &[false]));
        assert_eq!(null.finalize(), None);
    }

    #[test]
    fn repeated_adjacent_values_are_stored_once() {
        let mut state = forward_head_state();
        for ts in 1..=4 {
            state.update(make_event(ts, "page", false, &[false]));
        }
        state.update(make_event(5, "other", false, &[false]));
        state.update(make_event(6, "page", false, &[false]));
        assert_eq!(state.event_count(), 6);
        // "page" + "other" + "page" again (not adjacent to the first run).
        assert_eq!(
            state.values.len(),
            "page".len() + "other".len() + "page".len()
        );
    }

    #[test]
    fn combine_moves_the_value_ranges_of_the_appended_events() {
        // Interleaved timestamps, so the sorted order mixes both states' values.
        let mut a = forward_head_state();
        a.update(make_event(1, "head", true, &[true]));
        a.update(make_event(3, "third", false, &[false]));
        let mut b = SequenceNextNodeState::new();
        b.update(make_event(2, "second", false, &[false]));
        b.update(make_null_event(4, false, &[false]));
        b.update(make_event(5, "fifth", false, &[false]));

        let mut combined = a.clone();
        combined.combine_in_place(&b);
        assert_eq!(combined.finalize(), Some("second".to_string()));

        // The same events fed to one state give the same chain of values.
        let mut whole = forward_head_state();
        for (ts, value) in [
            (1, Some("head")),
            (3, Some("third")),
            (2, Some("second")),
            (4, None),
            (5, Some("fifth")),
        ] {
            match value {
                Some(v) => whole.update(make_event(ts, v, ts == 1, &[ts == 1])),
                None => whole.update(make_null_event(ts, false, &[false])),
            }
        }
        whole.sort_events();
        let in_order: Vec<Option<&[u8]>> =
            whole.events.iter().map(|&e| whole.value_of(e)).collect();
        let mut combined_sorted = combined.clone();
        combined_sorted.sort_events();
        let combined_order: Vec<Option<&[u8]>> = combined_sorted
            .events
            .iter()
            .map(|&e| combined_sorted.value_of(e))
            .collect();
        assert_eq!(in_order, combined_order);
    }

    #[test]
    fn combine_leaves_the_source_unchanged() {
        let mut a = forward_head_state();
        a.update(make_event(1, "x", true, &[true]));
        let mut b = SequenceNextNodeState::new();
        b.update(make_event(2, "y", false, &[false]));
        let before = (b.event_count(), b.values.clone());
        a.combine_in_place(&b);
        assert_eq!((b.event_count(), b.values.clone()), before);
        assert_eq!(a.event_count(), 2);
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
    fn test_forward_tail_is_structurally_none() {
        // ClickHouse parity: `tail` anchors at the literal LAST event. A
        // forward chain from the tail ends at or past the last event, so the
        // "next node" is always out of bounds -> NULL (even when the tail
        // satisfies base_condition and event1).
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Forward);
        state.base = Some(Base::Tail);
        state.num_steps = 1;

        state.update(make_event(1, "A", false, &[false]));
        state.update(make_event(2, "B", true, &[true])); // tail: base + event1

        assert_eq!(state.finalize(), None);
    }

    #[test]
    fn test_forward_tail_not_base_is_none() {
        // ClickHouse parity: when the literal last event does not satisfy
        // base_condition, `tail` produces no anchor at all — earlier
        // base-condition events are NOT considered.
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Forward);
        state.base = Some(Base::Tail);
        state.num_steps = 2;

        state.update(make_event(1, "A", true, &[true, false]));
        state.update(make_event(2, "B", false, &[false, true]));
        state.update(make_event(3, "E", false, &[false, false])); // tail, not base

        assert_eq!(state.finalize(), None);
    }

    // --- Forward + FirstMatch ---

    #[test]
    fn test_forward_first_match_basic() {
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Forward);
        state.base = Some(Base::FirstMatch);
        state.num_steps = 2;

        // First event satisfying base + event1 anchors the chain; event2
        // matches at the immediately following position.
        state.update(make_event(1, "X", false, &[false, false]));
        state.update(make_event(2, "A", true, &[true, false]));
        state.update(make_event(3, "B", false, &[false, true]));
        state.update(make_event(4, "C", false, &[false, false]));

        assert_eq!(state.finalize(), Some("C".to_string()));
    }

    #[test]
    fn test_forward_first_match_single_anchor_no_retry() {
        // ClickHouse parity: getBaseIndex selects ONE anchor (the first event
        // satisfying base + event1). If the chain fails at that anchor, the
        // result is NULL — later anchors are not tried.
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Forward);
        state.base = Some(Base::FirstMatch);
        state.num_steps = 2;

        state.update(make_event(1, "A", true, &[true, false]));
        state.update(make_event(2, "X", false, &[false, false])); // breaks the chain
        state.update(make_event(3, "A", true, &[true, false]));
        state.update(make_event(4, "B", false, &[false, true]));
        state.update(make_event(5, "C", false, &[false, false]));

        assert_eq!(state.finalize(), None);
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
    fn test_backward_head_is_structurally_none() {
        // ClickHouse parity: `head` anchors at the literal FIRST event. A
        // backward chain from the head would need a node before index 0,
        // which never exists -> NULL (even when the head matches).
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Backward);
        state.base = Some(Base::Head);
        state.num_steps = 1;

        state.update(make_event(1, "A", true, &[true])); // head: base + event1
        state.update(make_event(2, "B", false, &[false]));

        assert_eq!(state.finalize(), None);
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

        // first_match scans ASCENDING (ClickHouse getBaseIndex): the first
        // event with base + event1 is C (pos 2). Backward chain: event1 at
        // pos 2, event2 at pos 1 (B). Next node backward = pos 0 = A.
        assert_eq!(state.finalize(), Some("A".to_string()));
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

        // last_match takes the LAST event with base + event1: E (pos 4).
        // Backward chain: event1 at pos 4, event2 at pos 3 (D). Next node
        // backward = pos 2 = C.
        assert_eq!(state.finalize(), Some("C".to_string()));
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
    fn test_null_value_event_can_be_a_chain_step() {
        // A NULL node value does not block matching — conditions do. Here the
        // NULL-value event itself satisfies event2, so the chain completes
        // and the next node is returned.
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Forward);
        state.base = Some(Base::FirstMatch);
        state.num_steps = 2;

        state.update(make_event(1, "A", true, &[true, false]));
        state.update(make_null_event(2, false, &[false, true])); // event2, NULL value
        state.update(make_event(3, "B", false, &[false, false]));

        assert_eq!(state.finalize(), Some("B".to_string()));
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
    fn test_forward_gap_events_break_the_chain() {
        // ClickHouse parity: the chain must match CONSECUTIVE events.
        // eventK must hold at position base+K; interleaved non-matching
        // events break the chain.
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Forward);
        state.base = Some(Base::FirstMatch);
        state.num_steps = 2;

        state.update(make_event(1, "A", true, &[true, false]));
        state.update(make_event(2, "X", false, &[false, false])); // gap
        state.update(make_event(3, "Y", false, &[false, false])); // gap
        state.update(make_event(4, "B", false, &[false, true]));
        state.update(make_event(5, "C", false, &[false, false]));

        assert_eq!(state.finalize(), None);
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
    fn test_stored_event_size() {
        // The per-event record kept in a state; the docs quote 24 bytes.
        assert_eq!(std::mem::size_of::<StoredEvent>(), 24);
    }

    #[test]
    fn test_combine_appends_events_and_keeps_their_values_readable() {
        let mut a = SequenceNextNodeState::new();
        a.direction = Some(Direction::Forward);
        a.base = Some(Base::FirstMatch);
        a.num_steps = 1;
        a.update(make_event(1, "A", true, &[true]));

        let mut b = SequenceNextNodeState::new();
        b.update(make_event(2, "B", false, &[false]));

        a.combine_in_place(&b);
        assert_eq!(a.events.len(), 2);
        assert_eq!(a.finalize(), Some("B".to_string()));
    }

    /// Values are arbitrary bytes of a `VARCHAR`: an embedded NUL neither ends
    /// the string nor changes its length.
    #[test]
    fn test_value_with_embedded_nul_is_returned_whole() {
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Forward);
        state.base = Some(Base::FirstMatch);
        state.num_steps = 1;
        state.update(make_event(1, "A", true, &[true]));
        state.update(make_event(2, "hello\0world", false, &[false]));
        assert_eq!(state.finalize().as_deref(), Some("hello\0world"));
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
            value: Some(Arc::from("")),
            base_condition: true,
            conditions: 1,
        });
        state.update(NextNodeEvent {
            timestamp_us: 2,
            value: Some(Arc::from("")),
            base_condition: false,
            conditions: 0,
        });

        assert_eq!(state.finalize(), Some(String::new()));
    }

    #[test]
    fn test_unicode_string_value() {
        // Edge case: Unicode strings should be preserved through Arc<str>
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Forward);
        state.base = Some(Base::FirstMatch);
        state.num_steps = 1;

        state.update(NextNodeEvent {
            timestamp_us: 1,
            value: Some(Arc::from("hello")),
            base_condition: true,
            conditions: 1,
        });
        state.update(NextNodeEvent {
            timestamp_us: 2,
            value: Some(Arc::from("world")),
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
            value: Some(Arc::from(long_value.as_str())),
            base_condition: false,
            conditions: 0,
        });

        let result = state.finalize();
        assert_eq!(result.as_deref(), Some(long_value.as_str()));
    }

    #[test]
    fn test_32_conditions_with_rc_str() {
        // Verify 32-condition support works with Arc<str> values
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Forward);
        state.base = Some(Base::FirstMatch);
        state.num_steps = 32;

        // Event with all 32 conditions set
        state.update(NextNodeEvent {
            timestamp_us: 1,
            value: Some(Arc::from("start")),
            base_condition: true,
            conditions: 0xFFFF_FFFF,
        });
        state.update(NextNodeEvent {
            timestamp_us: 2,
            value: Some(Arc::from("result")),
            base_condition: false,
            conditions: 0,
        });

        // With 32 steps and all conditions set on one event, only step 0
        // matches at position 0. Steps 1-31 need separate events.
        // This tests that the condition bitmask works correctly with Arc<str>.
        // (Full match won't happen with just one event for 32 steps, so None)
        assert_eq!(state.finalize(), None);
    }

    #[test]
    fn test_combine_chain_three_states() {
        // Verify combine chain with 3 states preserves all Arc<str> values
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
        // Verify combine handles mixed null/non-null Arc<str> values correctly
        let mut a = SequenceNextNodeState::new();
        a.direction = Some(Direction::Forward);
        a.base = Some(Base::FirstMatch);
        a.num_steps = 2;
        a.update(make_event(1, "A", true, &[true, false]));

        let mut b = SequenceNextNodeState::new();
        b.update(make_null_event(2, false, &[false, true])); // event2, NULL value
        b.update(make_event(3, "B", false, &[false, false]));
        b.update(make_event(4, "C", false, &[false, false]));

        a.combine_in_place(&b);
        // Chain: A (event1) -> NULL-value event (event2); next node is B.
        assert_eq!(a.finalize(), Some("B".to_string()));
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

    // ── Coverage gap: direction × base edge cases ──

    #[test]
    fn test_backward_tail_single_match_with_previous() {
        // Backward + Tail: the anchor is the literal LAST event (which must
        // satisfy base_condition); event1 matches there and the node before
        // the chain is returned.
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Backward);
        state.base = Some(Base::Tail);
        state.num_steps = 1;
        state.update(make_event(1, "A", false, &[false]));
        state.update(make_event(2, "B", false, &[false]));
        state.update(make_event(3, "C", true, &[true])); // tail: base + event1

        // Chain matches at the tail (pos 2); next node backward = pos 1 = B.
        assert_eq!(state.finalize(), Some("B".to_string()));
    }

    #[test]
    fn test_backward_tail_match_at_start_no_previous() {
        // Backward + Tail: match at first position, no previous event.
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Backward);
        state.base = Some(Base::Tail);
        state.num_steps = 1;
        state.update(make_event(1, "A", true, &[true])); // base + event1 at pos 0
        state.update(make_event(2, "B", false, &[false]));
        // Backward from tail: last base is A at pos 0, no event before → None
        assert_eq!(state.finalize(), None);
    }

    #[test]
    fn test_no_base_condition_events_returns_none() {
        // No events have base_condition=true → no match possible.
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Forward);
        state.base = Some(Base::FirstMatch);
        state.num_steps = 1;
        state.update(make_event(1, "A", false, &[true]));
        state.update(make_event(2, "B", false, &[true]));
        // base_condition is false for all events
        assert_eq!(state.finalize(), None);
    }

    #[test]
    fn test_backward_last_match_unsorted_input() {
        // Unsorted events should be sorted before matching.
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Backward);
        state.base = Some(Base::LastMatch);
        state.num_steps = 1;
        // Insert out of order
        state.update(make_event(3, "C", false, &[false]));
        state.update(make_event(1, "A", true, &[true])); // base + event1
        state.update(make_event(2, "B", false, &[false]));
        // After sort: A(1) → B(2) → C(3)
        // LastMatch backward: last base at pos 0 (A), event1 at pos 0
        // backward → event before pos 0 = None
        assert_eq!(state.finalize(), None);
    }

    #[test]
    fn test_backward_head_anchor_is_literal_first_event() {
        // ClickHouse parity: `head` is the literal first event regardless of
        // which later events satisfy base_condition. Here the first event is
        // not a base event, so there is no anchor at all.
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(Direction::Backward);
        state.base = Some(Base::Head);
        state.num_steps = 1;
        state.update(make_event(1, "A", false, &[false])); // head, not base
        state.update(make_event(2, "B", true, &[true]));
        state.update(make_event(3, "C", true, &[true]));
        state.update(make_event(4, "D", false, &[false]));
        assert_eq!(state.finalize(), None);
    }
}

/// Tests written for mutants cargo-mutants found surviving: each pins a
/// behaviour that one operator change would alter.
#[cfg(test)]
mod mutation_tests {
    use super::*;

    fn event(ts: i64, value: &str, base: bool, conditions: u32) -> NextNodeEvent {
        NextNodeEvent::new(ts, Some(Arc::from(value)), base, conditions)
    }

    fn state(
        direction: Direction,
        base: Base,
        steps: usize,
        events: Vec<NextNodeEvent>,
    ) -> SequenceNextNodeState {
        let mut state = SequenceNextNodeState::new();
        state.direction = Some(direction);
        state.base = Some(base);
        state.num_steps = steps;
        for e in events {
            state.update(e);
        }
        state
    }

    /// `first_match` anchors on the first event satisfying the base
    /// condition and `event1`, skipping one that satisfies only the base.
    #[test]
    fn first_match_needs_event1() {
        let mut s = state(
            Direction::Forward,
            Base::FirstMatch,
            1,
            vec![
                event(0, "A", true, 0),
                event(1, "B", true, 1),
                event(2, "C", false, 0),
            ],
        );
        assert_eq!(s.finalize().as_deref(), Some("C"));
    }

    /// `last_match` anchors on the last event satisfying both the base
    /// condition and `event1` (B), not on one satisfying either (C or D).
    #[test]
    fn last_match_needs_base_and_event1() {
        let mut s = state(
            Direction::Backward,
            Base::LastMatch,
            1,
            vec![
                event(0, "A", false, 0),
                event(1, "B", true, 1),
                event(2, "C", true, 0),
                event(3, "D", false, 1),
            ],
        );
        assert_eq!(s.finalize().as_deref(), Some("A"));
    }

    /// A backward chain fails when an earlier step does not hold.
    #[test]
    fn backward_chain_checks_every_step() {
        let mut s = state(
            Direction::Backward,
            Base::Tail,
            2,
            vec![
                event(0, "A", false, 0),
                event(1, "B", false, 0),
                event(2, "C", true, 1),
            ],
        );
        assert_eq!(s.finalize(), None);
    }

    /// Combining with a fresh state keeps the step count.
    #[test]
    fn combine_with_fresh_state_keeps_num_steps() {
        let s = state(
            Direction::Forward,
            Base::Head,
            1,
            vec![event(0, "A", true, 1), event(1, "B", false, 0)],
        );
        let mut combined = s.combine(&SequenceNextNodeState::new());
        assert_eq!(combined.finalize().as_deref(), Some("B"));
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn forward_first_match_with_match_returns_some(
            num_prefix_events in 0..20usize,
        ) {
            // Any number of non-qualifying PREFIX events must not disturb
            // first_match anchoring; the consecutive chain that follows
            // matches and the next node is returned (ClickHouse semantics:
            // the chain itself must be consecutive, the anchor search skips
            // non-qualifying events).
            let mut state = SequenceNextNodeState::new();
            state.direction = Some(Direction::Forward);
            state.base = Some(Base::FirstMatch);
            state.num_steps = 2;

            for i in 0..num_prefix_events {
                state.update(NextNodeEvent {
                    timestamp_us: i as i64,
                    value: Some(Arc::from(format!("prefix_{i}").as_str())),
                    base_condition: false,
                    conditions: 0,
                });
            }

            state.update(NextNodeEvent {
                timestamp_us: num_prefix_events as i64,
                value: Some(Arc::from("start")),
                base_condition: true,
                conditions: 1, // event1
            });

            state.update(NextNodeEvent {
                timestamp_us: (num_prefix_events as i64 + 1),
                value: Some(Arc::from("matched")),
                base_condition: false,
                conditions: 2, // event2 (consecutive with event1)
            });

            state.update(NextNodeEvent {
                timestamp_us: (num_prefix_events as i64 + 2),
                value: Some(Arc::from("result")),
                base_condition: false,
                conditions: 0,
            });

            let result = state.finalize();
            prop_assert_eq!(result, Some("result".to_string()));
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
                    value: Some(Arc::from(format!("evt_{i}").as_str())),
                    base_condition: false, // no base condition satisfied
                    conditions: 1,
                });
            }

            prop_assert!(state.finalize().is_none());
        }
    }
}
