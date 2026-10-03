// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! Pattern executor for sequence matching.
//!
//! Executes compiled patterns against sorted event streams. Common shapes
//! (`(?1)(?2)`, `(?1).*(?2)`) use linear fast paths; every other pattern goes
//! through a feasibility-then-greedy matcher (described below) that runs in
//! O(s · n log n) for `n` events and `s` pattern steps.
//!
//! # Semantics
//!
//! The matcher reproduces, result for result, the lazy depth-first search the
//! extension used before (kept as a test oracle in `reference_nfa`):
//!
//! - A pattern is a list of event-consuming steps (`(?N)`, `.`) separated by
//!   *gaps* of non-consuming steps (`.*`, `(?t op N)`).
//! - `.*` skips any number of events.
//! - A time constraint is measured from the event consumed by the last
//!   `(?N)` or `.` (the gap's *anchor*), in whole seconds (the microsecond
//!   gap floored). It passes at an event where the comparison holds, and may
//!   skip events while it could still (or again) hold: `>=`, `>`, `!=`
//!   always skip; `<=`, `<` skip only while satisfied; `==` skips while the
//!   elapsed time is at most `N`. At the end of the events, `<=`, `<` and
//!   `>= 0` pass vacuously.
//! - `sequence_match`: does any start position admit a full match.
//! - `sequence_count`: from the first position that admits a match, take the
//!   match the lazy search finds first, resume at the position after it
//!   (which, after a trailing gate, can be past the last consumed event), and
//!   repeat.
//! - `sequence_match_events`: the condition timestamps of the first full
//!   match; if none exists, of the first-explored chain reaching the furthest
//!   `(?N)` step.
//!
//! "The match the lazy search finds first" is the lexicographically smallest
//! vector of positions. Because every gap step reaches a superset of
//! positions from an earlier position (for the same anchor), that vector's
//! consumed positions are exactly those a greedy walk picks when it always
//! takes the earliest position from which the rest of the pattern can still
//! complete. The matcher computes that "can still complete" set with one
//! backward pass per consuming step and then walks forward greedily.

use crate::common::event::Event;
use crate::common::timestamp::MICROS_PER_SECOND;
use crate::pattern::parser::{CompiledPattern, PatternError, PatternStep, TimeOp};

/// Result of executing a pattern against an event stream.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct MatchResult {
    /// Whether any full match was found.
    pub matched: bool,
    /// Number of non-overlapping full matches found.
    pub count: usize,
}

/// Executes a compiled pattern against a sorted event stream.
///
/// Events must be sorted by timestamp (ascending) before calling this function.
///
/// For `sequence_match` semantics (`count_all == false`): reports whether
/// one match exists. For `sequence_count` semantics: counts all
/// non-overlapping matches.
///
/// # Algorithm
///
/// - **Adjacent conditions only** (`(?1)(?2)(?3)`): O(n) scan with a sliding
///   window of `k` events.
/// - **Wildcard-separated conditions** (`(?1).*(?2).*(?3)`): O(n) single-pass
///   linear scan with a step counter.
/// - **Everything else** (time constraints, `.`, mixed shapes): the
///   feasibility-then-greedy matcher, O(s · n log n) for `s` steps (see the
///   module docs).
///
/// # Errors
///
/// Returns a [`PatternError`] if the general matcher cannot allocate its
/// working memory (about one bit per event per `(?N)`/`.` step plus 8 bytes
/// per event); the fast paths need none.
pub fn execute_pattern(
    pattern: &CompiledPattern,
    events: &[Event],
    count_all: bool,
) -> Result<MatchResult, PatternError> {
    if events.is_empty() || pattern.steps.is_empty() {
        return Ok(MatchResult {
            matched: false,
            count: 0,
        });
    }

    match classify_pattern(pattern) {
        PatternShape::AdjacentConditions => Ok(with_conditions(pattern, |conds| {
            fast_adjacent(events, conds, count_all)
        })),
        PatternShape::WildcardSeparated => Ok(with_conditions(pattern, |conds| {
            fast_wildcard(events, conds, count_all)
        })),
        PatternShape::Complex => Matcher::new(pattern, events).execute(count_all),
    }
}

/// Executes a compiled pattern and returns matched condition timestamps.
///
/// Returns the timestamps of the `(?N)` condition steps (not `.`, `.*`, or
/// time constraints) of the first full match. If the pattern never matches,
/// returns those of the first chain reaching the furthest `(?N)` step
/// (`ClickHouse`'s "longest chain"), or an empty vector when no `(?N)` step
/// is ever reached. Events must be sorted by timestamp (ascending).
///
/// # Errors
///
/// Returns a [`PatternError`] if the general matcher cannot allocate its
/// working memory.
pub fn execute_pattern_events(
    pattern: &CompiledPattern,
    events: &[Event],
) -> Result<Vec<i64>, PatternError> {
    if events.is_empty() || pattern.steps.is_empty() {
        return Ok(Vec::new());
    }
    if classify_pattern(pattern) == PatternShape::WildcardSeparated {
        return Ok(with_conditions(pattern, |conds| {
            fast_wildcard_events(events, conds)
        }));
    }
    Matcher::new(pattern, events).execute_events()
}

/// Pattern shape classification for fast-path dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PatternShape {
    /// All steps are `Condition` — adjacent matching required.
    AdjacentConditions,
    /// Conditions separated by `.*` — greedy forward scan.
    WildcardSeparated,
    /// Requires the general matcher (time constraints, `.`, mixed shapes).
    Complex,
}

/// Classifies a compiled pattern into a fast-path shape, without allocating
/// (it runs once per group).
///
/// Returns `AdjacentConditions` if all steps are `Condition` (no wildcards).
/// Returns `WildcardSeparated` if the pattern mixes `Condition` and
/// `AnyEvents` steps with no two conditions adjacent (e.g.,
/// `(?1).*(?2).*(?3)`). Returns `Complex` for patterns with time
/// constraints, `.` (`OneEvent`), or other structures.
fn classify_pattern(pattern: &CompiledPattern) -> PatternShape {
    let mut has_condition = false;
    let mut has_any_events = false;
    for step in &pattern.steps {
        match step {
            PatternStep::Condition(_) => has_condition = true,
            PatternStep::AnyEvents => has_any_events = true,
            PatternStep::OneEvent | PatternStep::TimeConstraint(_, _) => {
                return PatternShape::Complex;
            }
        }
    }
    if !has_condition {
        return PatternShape::Complex;
    }
    if !has_any_events {
        return PatternShape::AdjacentConditions;
    }
    // Only valid when no two conditions are adjacent: `(?1)(?2).*(?3)`
    // requires `(?2)` on the event right after `(?1)`, which the
    // step-counter scan cannot express.
    let adjacent_conditions = pattern.steps.windows(2).any(|w| {
        matches!(w[0], PatternStep::Condition(_)) && matches!(w[1], PatternStep::Condition(_))
    });
    if adjacent_conditions {
        PatternShape::Complex
    } else {
        PatternShape::WildcardSeparated
    }
}

/// Calls `f` with the pattern's condition indices in order, from a stack
/// buffer when there are at most 32 (no per-group allocation).
fn with_conditions<R>(pattern: &CompiledPattern, f: impl FnOnce(&[usize]) -> R) -> R {
    let conditions = pattern.steps.iter().filter_map(|step| match step {
        PatternStep::Condition(idx) => Some(*idx),
        _ => None,
    });
    let mut buffer = [0usize; 32];
    let mut len = 0;
    for idx in conditions.clone() {
        if len == buffer.len() {
            return f(&conditions.collect::<Vec<_>>());
        }
        buffer[len] = idx;
        len += 1;
    }
    f(&buffer[..len])
}

/// Fast path for adjacent-condition patterns like `(?1)(?2)(?3)`.
///
/// Scans with a sliding window of `k` events, checking each window for a
/// consecutive match of all conditions. O(n) time, O(1) space.
fn fast_adjacent(events: &[Event], conditions: &[usize], count_all: bool) -> MatchResult {
    let k = conditions.len();
    if events.len() < k {
        return MatchResult {
            matched: false,
            count: 0,
        };
    }

    let mut total = 0;
    let mut i = 0;
    while i + k <= events.len() {
        let mut matched = true;
        for (j, &cond_idx) in conditions.iter().enumerate() {
            if !events[i + j].condition(cond_idx) {
                matched = false;
                i += 1;
                break;
            }
        }
        if matched {
            total += 1;
            if !count_all {
                return MatchResult {
                    matched: true,
                    count: 1,
                };
            }
            i += k; // Non-overlapping: advance past the match
        }
    }

    MatchResult {
        matched: total > 0,
        count: total,
    }
}

/// Fast path for wildcard-separated patterns like `(?1).*(?2).*(?3)`.
///
/// Single-pass linear scan: maintains a step counter and advances through
/// conditions as matching events are found. O(n) time, O(1) space.
/// Equivalent to lazy NFA matching for this pattern shape.
fn fast_wildcard(events: &[Event], conditions: &[usize], count_all: bool) -> MatchResult {
    let k = conditions.len();
    let mut total = 0;
    let mut step = 0;

    for event in events {
        if event.condition(conditions[step]) {
            step += 1;
            if step >= k {
                total += 1;
                if !count_all {
                    return MatchResult {
                        matched: true,
                        count: 1,
                    };
                }
                step = 0; // Reset for next non-overlapping match
            }
        }
    }

    MatchResult {
        matched: total > 0,
        count: total,
    }
}

/// Event-collecting fast path for wildcard-separated patterns.
///
/// The earliest event for each condition in turn is the chain the lazy
/// search finds first: any chain can be moved to these positions. When the
/// scan runs out of events, the same greedy prefix is the first-explored
/// longest partial chain. O(n) time, stops at the first full match.
fn fast_wildcard_events(events: &[Event], conditions: &[usize]) -> Vec<i64> {
    let mut timestamps = Vec::with_capacity(conditions.len());
    for event in events {
        if event.condition(conditions[timestamps.len()]) {
            timestamps.push(event.timestamp_us);
            if timestamps.len() == conditions.len() {
                break;
            }
        }
    }
    timestamps
}

/// A non-consuming step inside a gap.
#[derive(Debug, Clone, Copy)]
enum GapStep {
    /// `.*`
    AnyEvents,
    /// `(?t op N)`
    Gate(TimeOp, i64),
}

/// A pattern split into event-consuming steps and the gaps after them.
struct Plan {
    /// One entry per `(?N)` (`Some(N - 1)`) or `.` (`None`), in order.
    consumers: Vec<Option<usize>>,
    /// `gaps[k]` holds the non-consuming steps after consumer `k`; the last
    /// entry is the tail after the final consumer. Steps before the first
    /// consumer can only be `.*` (the parser rejects a leading time
    /// constraint), which does not change any result: the start loop already
    /// tries every position.
    gaps: Vec<Vec<GapStep>>,
}

impl Plan {
    fn new(pattern: &CompiledPattern) -> Self {
        let mut consumers = Vec::new();
        let mut gaps = Vec::new();
        let mut current = Vec::new();
        for step in &pattern.steps {
            let consumer = match *step {
                PatternStep::Condition(idx) => Some(idx),
                PatternStep::OneEvent => None,
                PatternStep::AnyEvents => {
                    current.push(GapStep::AnyEvents);
                    continue;
                }
                PatternStep::TimeConstraint(op, threshold) => {
                    current.push(GapStep::Gate(op, threshold));
                    continue;
                }
            };
            if consumers.is_empty() {
                debug_assert!(
                    current.iter().all(|s| matches!(s, GapStep::AnyEvents)),
                    "the parser rejects a time constraint before the first consuming step"
                );
                current.clear();
            } else {
                gaps.push(std::mem::take(&mut current));
            }
            consumers.push(consumer);
        }
        gaps.push(current);
        Self { consumers, gaps }
    }
}

/// Up to two half-open position ranges, in ascending order. Empty ranges
/// have `lo >= hi`.
type Ranges = [(usize, usize); 2];

/// Feasibility-then-greedy matcher over one sorted event slice.
///
/// Positions are event indices; `n` denotes the number of events, and a
/// gap's *anchor* is the event its preceding consumer consumed.
struct Matcher<'a> {
    events: &'a [Event],
    plan: Plan,
}

impl<'a> Matcher<'a> {
    fn new(pattern: &CompiledPattern, events: &'a [Event]) -> Self {
        Self {
            events,
            plan: Plan::new(pattern),
        }
    }

    const fn len(&self) -> usize {
        self.events.len()
    }

    /// Whether consumer `k` can consume event `b`.
    fn consumes(&self, k: usize, b: usize) -> bool {
        self.plan.consumers[k].is_none_or(|cond| self.events[b].condition(cond))
    }

    /// Whole seconds from event `anchor` to event `p` (`p > anchor`).
    ///
    /// Events are sorted, so the true gap is non-negative and (even spanning
    /// ±infinity timestamps) fits in u64; `wrapping_sub` reinterpreted as
    /// u64 IS that gap, and dividing in u64 keeps the i64 conversion exact.
    fn elapsed(&self, anchor: usize, p: usize) -> i64 {
        let gap_us = self.events[p]
            .timestamp_us
            .wrapping_sub(self.events[anchor].timestamp_us) as u64;
        (gap_us / MICROS_PER_SECOND as u64) as i64
    }

    /// First position in `[lo, n)` whose elapsed time from `anchor` fails `pred`
    /// (`n` if none), given that `pred` holds on a prefix of it (elapsed
    /// time is non-decreasing in position).
    ///
    /// Gallops from `lo` before binary searching, so the cost is logarithmic
    /// in the distance to the answer rather than in the remaining events.
    fn first_failing(&self, anchor: usize, lo: usize, pred: impl Fn(i64) -> bool) -> usize {
        let len = self.len();
        let anchor_us = self.events[anchor].timestamp_us;
        let holds = |e: &Event| {
            let gap_us = e.timestamp_us.wrapping_sub(anchor_us) as u64;
            pred((gap_us / MICROS_PER_SECOND as u64) as i64)
        };
        if lo >= len || !holds(&self.events[lo]) {
            return lo;
        }
        // `pred` holds at `known`; the answer is in (known, bound].
        let mut known = lo;
        let mut step = 1;
        let bound = loop {
            let probe = known.saturating_add(step);
            if probe >= len {
                break len;
            }
            if !holds(&self.events[probe]) {
                break probe;
            }
            known = probe;
            step *= 2;
        };
        known + 1 + self.events[known + 1..bound].partition_point(holds)
    }

    /// Positions in `[p, n)` where the gate passes after skipping from `p`.
    fn gate_ranges(&self, op: TimeOp, threshold: i64, anchor: usize, p: usize) -> Ranges {
        let len = self.len();
        let none = (len, len);
        if p >= len {
            return [none, none];
        }
        match op {
            // Satisfied on a prefix; skipping stops at the first violation.
            TimeOp::Lt | TimeOp::Lte => [
                (
                    p,
                    self.first_failing(anchor, p, |e| op.evaluate(e, threshold)),
                ),
                none,
            ],
            // Satisfied on a suffix; skipping never stops.
            TimeOp::Gt | TimeOp::Gte => [
                (
                    self.first_failing(anchor, p, |e| !op.evaluate(e, threshold)),
                    len,
                ),
                none,
            ],
            // Skipping stops once elapsed exceeds the threshold.
            TimeOp::Eq => [
                (
                    self.first_failing(anchor, p, |e| e < threshold),
                    self.first_failing(anchor, p, |e| e <= threshold),
                ),
                none,
            ],
            TimeOp::Ne => [
                (p, self.first_failing(anchor, p, |e| e < threshold)),
                (self.first_failing(anchor, p, |e| e <= threshold), len),
            ],
        }
    }

    /// Earliest position below `n` where the gate passes, starting at `p`.
    fn gate_earliest(&self, op: TimeOp, threshold: i64, anchor: usize, p: usize) -> Option<usize> {
        let len = self.len();
        if p >= len {
            return None;
        }
        let found = match op {
            TimeOp::Lt | TimeOp::Lte => p,
            TimeOp::Gt | TimeOp::Gte => {
                self.first_failing(anchor, p, |e| !op.evaluate(e, threshold))
            }
            TimeOp::Eq => self.first_failing(anchor, p, |e| e < threshold),
            TimeOp::Ne if self.elapsed(anchor, p) != threshold => p,
            TimeOp::Ne => self.first_failing(anchor, p, |e| e <= threshold),
        };
        (found < len && op.evaluate(self.elapsed(anchor, found), threshold)).then_some(found)
    }

    /// The earliest position at which consumer `k + 1` can consume an event
    /// when consumer `k` consumed event `b`, among positions where
    /// `first_true(i)` (the smallest acceptable position `>= i`, or `n`)
    /// accepts. Agrees with the earliest acceptable position in
    /// [`Self::candidates`], without computing the ranges' upper ends.
    fn first_candidate(
        &self,
        k: usize,
        b: usize,
        first_true: impl Fn(usize) -> usize,
    ) -> Option<usize> {
        let len = self.len();
        let mut p = b + 1;
        if p >= len {
            return None;
        }
        let Some((last, init)) = self.plan.gaps[k].split_last() else {
            return (first_true(p) == p).then_some(p);
        };
        for step in init {
            if let GapStep::Gate(op, threshold) = *step {
                p = self.gate_earliest(op, threshold, b, p)?;
            }
        }
        let found = match *last {
            GapStep::AnyEvents => first_true(p),
            GapStep::Gate(op, threshold) => match op {
                // Passes on a prefix of [p, n): the first acceptable
                // position is in range iff it passes.
                TimeOp::Lt | TimeOp::Lte => first_true(p),
                // Passes on a suffix.
                TimeOp::Gt | TimeOp::Gte => {
                    first_true(self.first_failing(b, p, |e| !op.evaluate(e, threshold)))
                }
                // Passes on the run with elapsed == threshold.
                TimeOp::Eq => first_true(self.first_failing(b, p, |e| e < threshold)),
                // Passes everywhere except the run with elapsed == threshold.
                TimeOp::Ne => {
                    let found = first_true(p);
                    if found < len && self.elapsed(b, found) == threshold {
                        first_true(self.first_failing(b, found, |e| e <= threshold))
                    } else {
                        found
                    }
                }
            },
        };
        let passes = |found: usize| match *last {
            GapStep::AnyEvents => true,
            GapStep::Gate(op, threshold) => op.evaluate(self.elapsed(b, found), threshold),
        };
        (found < len && passes(found)).then_some(found)
    }

    /// Positions where consumer `k + 1` may consume an event when consumer
    /// `k` consumed event `b`.
    ///
    /// Only positions below `n` matter here (a consumer needs an event), and
    /// below `n` each gap step reaches a superset of positions from an
    /// earlier position. So every step but the last contributes only its
    /// earliest position, and the last step's full range is the answer.
    fn candidates(&self, k: usize, b: usize) -> Ranges {
        let len = self.len();
        let none = (len, len);
        let mut p = b + 1;
        if p >= len {
            return [none, none];
        }
        let Some((last, init)) = self.plan.gaps[k].split_last() else {
            return [(p, p + 1), none];
        };
        for step in init {
            if let GapStep::Gate(op, threshold) = *step {
                match self.gate_earliest(op, threshold, b, p) {
                    Some(found) => p = found,
                    None => return [none, none],
                }
            }
        }
        match *last {
            GapStep::AnyEvents => [(p, len), none],
            GapStep::Gate(op, threshold) => self.gate_ranges(op, threshold, b, p),
        }
    }

    /// Where a full match ends when the last consumer consumed event `b`:
    /// the earliest position the tail can reach, which is `n` when the tail
    /// can only finish at the end of the events (e.g. `.*(?t<5)` after the
    /// last gap event is too late). `None` if the tail cannot finish.
    fn tail_end(&self, b: usize) -> Option<usize> {
        let len = self.len();
        // Earliest reachable position below n, and whether n is reachable.
        let mut earliest = (b + 1 < len).then_some(b + 1);
        let mut at_end = b + 1 >= len;
        for step in self.plan.gaps.last().expect("tail gap") {
            match *step {
                GapStep::AnyEvents => at_end |= earliest.is_some(),
                GapStep::Gate(op, threshold) => {
                    let vacuous_at_end = matches!(op, TimeOp::Lte | TimeOp::Lt)
                        || (op == TimeOp::Gte && threshold == 0);
                    // From a position below n the gate skips to the end
                    // only if it can skip every remaining event.
                    let skips_to_end = || match op {
                        TimeOp::Lte | TimeOp::Lt => {
                            op.evaluate(self.elapsed(b, len - 1), threshold)
                        }
                        _ => true,
                    };
                    at_end = vacuous_at_end && (at_end || (earliest.is_some() && skips_to_end()));
                    earliest = earliest.and_then(|p| self.gate_earliest(op, threshold, b, p));
                }
            }
        }
        earliest.or_else(|| at_end.then_some(len))
    }

    /// The error for a working-memory allocation that failed.
    fn out_of_memory(&self) -> PatternError {
        PatternError {
            message: format!(
                "out of memory matching a pattern of {} steps against {} events in one group",
                self.plan.consumers.len() + self.plan.gaps.iter().map(Vec::len).sum::<usize>(),
                self.len()
            ),
            position: PatternError::NO_POSITION,
        }
    }

    /// Allocates `len` copies of `value`, reporting failure instead of
    /// aborting the process.
    fn try_vec<T: Clone>(&self, len: usize, value: T) -> Result<Vec<T>, PatternError> {
        let mut v = Vec::new();
        v.try_reserve_exact(len).map_err(|_| self.out_of_memory())?;
        v.resize(len, value);
        Ok(v)
    }

    /// An all-clear bit set over the event positions.
    fn try_bits(&self) -> Result<Bits, PatternError> {
        Ok(Bits(self.try_vec(self.len().div_ceil(64), 0u64)?))
    }

    /// `feasible[k]` holds `b` when consumer `k` can consume event `b` and
    /// consumers `k + 1 ..= last` can still follow (plus the tail, if
    /// `with_tail`). One bit per event per consumer, plus one transient
    /// `usize` per event.
    fn feasibility(&self, last: usize, with_tail: bool) -> Result<Vec<Bits>, PatternError> {
        let len = self.len();
        let mut levels = Vec::new();
        levels
            .try_reserve_exact(last + 1)
            .map_err(|_| self.out_of_memory())?;
        // An empty tail always finishes (at the next position).
        let check_tail = with_tail && !self.plan.gaps.last().expect("tail gap").is_empty();
        let mut top = self.try_bits()?;
        for b in 0..len {
            if self.consumes(last, b) && (!check_tail || self.tail_end(b).is_some()) {
                top.set(b);
            }
        }
        levels.push(top);
        // next_true[i]: smallest feasible position >= i in the level above.
        let mut next_true = self.try_vec(len + 1, len)?;
        for k in (0..last).rev() {
            let above = levels.last().expect("level k + 1");
            for i in (0..len).rev() {
                next_true[i] = if above.get(i) { i } else { next_true[i + 1] };
            }
            let mut level = self.try_bits()?;
            for b in 0..len {
                if self.consumes(k, b) && self.first_candidate(k, b, |i| next_true[i]).is_some() {
                    level.set(b);
                }
            }
            levels.push(level);
        }
        levels.reverse();
        Ok(levels)
    }

    /// The greedy walk from `b0` through consumer `levels.len() - 1`: at each
    /// step, the earliest feasible candidate. Calls `visit(k, b)` for every
    /// consumed position and returns the last one.
    fn walk(&self, levels: &[Bits], b0: usize, mut visit: impl FnMut(usize, usize)) -> usize {
        let len = self.len();
        visit(0, b0);
        let mut b = b0;
        for k in 0..levels.len() - 1 {
            let level = &levels[k + 1];
            // Forward scans: the walk only moves forward, so over a whole
            // sequence_count the scans cover each position O(1) times.
            b = self
                .first_candidate(k, b, |i| level.next_set(i, len))
                .expect("a feasible position has a feasible successor");
            visit(k + 1, b);
        }
        b
    }

    fn execute(&self, count_all: bool) -> Result<MatchResult, PatternError> {
        let len = self.len();
        let num_consumers = self.plan.consumers.len();
        if num_consumers == 0 {
            // Only `.*`: an empty match at every position.
            let count = if count_all { len } else { 1 };
            return Ok(MatchResult {
                matched: true,
                count,
            });
        }
        let levels = self.feasibility(num_consumers - 1, true)?;
        let first = &levels[0];
        let mut count = 0;
        let mut start = 0;
        loop {
            let b0 = first.next_set(start, len);
            if b0 == len {
                break;
            }
            count += 1;
            if !count_all {
                break;
            }
            let last = self.walk(&levels, b0, |_, _| {});
            // Non-overlapping: resume after the match. It consumed at least
            // one event, so this always advances.
            start = self.tail_end(last).expect("feasible chain has a tail");
        }
        Ok(MatchResult {
            matched: count > 0,
            count,
        })
    }

    /// Replaces `reach` (the positions consumer `k` can consume on some
    /// chain) with those of consumer `k + 1`.
    ///
    /// For an empty gap, a lone `.*` or a lone gate other than `==`, whether
    /// `b'` is reachable depends on one representative earlier reachable
    /// event: the gate passes at `b'` from anchor `b` exactly when its
    /// comparison holds for the elapsed time from `b` to `b'`, and that time
    /// shrinks as `b` moves later. So `<`/`<=` test the latest reachable `b`,
    /// `>`/`>=` the earliest, and `!=` both (every `b` between them has an
    /// elapsed time between theirs). That is O(n) per step; other gaps mark
    /// every candidate range, which costs a galloping search per reachable
    /// event.
    fn reach_next(&self, k: usize, reach: &mut Bits, delta: &mut [i64]) {
        let len = self.len();
        let single_gate = match self.plan.gaps[k].as_slice() {
            [] | [GapStep::AnyEvents] => None,
            [GapStep::Gate(op, threshold)] if *op != TimeOp::Eq => Some((*op, *threshold)),
            _ => return self.reach_next_by_ranges(k, reach, delta),
        };
        let empty_gap = self.plan.gaps[k].is_empty();
        let mut next = Bits(std::mem::take(&mut reach.0));
        let (mut earliest, mut latest) = (None::<usize>, None::<usize>);
        for b in 0..len {
            // `latest`/`earliest` cover reachable positions before `b`.
            let reachable = match (single_gate, latest) {
                (_, None) => false,
                (None, Some(last)) => !empty_gap || last + 1 == b,
                (Some((op, threshold)), Some(last)) => {
                    let first = earliest.expect("set with latest");
                    match op {
                        TimeOp::Lt | TimeOp::Lte => op.evaluate(self.elapsed(last, b), threshold),
                        TimeOp::Gt | TimeOp::Gte => op.evaluate(self.elapsed(first, b), threshold),
                        _ => {
                            op.evaluate(self.elapsed(last, b), threshold)
                                || op.evaluate(self.elapsed(first, b), threshold)
                        }
                    }
                }
            };
            // `next` reuses `reach`'s words: read bit `b` before overwriting.
            let was_reachable = next.get(b);
            if was_reachable {
                earliest.get_or_insert(b);
                latest = Some(b);
            }
            if reachable && self.consumes(k + 1, b) {
                next.set(b);
            } else {
                next.unset(b);
            }
        }
        *reach = next;
    }

    /// [`Self::reach_next`] for any gap: marks every candidate range of every
    /// reachable position with a difference array.
    fn reach_next_by_ranges(&self, k: usize, reach: &mut Bits, delta: &mut [i64]) {
        let len = self.len();
        delta.fill(0);
        for b in (0..len).filter(|&b| reach.get(b)) {
            for (lo, hi) in self.candidates(k, b) {
                if lo < hi {
                    delta[lo] += 1;
                    delta[hi] -= 1;
                }
            }
        }
        let mut covering = 0;
        reach.clear();
        for (b, d) in delta.iter().take(len).enumerate() {
            covering += d;
            if covering > 0 && self.consumes(k + 1, b) {
                reach.set(b);
            }
        }
    }

    /// The timestamps of the `(?N)` steps on the greedy walk from `b0`.
    fn walk_timestamps(&self, levels: &[Bits], b0: usize) -> Vec<i64> {
        let mut timestamps = Vec::new();
        self.walk(levels, b0, |k, b| {
            if self.plan.consumers[k].is_some() {
                timestamps.push(self.events[b].timestamp_us);
            }
        });
        timestamps
    }

    fn execute_events(&self) -> Result<Vec<i64>, PatternError> {
        let len = self.len();
        let num_consumers = self.plan.consumers.len();
        if num_consumers == 0 {
            return Ok(Vec::new());
        }
        let levels = self.feasibility(num_consumers - 1, true)?;
        let b0 = levels[0].next_set(0, len);
        if b0 < len {
            return Ok(self.walk_timestamps(&levels, b0));
        }
        drop(levels);

        // No full match: find the furthest consumer any chain reaches.
        let mut reach = self.try_bits()?;
        for b in (0..len).filter(|&b| self.consumes(0, b)) {
            reach.set(b);
        }
        let mut delta = self.try_vec(len + 1, 0i64)?;
        let mut furthest = None;
        for k in 0..num_consumers {
            if reach.next_set(0, len) == len {
                break;
            }
            if self.plan.consumers[k].is_some() {
                furthest = Some(k);
            }
            if k + 1 == num_consumers {
                break;
            }
            self.reach_next(k, &mut reach, &mut delta);
        }
        drop((reach, delta));
        let Some(furthest) = furthest else {
            return Ok(Vec::new());
        };
        let levels = self.feasibility(furthest, false)?;
        let b0 = levels[0].next_set(0, len);
        debug_assert!(b0 < len, "the furthest consumer is reachable");
        Ok(self.walk_timestamps(&levels, b0))
    }
}

/// A fixed-size set of event positions, one bit each.
struct Bits(Vec<u64>);

impl Bits {
    fn get(&self, i: usize) -> bool {
        self.0[i / 64] >> (i % 64) & 1 == 1
    }

    fn set(&mut self, i: usize) {
        self.0[i / 64] |= 1 << (i % 64);
    }

    fn unset(&mut self, i: usize) {
        self.0[i / 64] &= !(1 << (i % 64));
    }

    fn clear(&mut self) {
        self.0.fill(0);
    }

    /// The smallest member `>= from`, or `len` (the number of positions) if
    /// there is none.
    fn next_set(&self, from: usize, len: usize) -> usize {
        if from >= len {
            return len;
        }
        let mut word = from / 64;
        let mut bits = self.0[word] & (u64::MAX << (from % 64));
        loop {
            if bits != 0 {
                return (word * 64 + bits.trailing_zeros() as usize).min(len);
            }
            word += 1;
            if word == self.0.len() {
                return len;
            }
            bits = self.0[word];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pattern::parser::parse_pattern;

    fn make_events(data: &[(i64, &[bool])]) -> Vec<Event> {
        data.iter()
            .map(|(ts, conds)| Event::from_bools(*ts, conds))
            .collect()
    }

    #[test]
    fn test_simple_match() {
        let pattern = parse_pattern("(?1)(?2)").unwrap();
        let events = make_events(&[(100, &[true, false]), (200, &[false, true])]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(result.matched);
    }

    #[test]
    fn test_simple_no_match() {
        let pattern = parse_pattern("(?1)(?2)").unwrap();
        let events = make_events(&[(100, &[false, true]), (200, &[true, false])]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(!result.matched);
    }

    #[test]
    fn test_wildcard_match() {
        let pattern = parse_pattern("(?1).*(?2)").unwrap();
        let events = make_events(&[
            (100, &[true, false]),
            (200, &[false, false]), // gap event
            (300, &[false, false]), // gap event
            (400, &[false, true]),
        ]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(result.matched);
    }

    #[test]
    fn test_one_event_gap() {
        let pattern = parse_pattern("(?1).(?2)").unwrap();
        let events = make_events(&[
            (100, &[true, false]),
            (200, &[false, false]), // exactly one event gap
            (300, &[false, true]),
        ]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(result.matched);
    }

    #[test]
    fn test_one_event_gap_too_many() {
        let pattern = parse_pattern("(?1).(?2)").unwrap();
        let events = make_events(&[
            (100, &[true, false]),
            (200, &[false, false]),
            (300, &[false, false]), // two events gap, not one
            (400, &[false, true]),
        ]);
        // The pattern (?1).(?2) requires exactly ONE event between (?1) and (?2)
        // Event at 200 is the "." and event at 300 needs to be (?2) but it's false
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(!result.matched);
    }

    #[test]
    fn test_time_constraint_satisfied() {
        let pattern = parse_pattern("(?1)(?t>=2)(?2)").unwrap();
        // Timestamps in microseconds, threshold in seconds
        let events = make_events(&[
            (0, &[true, false]),
            (3_000_000, &[false, true]), // 3 seconds later >= 2
        ]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(result.matched);
    }

    #[test]
    fn test_time_constraint_not_satisfied() {
        let pattern = parse_pattern("(?1)(?t>=5)(?2)").unwrap();
        let events = make_events(&[
            (0, &[true, false]),
            (3_000_000, &[false, true]), // 3 seconds < 5
        ]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(!result.matched);
    }

    #[test]
    fn test_count_non_overlapping() {
        let pattern = parse_pattern("(?1)(?2)").unwrap();
        let events = make_events(&[
            (100, &[true, false]),
            (200, &[false, true]),
            (300, &[true, false]),
            (400, &[false, true]),
        ]);
        let result = execute_pattern(&pattern, &events, true).unwrap();
        assert!(result.matched);
        assert_eq!(result.count, 2);
    }

    #[test]
    fn test_empty_events() {
        let pattern = parse_pattern("(?1)").unwrap();
        let result = execute_pattern(&pattern, &[], false).unwrap();
        assert!(!result.matched);
        assert_eq!(result.count, 0);
    }

    #[test]
    fn test_no_matching_condition() {
        let pattern = parse_pattern("(?1)").unwrap();
        let events = make_events(&[(100, &[false]), (200, &[false])]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(!result.matched);
    }

    #[test]
    fn test_wildcard_zero_events() {
        // .* can match zero events
        let pattern = parse_pattern("(?1).*(?2)").unwrap();
        let events = make_events(&[
            (100, &[true, true]), // both conditions true on same event
        ]);
        // (?1) matches event[0], .* matches 0 events, (?2) needs event[1] which doesn't exist
        // Actually, (?1) consumes event[0] and advances. .* matches 0 events.
        // (?2) tries event[1] which doesn't exist. So this should NOT match.
        // Unless event[0] has cond[1] = true and we can reuse it...
        // No - each step consumes events. (?1) consumed event[0], so (?2) needs another event.
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(!result.matched);
    }

    #[test]
    fn test_adjacent_match() {
        let pattern = parse_pattern("(?1).*(?2)").unwrap();
        let events = make_events(&[(100, &[true, false]), (200, &[false, true])]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(result.matched);
    }

    #[test]
    fn test_three_step_with_wildcards() {
        let pattern = parse_pattern("(?1).*(?2).*(?3)").unwrap();
        let events = make_events(&[
            (100, &[true, false, false]),
            (200, &[false, false, false]),
            (300, &[false, true, false]),
            (400, &[false, false, false]),
            (500, &[false, false, true]),
        ]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(result.matched);
    }

    #[test]
    fn test_time_lte_constraint() {
        let pattern = parse_pattern("(?1)(?t<=1)(?2)").unwrap();
        let events = make_events(&[
            (0, &[true, false]),
            (500_000, &[false, true]), // 0.5 seconds <= 1
        ]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(result.matched);
    }

    #[test]
    fn test_stacked_wildcards_no_match() {
        // Stacked `.*` (collapsed by the parser) over many non-matching
        // events: no hang, no match.
        let pattern = parse_pattern("(?1).*.*.*.*(?2)").unwrap();
        // Many events that don't match (?2) force extensive backtracking
        let mut event_data: Vec<(i64, &[bool])> = Vec::new();
        let conds_start: [bool; 2] = [true, false];
        let conds_mid: [bool; 2] = [false, false];
        event_data.push((0, &conds_start));
        for i in 1..100 {
            event_data.push((i, &conds_mid));
        }
        let events = make_events(&event_data);
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(!result.matched);
    }

    #[test]
    fn test_empty_pattern_steps() {
        // A pattern with no steps should not match anything
        let pattern = CompiledPattern { steps: vec![] };
        let events = make_events(&[(100, &[true])]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(!result.matched);
        assert_eq!(result.count, 0);
    }

    #[test]
    fn test_count_all_no_matches() {
        let pattern = parse_pattern("(?1)(?2)").unwrap();
        let events = make_events(&[(100, &[false, true]), (200, &[false, true])]);
        let result = execute_pattern(&pattern, &events, true).unwrap();
        assert!(!result.matched);
        assert_eq!(result.count, 0);
    }

    #[test]
    fn test_time_eq_constraint() {
        let pattern = parse_pattern("(?1)(?t==2)(?2)").unwrap();
        let events = make_events(&[
            (0, &[true, false]),
            (2_000_000, &[false, true]), // exactly 2 seconds
        ]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(result.matched);
    }

    #[test]
    fn test_time_ne_constraint() {
        let pattern = parse_pattern("(?1)(?t!=2)(?2)").unwrap();
        let events = make_events(&[
            (0, &[true, false]),
            (3_000_000, &[false, true]), // 3 seconds != 2
        ]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(result.matched);
    }

    #[test]
    fn test_time_gt_constraint() {
        let pattern = parse_pattern("(?1)(?t>5)(?2)").unwrap();
        let events = make_events(&[
            (0, &[true, false]),
            (6_000_000, &[false, true]), // 6 > 5
        ]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(result.matched);
    }

    #[test]
    fn test_time_lt_constraint() {
        let pattern = parse_pattern("(?1)(?t<5)(?2)").unwrap();
        let events = make_events(&[
            (0, &[true, false]),
            (4_000_000, &[false, true]), // 4 < 5
        ]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(result.matched);
    }

    #[test]
    fn test_single_event_single_condition() {
        let pattern = parse_pattern("(?1)").unwrap();
        let events = make_events(&[(100, &[true])]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(result.matched);
    }

    #[test]
    fn test_wildcard_at_end() {
        // .* at the end of pattern should still match
        let pattern = parse_pattern("(?1).*").unwrap();
        let events = make_events(&[(100, &[true]), (200, &[false])]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(result.matched);
    }

    #[test]
    fn test_count_three_non_overlapping() {
        let pattern = parse_pattern("(?1)(?2)").unwrap();
        let events = make_events(&[
            (100, &[true, false]),
            (200, &[false, true]),
            (300, &[true, false]),
            (400, &[false, true]),
            (500, &[true, false]),
            (600, &[false, true]),
        ]);
        let result = execute_pattern(&pattern, &events, true).unwrap();
        assert_eq!(result.count, 3);
    }

    // --- Session 4: Mutation-killing tests for identified gaps ---

    #[test]
    fn test_one_event_dot_with_time_constraint() {
        // Kills mutant: removing last_match_ts update in OneEvent handler.
        // If `.` doesn't set last_match_ts, the following time constraint
        // would use the wrong baseline timestamp (or None).
        let pattern = parse_pattern("(?1).(?t<=3)(?2)").unwrap();
        let events = make_events(&[
            (0, &[true, false]),
            (1_000_000, &[false, false]), // matched by `.`
            (3_000_000, &[false, true]),  // 2s after the `.` event, <= 3
        ]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(result.matched);

        // Now verify the time constraint uses the `.` event's timestamp, not (?1)'s
        let pattern2 = parse_pattern("(?1).(?t<=1)(?2)").unwrap();
        let events2 = make_events(&[
            (0, &[true, false]),
            (1_000_000, &[false, false]), // matched by `.` at 1s
            (3_000_000, &[false, true]),  // 2s after `.`, > 1s limit
        ]);
        let result2 = execute_pattern(&pattern2, &events2, false).unwrap();
        assert!(!result2.matched);
    }

    #[test]
    fn test_time_constraint_without_anchor_is_rejected() {
        // A time constraint before any `(?N)` or `.` has nothing to measure
        // from. It used to be treated as always true; ClickHouse 26.9.8.3
        // answers `(?t>0)(?1)` over one c1 event with 0 where that gave true.
        for p in ["(?t<=5)(?1)", ".*(?t<5)(?1)", "(?t<1)(?t<2)(?1)"] {
            let err = parse_pattern(p).unwrap_err();
            assert!(err.message.contains("must follow an event"), "{p}: {err}");
        }
        // After `.` the constraint is anchored at the event `.` consumed.
        let pattern = parse_pattern(".(?t<=5)(?1)").unwrap();
        let events = make_events(&[(100, &[false]), (100, &[true])]);
        assert!(execute_pattern(&pattern, &events, false).unwrap().matched);
    }

    #[test]
    fn test_adjacent_conditions_with_wildcard_keep_adjacency() {
        // `(?1)(?2).*(?3)` requires (?2) on the event right after (?1). The
        // wildcard fast path used to drop that requirement. ClickHouse
        // 26.9.8.3 returns 0 for these events; the old code returned true.
        let pattern = parse_pattern("(?1)(?2).*(?3)").unwrap();
        let events = make_events(&[
            (1, &[true, false, false]),
            (2, &[false, false, true]),
            (3, &[false, true, false]),
            (4, &[false, false, true]),
        ]);
        let result = execute_pattern(&pattern, &events, true).unwrap();
        assert!(!result.matched);
        assert_eq!(result.count, 0);
        // ClickHouse counts 1 for `(?1)(?1).*` over c1,c1,c1,c2,c1 (the old
        // code counted 2).
        let pattern = parse_pattern("(?1)(?1).*").unwrap();
        let events = make_events(&[
            (0, &[true, false]),
            (0, &[true, false]),
            (0, &[true, false]),
            (0, &[false, true]),
            (1, &[true, false]),
        ]);
        assert_eq!(execute_pattern(&pattern, &events, true).unwrap().count, 1);
    }

    #[test]
    fn test_count_of_pattern_matching_no_events_terminates() {
        // `.*` matches the empty sequence; counting used to restart at the
        // same position forever. ClickHouse 26.9.8.3 counts one match per
        // event (2 here), advancing one event after an empty match.
        let pattern = parse_pattern(".*").unwrap();
        let events = make_events(&[(1, &[true, false]), (2, &[false, true])]);
        assert_eq!(execute_pattern(&pattern, &events, true).unwrap().count, 2);
        let pattern = parse_pattern("(?1).*").unwrap();
        let events = make_events(&[(1, &[true]), (2, &[true]), (3, &[true])]);
        assert_eq!(execute_pattern(&pattern, &events, true).unwrap().count, 3);
    }

    #[test]
    fn test_time_constraint_microsecond_to_second_conversion() {
        // Kills mutant: replacing `/` with `*` in elapsed_us / MICROS_PER_SECOND.
        // Uses non-trivial values where the division matters.
        // 1_500_000 µs = 1.5s, truncated to 1s.
        // With (?t>=2), 1s < 2s → should NOT match.
        let pattern = parse_pattern("(?1)(?t>=2)(?2)").unwrap();
        let events = make_events(&[
            (0, &[true, false]),
            (1_500_000, &[false, true]), // 1.5s → 1s (integer division) < 2
        ]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(!result.matched);

        // 2_500_000 µs = 2.5s, truncated to 2s. With (?t>=2), 2s >= 2 → match.
        let events2 = make_events(&[(0, &[true, false]), (2_500_000, &[false, true])]);
        let result2 = execute_pattern(&pattern, &events2, false).unwrap();
        assert!(result2.matched);
    }

    #[test]
    fn test_lazy_matching_prefers_advance_over_consume() {
        // Kills mutant: swapping AnyEvents push order (lazy → greedy).
        // With lazy matching, .* matches as few events as possible,
        // enabling more non-overlapping matches when count_all=true.
        let pattern = parse_pattern("(?1).*(?2)").unwrap();
        let events = make_events(&[
            (100, &[true, false]),
            (200, &[false, true]), // lazy: (?2) matches here immediately
            (300, &[true, false]), // start of second match
            (400, &[false, true]), // lazy: (?2) matches here immediately
        ]);
        let result = execute_pattern(&pattern, &events, true).unwrap();
        // Lazy: match (0→1), then (2→3) = 2 non-overlapping matches
        assert!(result.matched);
        assert_eq!(result.count, 2);
    }

    #[test]
    fn test_step_completion_boundary() {
        // Kills mutant: replacing `>=` with `>` in step completion check.
        // A pattern with 2 steps should complete when step_idx == 2 == steps.len().
        let pattern = parse_pattern("(?1)(?2)").unwrap();
        assert_eq!(pattern.steps.len(), 2);
        let events = make_events(&[(100, &[true, false]), (200, &[false, true])]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(result.matched);
    }

    #[test]
    fn test_match_end_index_for_non_overlapping_count() {
        // Kills mutant: altering match_end return value logic.
        // Verifies that non-overlapping count correctly advances past the match.
        let pattern = parse_pattern("(?1)(?2)").unwrap();
        // Events: c1, c2, c1, c2, c1, c2
        // Matches: (0,1), (2,3), (4,5) = 3 non-overlapping
        let events = make_events(&[
            (100, &[true, false]),
            (200, &[false, true]),
            (300, &[true, false]),
            (400, &[false, true]),
            (500, &[true, false]),
            (600, &[false, true]),
        ]);
        let result = execute_pattern(&pattern, &events, true).unwrap();
        assert_eq!(result.count, 3);

        // Adjacent events that share: c1, c1c2, c2
        // First match: event 0 (c1) → event 1 (c2). match_end = 1.
        // search_start = 2. Event 2 has c2 only, no c1. No second match.
        let events2 = make_events(&[
            (100, &[true, false]),
            (200, &[true, true]), // both conditions
            (300, &[false, true]),
        ]);
        let result2 = execute_pattern(&pattern, &events2, true).unwrap();
        assert_eq!(result2.count, 1);
    }

    #[test]
    fn test_any_events_at_end_of_stream() {
        // Kills mutant: not handling .* at end of stream when events exhausted.
        // .* should match zero remaining events at the end.
        let pattern = parse_pattern("(?1).*").unwrap();
        let events = make_events(&[(100, &[true])]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(result.matched);
    }

    // --- execute_pattern_events tests ---

    #[test]
    fn test_events_simple_match() {
        let pattern = parse_pattern("(?1)(?2)").unwrap();
        let events = make_events(&[(100, &[true, false]), (200, &[false, true])]);
        let result = execute_pattern_events(&pattern, &events).unwrap();
        assert_eq!(result, vec![100, 200]);
    }

    #[test]
    fn test_events_no_match() {
        // No complete match: ClickHouse's sequenceMatchEvents returns the
        // longest partial chain — here (?1) matched at 200.
        let pattern = parse_pattern("(?1)(?2)").unwrap();
        let events = make_events(&[(100, &[false, true]), (200, &[true, false])]);
        let result = execute_pattern_events(&pattern, &events).unwrap();
        assert_eq!(result, vec![200]);
    }

    #[test]
    fn test_events_with_wildcard() {
        let pattern = parse_pattern("(?1).*(?2)").unwrap();
        let events = make_events(&[
            (100, &[true, false]),
            (200, &[false, false]),
            (300, &[false, true]),
        ]);
        let result = execute_pattern_events(&pattern, &events).unwrap();
        // Only condition timestamps, not wildcard
        assert_eq!(result, vec![100, 300]);
    }

    #[test]
    fn test_events_empty_input() {
        let pattern = parse_pattern("(?1)").unwrap();
        let result = execute_pattern_events(&pattern, &[]).unwrap();
        assert_eq!(result, Vec::<i64>::new());
    }

    #[test]
    fn test_events_three_conditions() {
        let pattern = parse_pattern("(?1).*(?2).*(?3)").unwrap();
        let events = make_events(&[
            (10, &[true, false, false]),
            (20, &[false, true, false]),
            (30, &[false, false, true]),
        ]);
        let result = execute_pattern_events(&pattern, &events).unwrap();
        assert_eq!(result, vec![10, 20, 30]);
    }

    #[test]
    fn test_events_with_time_constraint() {
        let pattern = parse_pattern("(?1)(?t>=2)(?2)").unwrap();
        let events = make_events(&[(0, &[true, false]), (3_000_000, &[false, true])]);
        let result = execute_pattern_events(&pattern, &events).unwrap();
        assert_eq!(result, vec![0, 3_000_000]);
    }

    #[test]
    fn test_events_with_one_event() {
        let pattern = parse_pattern("(?1).(?2)").unwrap();
        let events = make_events(&[
            (100, &[true, false]),
            (200, &[false, false]),
            (300, &[false, true]),
        ]);
        let result = execute_pattern_events(&pattern, &events).unwrap();
        assert_eq!(result, vec![100, 300]);
    }

    // --- Fast path tests ---

    #[test]
    fn test_fast_adjacent_skip_correctness() {
        // Regression test: the fast_adjacent path must not skip valid starting
        // positions when an intermediate condition check fails.
        // Events: c1c2, c1, c2. Pattern (?1)(?2).
        // Position 0: events[0]=c1c2, events[1]=c1. c1 doesn't have condition 1 → fail.
        // Position 1: events[1]=c1, events[2]=c2. Match!
        let pattern = parse_pattern("(?1)(?2)").unwrap();
        let events = make_events(&[
            (100, &[true, true]),  // c1c2
            (200, &[true, false]), // c1
            (300, &[false, true]), // c2
        ]);
        let result = execute_pattern(&pattern, &events, true).unwrap();
        assert_eq!(result.count, 1);
    }

    #[test]
    fn test_fast_adjacent_three_step() {
        // Three adjacent conditions: (?1)(?2)(?3)
        let pattern = parse_pattern("(?1)(?2)(?3)").unwrap();
        let events = make_events(&[
            (100, &[true, false, false]),
            (200, &[false, true, false]),
            (300, &[false, false, true]),
        ]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(result.matched);
    }

    #[test]
    fn test_fast_wildcard_count() {
        // Wildcard-separated pattern counting: (?1).*(?2)
        let pattern = parse_pattern("(?1).*(?2)").unwrap();
        let events = make_events(&[
            (100, &[true, false]),
            (200, &[false, false]), // gap
            (300, &[false, true]),
            (400, &[true, false]),
            (500, &[false, true]),
        ]);
        let result = execute_pattern(&pattern, &events, true).unwrap();
        assert_eq!(result.count, 2);
    }

    #[test]
    fn test_fast_wildcard_no_match() {
        // Wildcard pattern where condition 2 never fires
        let pattern = parse_pattern("(?1).*(?2)").unwrap();
        let events = make_events(&[
            (100, &[true, false]),
            (200, &[true, false]),
            (300, &[true, false]),
        ]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(!result.matched);
    }

    #[test]
    fn test_fast_adjacent_insufficient_events() {
        // Fewer events than pattern steps
        let pattern = parse_pattern("(?1)(?2)(?3)").unwrap();
        let events = make_events(&[(100, &[true, false, false]), (200, &[false, true, false])]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(!result.matched);
    }

    #[test]
    fn test_classify_time_constraint_is_complex() {
        // Patterns with time constraints use the general matcher, not fast paths.
        let pattern = parse_pattern("(?1)(?t<=5)(?2)").unwrap();
        let events = make_events(&[(0, &[true, false]), (3_000_000, &[false, true])]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(result.matched);
    }

    #[test]
    fn test_classify_one_event_is_complex() {
        // Patterns with `.` (OneEvent) use the general matcher.
        let pattern = parse_pattern("(?1).(?2)").unwrap();
        let events = make_events(&[
            (100, &[true, false]),
            (200, &[false, false]),
            (300, &[false, true]),
        ]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(result.matched);
    }

    #[test]
    fn test_time_constraint_after_wildcard() {
        // Kills mutant: incorrect last_match_ts propagation through .*.
        // After .* matches, the time constraint should use the last
        // matched event's timestamp (from before .*), not the current event.
        let pattern = parse_pattern("(?1).*(?t<=3)(?2)").unwrap();
        let events = make_events(&[
            (0, &[true, false]),
            (1_000_000, &[false, false]), // consumed by .*
            (2_000_000, &[false, true]),  // 2s from (?1) match, <= 3
        ]);
        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(result.matched);

        // Time constraint too tight for the gap
        let pattern2 = parse_pattern("(?1).*(?t<=1)(?2)").unwrap();
        let events2 = make_events(&[
            (0, &[true, false]),
            (1_000_000, &[false, false]),
            (5_000_000, &[false, true]), // 5s from (?1), > 1
        ]);
        let result2 = execute_pattern(&pattern2, &events2, false).unwrap();
        assert!(!result2.matched);
    }

    // --- execute_pattern_events: additional edge case coverage ---

    #[test]
    fn test_events_wildcard_plus_time_constraint() {
        // Verifies timestamp collection when pattern contains both .* and (?t<=N).
        // Only condition timestamps should appear in the result.
        let pattern = parse_pattern("(?1).*(?t<=5)(?2)").unwrap();
        let events = make_events(&[
            (0, &[true, false]),
            (1_000_000, &[false, false]), // consumed by .*
            (3_000_000, &[false, true]),  // 3s from (?1), <= 5
        ]);
        let result = execute_pattern_events(&pattern, &events).unwrap();
        assert_eq!(result, vec![0, 3_000_000]);
    }

    #[test]
    fn test_events_wildcard_time_constraint_fails() {
        // Time constraint not satisfiable: no complete match, so the longest
        // partial chain — just (?1) at t=0 — is returned.
        let pattern = parse_pattern("(?1).*(?t<=1)(?2)").unwrap();
        let events = make_events(&[
            (0, &[true, false]),
            (1_000_000, &[false, false]),
            (5_000_000, &[false, true]), // 5s from (?1), > 1
        ]);
        let result = execute_pattern_events(&pattern, &events).unwrap();
        assert_eq!(result, vec![0]);
    }

    #[test]
    fn test_events_stacked_wildcards_and_gates() {
        // Consecutive `.*` runs are collapsed by the parser, so the classic
        // pathological shape stays on the fast path and simply reports
        // no-match.
        let pattern = parse_pattern("(?1).*.*.*.*(?2)").unwrap();
        let mut event_data: Vec<(i64, &[bool])> = Vec::new();
        let conds_start: [bool; 2] = [true, false];
        let conds_mid: [bool; 2] = [false, false];
        event_data.push((0, &conds_start));
        for i in 1..100 {
            event_data.push((i, &conds_mid));
        }
        let events = make_events(&event_data);
        let result = execute_pattern_events(&pattern, &events).unwrap();
        // No (?2) anywhere: the longest partial chain is (?1) at t=0.
        assert_eq!(result, vec![0]);

        // A pattern that cannot be normalized (wildcards interleaved with
        // time constraints) used to exhaust the backtracking search's
        // exploration budget and abort the query. It now completes, with
        // the same longest-partial answer.
        let adversarial =
            parse_pattern("(?1).*(?t>=0).*(?t>=0).*(?t>=0).*(?t>=0).*(?t>=0).*(?2)").unwrap();
        let mut big: Vec<Event> = vec![Event::from_bools(0, &[true, false])];
        for i in 1..3_000i64 {
            big.push(Event::new(i, 0b100));
        }
        assert_eq!(execute_pattern_events(&adversarial, &big).unwrap(), vec![0]);
    }

    #[test]
    fn test_events_empty_pattern() {
        // Empty pattern steps match nothing: empty result.
        let pattern = CompiledPattern { steps: vec![] };
        let events = make_events(&[(100, &[true])]);
        let result = execute_pattern_events(&pattern, &events).unwrap();
        assert_eq!(result, Vec::<i64>::new());
    }

    #[test]
    fn test_events_wildcard_zero_events_between_conditions() {
        // .* matching zero events between conditions.
        // (?1) consumes event[0], .* matches zero, (?2) needs event[1].
        let pattern = parse_pattern("(?1).*(?2)").unwrap();
        let events = make_events(&[(100, &[true, false]), (200, &[false, true])]);
        let result = execute_pattern_events(&pattern, &events).unwrap();
        assert_eq!(result, vec![100, 200]);
    }

    #[test]
    fn test_events_one_event_gap_fails() {
        // (?1).(?2) with two gap events — no complete match because `.`
        // matches exactly one event; longest partial is (?1) at 100.
        let pattern = parse_pattern("(?1).(?2)").unwrap();
        let events = make_events(&[
            (100, &[true, false]),
            (200, &[false, false]),
            (300, &[false, false]),
            (400, &[false, true]),
        ]);
        let result = execute_pattern_events(&pattern, &events).unwrap();
        assert_eq!(result, vec![100]);
    }

    #[test]
    fn test_events_wildcard_at_end_of_stream() {
        // .* at end of pattern when events run out. The .* should match
        // zero remaining events and the pattern should succeed.
        let pattern = parse_pattern("(?1).*").unwrap();
        let events = make_events(&[(100, &[true])]);
        let result = execute_pattern_events(&pattern, &events).unwrap();
        // Only one condition timestamp collected
        assert_eq!(result, vec![100]);
    }

    #[test]
    fn test_events_time_constraint_after_one_event() {
        // `.` anchors a following time constraint, so event collection sees
        // the constraint measured from the `.` event.
        let pattern = parse_pattern(".(?t<=5)(?1)").unwrap();
        let events = make_events(&[(100, &[false]), (100, &[true])]);
        let result = execute_pattern_events(&pattern, &events).unwrap();
        assert_eq!(result, vec![100]);
    }

    #[test]
    fn test_events_lazy_matching_collects_earliest() {
        // Verifies lazy matching during event collection: .* prefers
        // advancing the pattern over consuming events, so the earliest
        // matching (?2) timestamp is collected.
        let pattern = parse_pattern("(?1).*(?2)").unwrap();
        let events = make_events(&[
            (100, &[true, false]),
            (200, &[false, true]), // earliest (?2) — lazy match picks this
            (300, &[false, false]),
            (400, &[false, true]), // later (?2) — greedy would pick this
        ]);
        let result = execute_pattern_events(&pattern, &events).unwrap();
        assert_eq!(result, vec![100, 200]);
    }
}

#[cfg(test)]
mod large_gap_tests {
    use super::*;
    use crate::pattern::parser::parse_pattern;

    /// A complex pattern (the time constraint rules out the fast paths) over
    /// a large gap span must still find the match. (The old backtracking
    /// search once reported a false no-match here when its exploration
    /// budget ran out.)
    #[test]
    fn test_large_gap_span_still_matches_complex_pattern() {
        let pattern = parse_pattern("(?1).*(?t>=1)(?2)").unwrap();
        let mut events = Vec::new();
        events.push(Event::from_bools(0, &[true, false]));
        for i in 0..50_000i64 {
            // Gap events: condition 3 fires so they pass update() filters,
            // but they match neither pattern condition.
            events.push(Event::new(1_000_000 + i, 0b100));
        }
        events.push(Event::from_bools(60_000_000, &[false, true]));

        let result = execute_pattern(&pattern, &events, false).unwrap();
        assert!(result.matched, "the match exists");
    }

    /// The events-collecting variant has the same requirement.
    #[test]
    fn test_large_gap_span_events_variant() {
        let pattern = parse_pattern("(?1).(?t>=0)(?2)").unwrap();
        let mut events = Vec::new();
        events.push(Event::from_bools(0, &[true, false]));
        events.push(Event::new(500_000, 0b100));
        events.push(Event::from_bools(1_000_000, &[false, true]));
        // Long tail after the match must not matter.
        for i in 0..20_000i64 {
            events.push(Event::new(2_000_000 + i, 0b100));
        }
        let timestamps = execute_pattern_events(&pattern, &events).unwrap();
        assert_eq!(timestamps, vec![0, 1_000_000]);
    }
}

#[cfg(test)]
mod time_semantics_tests {
    use super::*;
    use crate::pattern::parser::parse_pattern;

    /// Elapsed time is floored to whole seconds before comparison — the
    /// faithful generalization of `ClickHouse`'s `DateTime` (whole-second)
    /// behavior to microsecond timestamps. 2.5s elapsed counts as 2.
    #[test]
    fn test_elapsed_seconds_floor_for_lte() {
        let pattern = parse_pattern("(?1)(?t<=2)(?2)").unwrap();
        let events = vec![
            Event::from_bools(0, &[true, false]),
            Event::from_bools(2_500_000, &[false, true]), // 2.5s -> floor 2
        ];
        assert!(execute_pattern(&pattern, &events, false).unwrap().matched);
    }

    /// (?t==N) therefore means "elapsed within [N, N+1) seconds".
    #[test]
    fn test_elapsed_seconds_floor_for_eq() {
        let pattern = parse_pattern("(?1)(?t==2)(?2)").unwrap();
        let events = vec![
            Event::from_bools(0, &[true, false]),
            Event::from_bools(2_900_000, &[false, true]), // 2.9s -> floor 2
        ];
        assert!(execute_pattern(&pattern, &events, false).unwrap().matched);

        let events = vec![
            Event::from_bools(0, &[true, false]),
            Event::from_bools(3_000_000, &[false, true]), // 3.0s -> floor 3
        ];
        assert!(!execute_pattern(&pattern, &events, false).unwrap().matched);
    }

    /// `ClickHouse` gap-skip semantics: `(?1)(?t<=N)(?2)` tolerates
    /// non-matching events between the two conditions.
    #[test]
    fn test_time_constraint_skips_gap_events() {
        let pattern = parse_pattern("(?1)(?t<=10)(?2)").unwrap();
        let events = vec![
            Event::from_bools(0, &[true, false]),
            Event::new(2_000_000, 0b100), // gap event (other condition)
            Event::new(3_000_000, 0b100), // gap event
            Event::from_bools(5_000_000, &[false, true]),
        ];
        assert!(execute_pattern(&pattern, &events, false).unwrap().matched);
    }

    /// The gate still rejects matches outside the window even when skipping.
    #[test]
    fn test_time_constraint_gate_still_enforced_with_gaps() {
        let pattern = parse_pattern("(?1)(?t<=2)(?2)").unwrap();
        let events = vec![
            Event::from_bools(0, &[true, false]),
            Event::new(1_000_000, 0b100), // gap inside window
            Event::from_bools(5_000_000, &[false, true]), // outside window
        ];
        assert!(!execute_pattern(&pattern, &events, false).unwrap().matched);
    }

    /// `ClickHouse` end-of-events rule: trailing `(?t<=N)` / `(?t<N)` /
    /// `(?t>=0)` match the empty remainder.
    #[test]
    fn test_trailing_constraints_vacuous_at_end() {
        let events = vec![Event::from_bools(0, &[true])];
        for pattern_str in ["(?1)(?t<=5)", "(?1)(?t<5)", "(?1)(?t>=0)"] {
            let pattern = parse_pattern(pattern_str).unwrap();
            assert!(
                execute_pattern(&pattern, &events, false).unwrap().matched,
                "{pattern_str} must match at end of events"
            );
        }
        // Trailing constraints that need a future event do not match.
        for pattern_str in ["(?1)(?t>=1)", "(?1)(?t>0)", "(?1)(?t==0)"] {
            let pattern = parse_pattern(pattern_str).unwrap();
            assert!(
                !execute_pattern(&pattern, &events, false).unwrap().matched,
                "{pattern_str} must not match at end of events"
            );
        }
    }

    /// `(?t>=N)` waits past arbitrarily many early events.
    #[test]
    fn test_gte_waits_for_later_events() {
        let pattern = parse_pattern("(?1)(?t>=4)(?2)").unwrap();
        let events = vec![
            Event::from_bools(0, &[true, false]),
            Event::from_bools(1_000_000, &[false, true]), // too early
            Event::from_bools(2_000_000, &[false, true]), // too early
            Event::from_bools(5_000_000, &[false, true]), // 5s >= 4 ✓
        ];
        assert!(execute_pattern(&pattern, &events, false).unwrap().matched);
        let timestamps = execute_pattern_events(&pattern, &events).unwrap();
        assert_eq!(timestamps, vec![0, 5_000_000]);
    }
}

/// Differential tests: the matcher must reproduce the original backtracking
/// search (`reference_nfa`) result for result.
#[cfg(test)]
mod differential_tests {
    use super::*;
    use crate::pattern::parser::parse_pattern;
    use crate::pattern::reference_nfa;
    use proptest::prelude::*;

    /// One pattern token: a consumer, `.*`, or a time constraint.
    fn token() -> impl Strategy<Value = String> {
        prop_oneof![
            3 => (1usize..=3).prop_map(|c| format!("(?{c})")),
            1 => Just(".".to_string()),
            2 => Just(".*".to_string()),
            3 => (
                prop::sample::select(vec![">=", "<=", ">", "<", "==", "!="]),
                0i64..4,
            )
                .prop_map(|(op, n)| format!("(?t{op}{n})")),
        ]
    }

    /// A consuming step: `(?N)` or `.`.
    fn consumer() -> impl Strategy<Value = String> {
        prop_oneof![
            3 => (1usize..=3).prop_map(|c| format!("(?{c})")),
            1 => Just(".".to_string()),
        ]
    }

    /// Valid patterns by construction: an optional leading `.*`, a consumer
    /// (time constraints need one before them), then any tokens; plus the
    /// consumer-free pattern `.*`.
    fn pattern() -> impl Strategy<Value = CompiledPattern> {
        prop_oneof![
            20 => (any::<bool>(), consumer(), prop::collection::vec(token(), 0..7)).prop_map(
                |(lead, first, rest)| {
                    let lead = if lead { ".*" } else { "" };
                    format!("{lead}{first}{}", rest.concat())
                }
            ),
            1 => Just(".*".to_string()),
        ]
        .prop_map(|text| parse_pattern(&text).expect("valid by construction"))
    }

    /// Small sorted event sets with many timestamp ties and sub-second
    /// offsets, so the floored-seconds gates hit their boundaries.
    fn events() -> impl Strategy<Value = Vec<Event>> {
        prop::collection::vec(
            (
                0i64..8,
                prop::sample::select(vec![0i64, 400_000, 999_999]),
                0u32..8,
            ),
            0..14,
        )
        .prop_map(|raw| {
            let mut events: Vec<Event> = raw
                .into_iter()
                .map(|(s, us, conds)| Event::new(s * 1_000_000 + us, conds))
                .collect();
            events.sort_by_key(|e| e.timestamp_us);
            events
        })
    }

    fn reference_result(
        pattern: &CompiledPattern,
        events: &[Event],
        count_all: bool,
    ) -> Option<(bool, usize)> {
        if events.is_empty() {
            return Some((false, 0));
        }
        reference_nfa::execute_pattern_nfa(pattern, events, count_all)
            .ok()
            .map(|r| (r.matched, r.count))
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(20_000))]

        #[test]
        fn matches_reference(pattern in pattern(), events in events()) {
            for count_all in [false, true] {
                // The reference aborts on its exploration budget; such
                // inputs carry no expected value.
                let Some(expected) = reference_result(&pattern, &events, count_all) else {
                    continue;
                };
                let actual = execute_pattern(&pattern, &events, count_all).unwrap();
                prop_assert_eq!(
                    (actual.matched, actual.count),
                    expected,
                    "count_all={} pattern={:?}",
                    count_all,
                    pattern.steps
                );
            }
        }

        #[test]
        fn events_match_reference(pattern in pattern(), events in events()) {
            if let Ok(expected) = reference_nfa::execute_pattern_events(&pattern, &events) {
                prop_assert_eq!(execute_pattern_events(&pattern, &events).unwrap(), expected);
            }
        }
    }

    /// The input shape behind the quadratic slowdown: `.*` before a time
    /// constraint, a match that never completes, and a large group. The
    /// backtracking search re-scanned the remaining events from every start
    /// (10.8 s at 32,000 events in a release build); this must stay fast.
    #[test]
    fn wildcard_then_gate_scales_linearly() {
        let pattern = parse_pattern("(?1).*(?t<5)(?2).*(?3)").unwrap();
        let n = 1_000_000i64;
        // Every event satisfies (?1) and (?2); (?3) never fires.
        let events: Vec<Event> = (0..n).map(|i| Event::new(i * 1_000, 0b011)).collect();
        let started = std::time::Instant::now();
        assert!(!execute_pattern(&pattern, &events, false).unwrap().matched);
        assert_eq!(execute_pattern(&pattern, &events, true).unwrap().count, 0);
        assert_eq!(execute_pattern_events(&pattern, &events).unwrap().len(), 2);
        // Generous bound for unoptimized test builds; the quadratic search
        // needs hours for this input.
        assert!(
            started.elapsed() < std::time::Duration::from_secs(60),
            "took {:?}",
            started.elapsed()
        );
    }
}

#[cfg(test)]
mod memory_tests {
    use super::*;
    use crate::pattern::parser::parse_pattern;

    /// An allocation the system cannot satisfy is a `PatternError`, not an
    /// abort of the host process.
    #[test]
    fn failed_allocation_is_an_error() {
        let pattern = parse_pattern("(?1).(?2)").unwrap();
        let events = vec![Event::new(0, 0b01), Event::new(1, 0b10)];
        let matcher = Matcher::new(&pattern, &events);
        let err = matcher.try_vec(usize::MAX / 2, 0u64).unwrap_err();
        assert!(err.message.contains("out of memory"), "{err}");
        assert!(err.message.contains("3 steps against 2 events"), "{err}");
    }

    /// `Bits::next_set` across word boundaries and at the end.
    #[test]
    fn bits_next_set() {
        let mut bits = Bits(vec![0; 3]);
        for i in [0, 63, 64, 130] {
            bits.set(i);
        }
        assert_eq!(bits.next_set(0, 140), 0);
        assert_eq!(bits.next_set(1, 140), 63);
        assert_eq!(bits.next_set(64, 140), 64);
        assert_eq!(bits.next_set(65, 140), 130);
        assert_eq!(bits.next_set(131, 140), 140);
        assert_eq!(bits.next_set(140, 140), 140);
        assert!(bits.get(130) && !bits.get(129));
        bits.clear();
        assert_eq!(bits.next_set(0, 140), 140);
    }
}
