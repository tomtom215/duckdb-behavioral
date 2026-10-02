// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! `window_funnel` — Aggregate function for conversion funnel analysis.
//!
//! Searches for the longest chain of events `cond1 -> cond2 -> ... -> condN`
//! where each subsequent event occurs within `window_size` of the **first**
//! event in the chain. Returns an integer 0..N indicating the max step reached.
//!
//! Follows `ClickHouse`'s `windowFunnel` (`AggregateFunctionWindowFunnel.cpp`,
//! v26.9.8.3): each event contributes one entry per condition it satisfies,
//! entries are visited in `(timestamp, condition)` order, and per funnel level
//! the chain with the latest entry is kept. Differential testing against
//! `ClickHouse` 26.9.8.3 agrees for every mode combination except where
//! `ClickHouse` itself is defective (see [`FunnelMode::STRICT_INCREASE`] and
//! [`FunnelMode::STRICT_ONCE`]).
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
//! Modes combine (comma-separated in SQL), as in `ClickHouse`:
//!
//! - **Default**: an event satisfying several conditions can fill several
//!   steps, including the entry event itself.
//! - **`strict_deduplication`** (also accepted as `'strict'`, which
//!   `ClickHouse` now rejects): a condition firing again for a step already
//!   reached stops the scan.
//! - **`strict_order`**: any other event between steps (one matching no
//!   condition, or a step arriving before its predecessor) stops the scan.
//! - **`strict_increase`**: a step must be strictly later than the step before
//!   it. `'timestamp_dedup'` (an extension mode) means the same.
//! - **`strict_once`**: an event fills at most one step of a chain.
//! - **`allow_reentry`** (requires `strict_order`): a step arriving before its
//!   predecessor is skipped instead of stopping the scan.

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
/// Bit 0 (0x01): STRICT             (ClickHouse: 'strict_deduplication'; 'strict' alias)
/// Bit 1 (0x02): STRICT_ORDER       (ClickHouse: 'strict_order')
/// Bit 2 (0x04): STRICT_DEDUPLICATION (Extension: 'timestamp_dedup')
/// Bit 3 (0x08): STRICT_INCREASE    (ClickHouse: 'strict_increase')
/// Bit 4 (0x10): STRICT_ONCE        (ClickHouse: 'strict_once')
/// Bit 5 (0x20): ALLOW_REENTRY      (ClickHouse: 'allow_reentry')
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FunnelMode(u8);

impl FunnelMode {
    /// Default mode: no constraints beyond the basic greedy scan.
    pub const DEFAULT: Self = Self(0);

    /// `'strict_deduplication'`: a condition firing again for a step that has
    /// already been reached (by any chain, even one whose window has passed)
    /// stops the scan. `'strict'` is accepted as an alias for backward
    /// compatibility; `ClickHouse` 26.9 rejects it.
    pub const STRICT: Self = Self(0x01);

    /// `'strict_order'`: once a chain has been entered, an event matching no
    /// condition, or a step arriving before its predecessor has been reached,
    /// stops the scan. A repeated entry or step does not.
    pub const STRICT_ORDER: Self = Self(0x02);

    /// **Extension mode** `'timestamp_dedup'` (not in `ClickHouse`): a step
    /// cannot be filled at the timestamp of the step before it, which is
    /// exactly [`STRICT_INCREASE`](Self::STRICT_INCREASE); kept as an alias.
    pub const STRICT_DEDUPLICATION: Self = Self(0x04);

    /// `'strict_increase'`: each step must be strictly later than the step
    /// before it, checked for every step (so one event cannot fill two).
    ///
    /// Without `strict_once`, `ClickHouse` keeps a single chain per level and
    /// loses a valid chain when a later one overwrites it (rows `c1@0, c1@1,
    /// c2@1` give 1 instead of 2). This implementation keeps the best chain
    /// that ends before each timestamp, so it can return more steps than
    /// `ClickHouse`, or fewer combined with `strict_deduplication`, because
    /// the kept chain makes a later repeat count as one.
    pub const STRICT_INCREASE: Self = Self(0x08);

    /// `'strict_once'`: an event fills at most one step of a chain.
    ///
    /// Same-timestamp events are ordered by their condition bitmask, so the
    /// result does not depend on the order rows arrive in. `ClickHouse` orders
    /// them by arrival, and combined with `strict_deduplication` its result
    /// can change with row order.
    pub const STRICT_ONCE: Self = Self(0x10);

    /// `'allow_reentry'` (requires `strict_order`, as in `ClickHouse`): a step
    /// arriving before its predecessor is skipped instead of stopping the scan.
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
    /// `'strict'` and `'strict_deduplication'` both map to [`STRICT`](Self::STRICT),
    /// matching `ClickHouse` semantics where they are aliases.
    ///
    /// `'timestamp_dedup'` maps to [`STRICT_DEDUPLICATION`](Self::STRICT_DEDUPLICATION),
    /// an extension mode not present in `ClickHouse`.
    ///
    /// Returns `None` for unrecognized mode strings.
    #[must_use]
    pub fn parse_mode_str(s: &str) -> Option<Self> {
        // Case-insensitive, like `sequence_next_node`'s direction and base.
        const NAMES: [(&str, FunnelMode); 7] = [
            ("strict", FunnelMode::STRICT),
            ("strict_deduplication", FunnelMode::STRICT),
            ("strict_order", FunnelMode::STRICT_ORDER),
            ("timestamp_dedup", FunnelMode::STRICT_DEDUPLICATION),
            ("strict_increase", FunnelMode::STRICT_INCREASE),
            ("strict_once", FunnelMode::STRICT_ONCE),
            ("allow_reentry", FunnelMode::ALLOW_REENTRY),
        ];
        NAMES
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(s))
            .map(|&(_, flag)| flag)
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
            (Self::STRICT_DEDUPLICATION, "timestamp_dedup"),
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

/// Maximum number of funnel steps (one per `u32` condition bit).
const MAX_STEPS: usize = crate::common::event::MAX_EVENT_CONDITIONS;

/// Where a chain records the timestamp each step was reached. `finalize`
/// needs none ([`NoPath`]); `finalize_events` needs all of them
/// ([`StepPath`]). Keeping the 256-byte path out of `finalize`'s chains keeps
/// its per-step copies small.
trait PathStore: Copy + std::fmt::Debug {
    fn start(ts: i64) -> Self;
    /// Records that steps `from..=to` were reached at `ts`.
    fn set(&mut self, from: usize, to: usize, ts: i64);
    fn steps(&self, reached: usize) -> Vec<i64>;
}

/// Records nothing.
#[derive(Debug, Clone, Copy)]
struct NoPath;

impl PathStore for NoPath {
    fn start(_ts: i64) -> Self {
        Self
    }
    fn set(&mut self, _from: usize, _to: usize, _ts: i64) {}
    fn steps(&self, _reached: usize) -> Vec<i64> {
        Vec::new()
    }
}

/// The timestamp at which each step was reached.
#[derive(Debug, Clone, Copy)]
struct StepPath([i64; MAX_STEPS]);

impl PathStore for StepPath {
    fn start(ts: i64) -> Self {
        let mut path = [0; MAX_STEPS];
        path[0] = ts;
        Self(path)
    }
    fn set(&mut self, from: usize, to: usize, ts: i64) {
        for step in &mut self.0[from..=to] {
            *step = ts;
        }
    }
    fn steps(&self, reached: usize) -> Vec<i64> {
        self.0[..reached].to_vec()
    }
}

/// One partial funnel chain: the timestamp of its entry event, and (with
/// [`StepPath`]) the timestamp at which each step `0..=level` was reached.
#[derive(Debug, Clone, Copy)]
struct Chain<P> {
    first: i64,
    path: P,
}

impl<P: PathStore> Chain<P> {
    /// A chain entered (step 0 reached) at `ts`.
    fn start(ts: i64) -> Self {
        Self {
            first: ts,
            path: P::start(ts),
        }
    }

    /// This chain extended through steps `from..=to`, all reached at `ts`.
    fn extended(mut self, from: usize, to: usize, ts: i64) -> Self {
        self.path.set(from, to, ts);
        self
    }
}

/// The chains known to have reached one funnel level.
///
/// Only the chain with the latest entry matters for future steps (a later
/// entry keeps every later event inside the window that an earlier entry
/// would), so one chain is kept per "age": `prev` among chains whose last
/// step is before the timestamp being processed, `cur` among chains that
/// reached this level at that timestamp. `strict_increase` may extend only
/// `prev`. Once a level is non-empty it stays non-empty, as in `ClickHouse`.
#[derive(Debug, Clone, Copy)]
struct Level<P> {
    prev: Option<Chain<P>>,
    cur: Option<Chain<P>>,
}

impl<P: PathStore> Level<P> {
    const EMPTY: Self = Self {
        prev: None,
        cur: None,
    };

    const fn is_set(&self) -> bool {
        self.prev.is_some() || self.cur.is_some()
    }

    /// The chain with the latest entry (`cur` on a tie: it was written last).
    fn best(&self) -> Option<Chain<P>> {
        match (self.prev, self.cur) {
            (Some(p), Some(c)) => Some(if c.first >= p.first { c } else { p }),
            (p, c) => c.or(p),
        }
    }

    fn offer(&mut self, chain: &Chain<P>) {
        if self.cur.is_none_or(|c| chain.first >= c.first) {
            self.cur = Some(*chain);
        }
    }

    /// Moves `cur` into `prev` when the timestamp advances.
    fn roll(&mut self) {
        if let Some(c) = self.cur.take() {
            if self.prev.is_none_or(|p| c.first >= p.first) {
                self.prev = Some(c);
            }
        }
    }
}

/// Signals that the scan must stop: the result is the highest level reached.
struct Stop;

/// Mutable scan state shared by both per-timestamp-group evaluators.
struct Scan<P> {
    /// One entry per funnel step (`steps` long).
    levels: Vec<Level<P>>,
    /// Bit `i` set when `levels[i].cur` may be set (rolled at the next group).
    dirty: u32,
    /// An entry (condition 1) event has been seen.
    first_event: bool,
    window: u64,
    steps: usize,
    mode: FunnelMode,
    /// `strict_increase`, or the `timestamp_dedup` extension mode, which
    /// skips a step at the timestamp of the step before it, i.e. the same.
    strict_increase: bool,
}

impl<P: PathStore> Scan<P> {
    /// `ts` is within the window opened at `first`. Events are sorted, so
    /// `ts >= first` and the gap, even spanning `DuckDB`'s ±infinity
    /// timestamps, fits in `u64`; `wrapping_sub` reinterpreted as `u64` is
    /// exactly that gap.
    const fn in_window(&self, ts: i64, first: i64) -> bool {
        ts.wrapping_sub(first) as u64 <= self.window
    }

    fn offer(&mut self, level: usize, chain: &Chain<P>) {
        self.levels[level].offer(chain);
        self.dirty |= 1 << level;
    }

    fn roll(&mut self) {
        let mut dirty = self.dirty;
        while dirty != 0 {
            let level = dirty.trailing_zeros() as usize;
            self.levels[level].roll();
            dirty &= dirty - 1;
        }
        self.dirty = 0;
    }

    /// Number of levels reached and the latest-entry chain at the highest.
    /// Levels are reached in order, so the set ones form a prefix.
    fn result(&self) -> (usize, Option<Chain<P>>) {
        let reached = self.levels.iter().take_while(|l| l.is_set()).count();
        let chain = reached
            .checked_sub(1)
            .and_then(|top| self.levels[top].best());
        (reached, chain)
    }

    /// The checks `ClickHouse` makes for an entry of condition `idx >= 1`
    /// before trying to extend a chain. `Ok(true)` means "try to extend",
    /// `Ok(false)` means "skip this entry".
    fn pre_extend(&self, idx: usize) -> Result<bool, Stop> {
        // strict_deduplication: a condition repeating a step already reached
        // stops the scan.
        if self.mode.has(FunnelMode::STRICT) && self.levels[idx].is_set() {
            return Err(Stop);
        }
        // strict_order: a step arriving before its predecessor stops the scan
        // (allow_reentry skips it instead).
        if self.mode.has(FunnelMode::STRICT_ORDER)
            && self.first_event
            && !self.levels[idx - 1].is_set()
        {
            return if self.mode.has(FunnelMode::ALLOW_REENTRY) {
                Ok(false)
            } else {
                Err(Stop)
            };
        }
        Ok(self.levels[idx - 1].is_set())
    }

    /// Handles the `strict_order` placeholder entries for rows matching no
    /// condition: they end the scan once a chain has been entered.
    fn no_condition_rows(&self, group: &[Event]) -> Result<(), Stop> {
        if self.mode.has(FunnelMode::STRICT_ORDER)
            && self.first_event
            && group.iter().any(|e| e.conditions == 0)
        {
            return Err(Stop);
        }
        Ok(())
    }

    /// Processes one timestamp group without `strict_once`: an event matching
    /// several conditions may fill several steps (`ClickHouse`'s
    /// `getEventLevelNonStrictOnce`).
    fn group_any(&mut self, group: &[Event], ts: i64) -> Result<(), Stop> {
        self.no_condition_rows(group)?;
        let present = group.iter().fold(0u32, |acc, e| acc | e.conditions);
        for idx in 0..self.steps {
            if present & (1 << idx) == 0 {
                continue;
            }
            if idx == 0 {
                self.offer(0, &Chain::start(ts));
                self.first_event = true;
                continue;
            }
            // Identical entries repeat the same effect, except that a second
            // one sees the level the first one set (strict_deduplication).
            let count = group.iter().filter(|e| e.condition(idx)).take(2).count();
            for _ in 0..count {
                if !self.pre_extend(idx)? {
                    continue;
                }
                let prev = &self.levels[idx - 1];
                // strict_increase extends only chains whose last step is
                // earlier than `ts`. (ClickHouse keeps a single chain per
                // level and can lose the valid earlier one here.)
                let source = if self.strict_increase {
                    prev.prev
                } else {
                    prev.best()
                };
                if let Some(chain) = source.filter(|c| self.in_window(ts, c.first)) {
                    self.offer(idx, &chain.extended(idx, idx, ts));
                    if idx + 1 == self.steps {
                        return Err(Stop);
                    }
                }
            }
        }
        Ok(())
    }

    /// Processes one timestamp group under `strict_once`: each event fills at
    /// most one step of a chain.
    ///
    /// `ClickHouse` enumerates every chain, which is exponential in the
    /// number of same-timestamp events matching several conditions (one
    /// 30-event group exceeded 6 GiB). Within one timestamp, whether a chain
    /// at level `idx - 1` that avoids event `u` exists reduces to whether the
    /// steps it spans can be given distinct events: a bipartite matching over
    /// at most 32 steps. Same-timestamp events are visited in
    /// `(timestamp, conditions)` order, so the answer does not depend on the
    /// order rows arrive in (`ClickHouse`'s does).
    fn group_once(&mut self, group: &[Event], ts: i64) -> Result<(), Stop> {
        self.no_condition_rows(group)?;
        let mut matcher = Matcher::new(group, self.steps);
        for idx in 0..self.steps {
            if idx == 0 {
                if group.iter().any(|e| e.condition(0)) {
                    self.offer(0, &Chain::start(ts));
                    self.first_event = true;
                }
                continue;
            }
            // `extend_once` depends on `u` only through the matchings, and
            // for an event that is no step's kept candidate every matching
            // answer is the same; compute that shared result once per step.
            // (`pre_extend` and `offer` still run per event: their effects
            // depend on what earlier events did.)
            let mut shared: Option<Option<Chain<P>>> = None;
            for (u, event) in group.iter().enumerate() {
                if !event.condition(idx) || !self.pre_extend(idx)? {
                    continue;
                }
                let extended = if matcher.is_candidate(u) {
                    self.extend_once(&mut matcher, idx, u, ts)
                } else {
                    *shared.get_or_insert_with(|| self.extend_once(&mut matcher, idx, u, ts))
                };
                if let Some(chain) = extended {
                    self.offer(idx, &chain);
                    if idx + 1 == self.steps {
                        return Err(Stop);
                    }
                }
            }
        }
        Ok(())
    }

    /// The latest-entry chain that event `u` (matching condition `idx`) can
    /// extend under `strict_once`, if any.
    ///
    /// Candidate bases: a chain entered in this group (entry at `ts`, steps
    /// `0..idx` filled by distinct events of the group other than `u`), or
    /// a chain from an earlier timestamp at level `j` extended through steps
    /// `j + 1..idx` by distinct events of the group other than `u`. Under
    /// `strict_increase` no step may be filled at the same timestamp as the
    /// one before it, so only a chain at level `idx - 1` from an earlier
    /// timestamp qualifies.
    fn extend_once(
        &self,
        matcher: &mut Matcher,
        idx: usize,
        u: usize,
        ts: i64,
    ) -> Option<Chain<P>> {
        if !self.strict_increase && matcher.distinct_events_exist(0, idx, u) {
            return Some(Chain::start(ts).extended(0, idx, ts));
        }
        let lowest = if self.strict_increase { idx - 1 } else { 0 };
        let mut best: Option<(usize, Chain<P>)> = None;
        for j in lowest..idx {
            let Some(base) = self.levels[j].prev else {
                continue;
            };
            if !self.in_window(ts, base.first) || best.is_some_and(|(_, b)| b.first >= base.first) {
                continue;
            }
            if matcher.distinct_events_exist(j + 1, idx, u) {
                best = Some((j, base));
            }
        }
        best.map(|(j, base)| base.extended(j + 1, idx, ts))
    }
}

/// Answers "can steps `lo..hi` each get a distinct event of this
/// same-timestamp group, none of them event `exclude`?" for `strict_once`.
///
/// Each step keeps at most `MAX_STEPS + 1` candidate events: a step needs one
/// event, the other steps use at most `MAX_STEPS - 1` and one more may be
/// excluded, so a step with that many candidates can always be served and
/// extra candidates never change an answer. Answers are cached per step
/// range; the excluded event is part of the key only when it is a candidate,
/// since excluding any other event changes nothing. This bounds the work for
/// a large group by the number of conditions rather than the group size.
struct Matcher {
    /// Number of events in the group.
    group_len: usize,
    candidates: Vec<Vec<usize>>,
    /// Per event, the steps whose (kept) candidates include it.
    candidate_steps: Vec<u64>,
    cache: std::collections::HashMap<u64, bool, BuildPackedKeyHasher>,
}

impl Matcher {
    fn new(group: &[Event], steps: usize) -> Self {
        // A one-event group never needs a matching (see
        // `distinct_events_exist`), so it skips building the candidates.
        if group.len() < 2 {
            return Self {
                group_len: group.len(),
                candidates: Vec::new(),
                candidate_steps: Vec::new(),
                cache: std::collections::HashMap::default(),
            };
        }
        let candidates: Vec<Vec<usize>> = (0..steps)
            .map(|step| {
                (0..group.len())
                    .filter(|&e| group[e].condition(step))
                    .take(MAX_STEPS + 1)
                    .collect()
            })
            .collect();
        let mut candidate_steps = vec![0u64; group.len()];
        for (step, events) in candidates.iter().enumerate() {
            for &e in events {
                candidate_steps[e] |= 1 << step;
            }
        }
        Self {
            group_len: group.len(),
            candidates,
            candidate_steps,
            cache: std::collections::HashMap::default(),
        }
    }

    /// Whether event `e` is a kept candidate of some step. Events that are
    /// not cannot change any answer when excluded. (In a one-event group the
    /// candidates are not built; that event counts as a candidate.)
    fn is_candidate(&self, e: usize) -> bool {
        self.candidate_steps.get(e).is_none_or(|&steps| steps != 0)
    }

    /// `exclude` is an event of the group (the one extending the chain).
    fn distinct_events_exist(&mut self, lo: usize, hi: usize, exclude: usize) -> bool {
        if lo >= hi {
            return true;
        }
        // The other events of the group are all there is to give out.
        if hi - lo > self.group_len - 1 {
            return false;
        }
        if hi - lo == 1 {
            return self.candidates[lo].iter().any(|&e| e != exclude);
        }
        let range = (u64::MAX << lo) & !(u64::MAX << hi);
        let relevant = self.candidate_steps[exclude] & range != 0;
        // lo, hi <= 32 fit in 6 bits each; exclude + 1 (0 = not relevant)
        // fills the rest.
        let key = lo as u64
            | (hi as u64) << 6
            | if relevant {
                (exclude as u64 + 1) << 12
            } else {
                0
            };
        if let Some(&known) = self.cache.get(&key) {
            return known;
        }
        let answer = Self::matching_exists(&self.candidates[lo..hi], exclude);
        self.cache.insert(key, answer);
        answer
    }

    /// Kuhn's augmenting-path bipartite matching of steps to events.
    fn matching_exists(candidates: &[Vec<usize>], exclude: usize) -> bool {
        fn augment(
            step: usize,
            candidates: &[Vec<usize>],
            exclude: usize,
            owner: &mut Vec<(usize, usize)>,
            seen: &mut Vec<usize>,
        ) -> bool {
            for &event in &candidates[step] {
                if event == exclude || seen.contains(&event) {
                    continue;
                }
                seen.push(event);
                match owner.iter().position(|&(e, _)| e == event) {
                    None => {
                        owner.push((event, step));
                        return true;
                    }
                    Some(pos) => {
                        let other = owner[pos].1;
                        if augment(other, candidates, exclude, owner, seen) {
                            // `other` now owns a different event; give this one to `step`.
                            owner[pos] = (event, step);
                            return true;
                        }
                    }
                }
            }
            false
        }

        let mut owner: Vec<(usize, usize)> = Vec::with_capacity(candidates.len());
        let mut seen = Vec::new();
        (0..candidates.len()).all(|step| {
            seen.clear();
            augment(step, candidates, exclude, &mut owner, &mut seen)
        })
    }
}

/// A hasher for [`Matcher`]'s packed `u64` keys: one multiply (Fibonacci
/// hashing). The keys are small integers, so `SipHash`'s protection against
/// adversarial keys buys nothing here, and it was most of `strict_once`'s
/// time.
#[derive(Default)]
struct PackedKeyHasher(u64);

impl std::hash::Hasher for PackedKeyHasher {
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0.rotate_left(8) ^ u64::from(b)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        }
    }

    fn write_u64(&mut self, x: u64) {
        self.0 = x.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

type BuildPackedKeyHasher = std::hash::BuildHasherDefault<PackedKeyHasher>;

/// State for the `window_funnel` aggregate function.
///
/// Collects timestamped events during `update`, then evaluates the funnel in
/// `finalize`.
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
    /// Whether `window_size_us` came from a row (a zero window is valid, so
    /// zero cannot mean "unset").
    pub window_set: bool,
    /// Whether `mode` came from a row's non-`NULL` mode argument.
    pub mode_set: bool,
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
            window_set: false,
            mode_set: false,
        }
    }

    /// Adds an event to the state.
    ///
    /// Events where no condition is true cannot fill a funnel step and are
    /// dropped to save memory, except under `strict_order`, where such an
    /// event interrupts the chain (as in `ClickHouse`). The mode must be set
    /// before the event is added.
    ///
    /// `num_conditions` is the total number of funnel steps. This is passed
    /// explicitly because the `Event` bitmask does not carry length information.
    pub fn update(&mut self, event: Event, num_conditions: usize) {
        self.num_conditions = num_conditions;
        if event.has_any_condition() || self.mode.has(FunnelMode::STRICT_ORDER) {
            self.events.push(event);
        }
    }

    /// Combines two states by concatenating their event lists, returning a new state.
    ///
    /// Events do not need to be in sorted order during combine because
    /// `finalize()` sorts them before scanning.
    #[must_use]
    pub fn combine(&self, other: &Self) -> Self {
        let mut combined = self.clone();
        combined.combine_in_place(other);
        combined
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
        // and combines source states into them, so these fields must be
        // propagated. (The FFI layer rejects groups whose rows disagree.)
        if !self.window_set && (other.window_set || self.window_size_us == 0) {
            self.window_size_us = other.window_size_us;
            self.window_set = other.window_set;
        }
        if !self.mode_set && (other.mode_set || self.mode.is_default()) {
            self.mode = other.mode;
            self.mode_set = other.mode_set;
        }
    }

    /// Computes the number of funnel steps reached (0..=`num_conditions`).
    #[must_use]
    pub fn finalize(&mut self) -> i64 {
        self.evaluate::<NoPath>().0 as i64
    }

    /// Computes the step timestamps of the chain that reached the most steps.
    ///
    /// Returns one timestamp per step reached, so the length always equals
    /// [`finalize`](Self::finalize)'s result. Among chains reaching that many
    /// steps, the one with the latest entry is returned (the chain the scan
    /// keeps; on a tie, the one completed last). An event that fills several
    /// steps contributes its timestamp once per step. Empty when no entry
    /// condition matches.
    #[must_use]
    pub fn finalize_events(&mut self) -> Vec<i64> {
        match self.evaluate::<StepPath>() {
            (reached, Some(chain)) => chain.path.steps(reached),
            (_, None) => Vec::new(),
        }
    }

    /// Runs `ClickHouse`'s `windowFunnel` scan over the sorted events.
    ///
    /// Each event contributes one entry per true condition (plus, under
    /// `strict_order`, one placeholder when none is true), visited in
    /// `(timestamp, condition)` order. For each level the latest-entry chain
    /// is tracked; an entry for condition `k` extends the chain at level
    /// `k - 1` when it is within the window of that chain's entry. Mode
    /// checks may stop the scan early. The result is the number of levels
    /// reached.
    ///
    /// Time: O(n log n) for the sort, then O(n * k) for the scan, plus a
    /// bounded matching per same-timestamp entry under `strict_once`.
    fn evaluate<P: PathStore>(&mut self) -> (usize, Option<Chain<P>>) {
        let steps = self.num_conditions.min(MAX_STEPS);
        if self.events.is_empty() || steps == 0 {
            return (0, None);
        }
        sort_events(&mut self.events);

        let mode = self.mode;
        let mut scan = Scan {
            levels: vec![Level::EMPTY; steps],
            dirty: 0,
            first_event: false,
            window: self.window_size_us as u64,
            steps,
            mode,
            strict_increase: mode.has(FunnelMode::STRICT_INCREASE)
                || mode.has(FunnelMode::STRICT_DEDUPLICATION),
        };

        for group in self
            .events
            .chunk_by(|a, b| a.timestamp_us == b.timestamp_us)
        {
            let ts = group[0].timestamp_us;
            scan.roll();
            let outcome = if mode.has(FunnelMode::STRICT_ONCE) {
                scan.group_once(group, ts)
            } else {
                scan.group_any(group, ts)
            };
            if outcome.is_err() {
                break;
            }
        }
        scan.result()
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
        // One event satisfying every condition fills every step (one entry
        // per true condition, as in ClickHouse; verified against
        // windowFunnel(3600) on ClickHouse 26.9.8.3, which returns 3).
        state.update(make_event(0, &[true, true, true]), 3);
        assert_eq!(state.finalize(), 3);
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
    fn test_strict_order_unrelated_event_breaks_chain() {
        // Under strict_order an event matching no condition interrupts the
        // chain ("doesn't allow interventions of other events"). ClickHouse
        // 26.9.8.3 windowFunnel(3600, 'strict_order') returns 1 here.
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.mode = FunnelMode::STRICT_ORDER;
        state.update(make_event(0, &[true, false, false]), 3);
        state.update(make_event(1_000, &[false, false, false]), 3);
        state.update(make_event(2_000, &[false, true, false]), 3);
        assert_eq!(state.finalize(), 1);
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
        // strict_deduplication is a ClickHouse alias for strict
        assert_eq!(
            FunnelMode::parse_mode_str("strict_deduplication"),
            Some(FunnelMode::STRICT)
        );
        // timestamp_dedup is our extension mode
        assert_eq!(
            FunnelMode::parse_mode_str("timestamp_dedup"),
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
    fn test_parse_modes_all_clickhouse_modes() {
        // ClickHouse-compatible modes: strict_deduplication is an alias for strict
        let mode = FunnelMode::parse_modes(
            "strict, strict_order, strict_deduplication, strict_increase, strict_once, allow_reentry",
        )
        .unwrap();
        assert!(mode.has(FunnelMode::STRICT)); // both 'strict' and 'strict_deduplication' set this
        assert!(mode.has(FunnelMode::STRICT_ORDER));
        assert!(!mode.has(FunnelMode::STRICT_DEDUPLICATION)); // not set by ClickHouse mode names
        assert!(mode.has(FunnelMode::STRICT_INCREASE));
        assert!(mode.has(FunnelMode::STRICT_ONCE));
        assert!(mode.has(FunnelMode::ALLOW_REENTRY));
    }

    #[test]
    fn test_parse_modes_all_modes_including_extensions() {
        // All modes including our extension mode
        let mode = FunnelMode::parse_modes(
            "strict, strict_order, timestamp_dedup, strict_increase, strict_once, allow_reentry",
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

    // ── Coverage gap tests: mode combination edge cases ──

    #[test]
    fn test_strict_dedup_plus_allow_reentry() {
        // STRICT_DEDUPLICATION + ALLOW_REENTRY: dedup skips same-timestamp
        // events after the previous matched step, and reentry resets the
        // chain when entry condition fires again.
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.mode = FunnelMode::STRICT_DEDUPLICATION.with(FunnelMode::ALLOW_REENTRY);
        // Entry at t=100
        state.update(make_event(100, &[true, false, false]), 3);
        // Step 2 at t=100 (same ts as entry) → STRICT_DEDUP should skip
        state.update(make_event(100, &[false, true, false]), 3);
        // Step 2 at t=200 (different ts) → should advance
        state.update(make_event(200, &[false, true, false]), 3);
        // Step 3 at t=300 → should complete
        state.update(make_event(300, &[false, false, true]), 3);
        assert_eq!(state.finalize(), 3);
    }

    #[test]
    fn test_strict_dedup_plus_allow_reentry_reset_mid_chain() {
        // Reentry at same timestamp as previous match should reset
        // but dedup should then skip same-timestamp advancement.
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.mode = FunnelMode::STRICT_DEDUPLICATION.with(FunnelMode::ALLOW_REENTRY);
        // Entry at t=100, advance to step 2 at t=200
        state.update(make_event(100, &[true, false, false]), 3);
        state.update(make_event(200, &[false, true, false]), 3);
        // Reentry at t=300 → resets chain
        state.update(make_event(300, &[true, false, false]), 3);
        // Step 2 at t=300 (same ts as reentry) → dedup skips
        state.update(make_event(300, &[false, true, false]), 3);
        // Step 2 at t=400 (different ts) → should advance
        state.update(make_event(400, &[false, true, false]), 3);
        // Step 3 at t=500
        state.update(make_event(500, &[false, false, true]), 3);
        assert_eq!(state.finalize(), 3);
    }

    #[test]
    fn test_strict_dedup_plus_strict_order() {
        // STRICT_DEDUPLICATION + STRICT_ORDER: dedup skips same-ts events,
        // and strict_order breaks if earlier conditions appear between steps.
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.mode = FunnelMode::STRICT_DEDUPLICATION.with(FunnelMode::STRICT_ORDER);
        state.update(make_event(100, &[true, false, false]), 3);
        // Step 2 at same ts as entry → dedup skips
        state.update(make_event(100, &[false, true, false]), 3);
        // Step 2 at different ts → should advance
        state.update(make_event(200, &[false, true, false]), 3);
        // Step 3 at different ts
        state.update(make_event(300, &[false, false, true]), 3);
        assert_eq!(state.finalize(), 3);
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

        // ── Coverage gap: STRICT_DEDUPLICATION + ALLOW_REENTRY combined mode ──
        // Validates the interaction when both modes are active:
        //   ALLOW_REENTRY resets the chain when entry condition fires again
        //   STRICT_DEDUPLICATION skips events with the same timestamp as prev match
        // The modes are evaluated in order: ALLOW_REENTRY first, then STRICT_DEDUP.

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

#[cfg(test)]
mod finalize_events_tests {
    use super::*;

    fn make_event(ts: i64, conds: &[bool]) -> Event {
        Event::from_bools(ts, conds)
    }

    fn state_with(
        window_us: i64,
        mode: FunnelMode,
        events: &[(i64, &[bool])],
    ) -> WindowFunnelState {
        let mut state = WindowFunnelState::new();
        state.window_size_us = window_us;
        state.mode = mode;
        for &(ts, conds) in events {
            state.update(make_event(ts, conds), conds.len());
        }
        state
    }

    #[test]
    fn test_events_empty_state() {
        let mut state = WindowFunnelState::new();
        assert!(state.finalize_events().is_empty());
    }

    #[test]
    fn test_events_complete_funnel() {
        let mut state = state_with(
            3_600_000_000,
            FunnelMode::DEFAULT,
            &[
                (0, &[true, false, false]),
                (1_000_000, &[false, true, false]),
                (2_000_000, &[false, false, true]),
            ],
        );
        assert_eq!(state.finalize_events(), vec![0, 1_000_000, 2_000_000]);
    }

    #[test]
    fn test_events_partial_funnel() {
        let mut state = state_with(
            3_600_000_000,
            FunnelMode::DEFAULT,
            &[
                (0, &[true, false, false]),
                (1_000_000, &[false, true, false]),
            ],
        );
        assert_eq!(state.finalize_events(), vec![0, 1_000_000]);
    }

    #[test]
    fn test_events_no_entry_returns_empty() {
        let mut state = state_with(
            3_600_000_000,
            FunnelMode::DEFAULT,
            &[
                (0, &[false, true, false]),
                (1_000_000, &[false, false, true]),
            ],
        );
        assert!(state.finalize_events().is_empty());
    }

    #[test]
    fn test_events_multi_step_event_repeats_timestamp() {
        // One event satisfying conditions 2 and 3 advances two steps and
        // contributes its timestamp twice.
        let mut state = state_with(
            3_600_000_000,
            FunnelMode::DEFAULT,
            &[
                (0, &[true, false, false]),
                (1_000_000, &[false, true, true]),
            ],
        );
        assert_eq!(state.finalize_events(), vec![0, 1_000_000, 1_000_000]);
    }

    #[test]
    fn test_events_best_chain_wins_over_earlier_shorter() {
        // First entry only reaches step 1 (next event out of window);
        // second entry completes the funnel.
        let mut state = state_with(
            5_000_000,
            FunnelMode::DEFAULT,
            &[
                (0, &[true, false]),
                (10_000_000, &[true, false]),
                (11_000_000, &[false, true]),
            ],
        );
        assert_eq!(state.finalize_events(), vec![10_000_000, 11_000_000]);
    }

    #[test]
    fn test_events_earliest_chain_wins_ties() {
        // Both entries reach the full 2 steps; the earliest entry's chain is
        // returned (mirrors finalize's greedy order).
        let mut state = state_with(
            5_000_000,
            FunnelMode::DEFAULT,
            &[
                (0, &[true, false]),
                (1_000_000, &[false, true]),
                (2_000_000, &[true, false]),
                (3_000_000, &[false, true]),
            ],
        );
        assert_eq!(state.finalize_events(), vec![0, 1_000_000]);
    }

    #[test]
    fn test_events_window_boundary_excluded() {
        let mut state = state_with(
            1_000_000,
            FunnelMode::DEFAULT,
            &[(0, &[true, false]), (2_000_000, &[false, true])],
        );
        // Second event is outside the window: only the entry is matched.
        assert_eq!(state.finalize_events(), vec![0]);
    }

    #[test]
    fn test_events_allow_reentry_records_reset_chain() {
        // Re-entry at t=2s resets the chain; the winning chain starts there.
        let mut state = state_with(
            10_000_000,
            FunnelMode::ALLOW_REENTRY,
            &[
                (0, &[true, false, false]),
                (1_000_000, &[false, true, false]),
                (2_000_000, &[true, false, false]),
                (3_000_000, &[false, true, false]),
                (4_000_000, &[false, false, true]),
            ],
        );
        assert_eq!(
            state.finalize_events(),
            vec![2_000_000, 3_000_000, 4_000_000]
        );
    }

    #[test]
    fn test_events_strict_once_one_step_per_event() {
        let mut state = state_with(
            3_600_000_000,
            FunnelMode::STRICT_ONCE,
            &[
                (0, &[true, false, false]),
                (1_000_000, &[false, true, true]),
            ],
        );
        // strict_once: the dual-condition event advances only one step.
        assert_eq!(state.finalize_events(), vec![0, 1_000_000]);
    }

    /// A finalize-parity scenario: window, mode, and event list.
    type Scenario = (i64, FunnelMode, &'static [(i64, &'static [bool])]);

    #[test]
    fn test_events_length_always_matches_finalize() {
        // finalize_events().len() must equal finalize() across modes/shapes.
        let scenarios: &[Scenario] = &[
            (
                5_000_000,
                FunnelMode::DEFAULT,
                &[
                    (0, &[true, false, true]),
                    (1_000_000, &[false, true, false]),
                    (2_000_000, &[true, false, false]),
                    (7_000_000, &[false, false, true]),
                ],
            ),
            (
                10_000_000,
                FunnelMode::STRICT,
                &[
                    (0, &[true, false, false]),
                    (1_000_000, &[false, true, false]),
                    (2_000_000, &[false, true, true]),
                ],
            ),
            (
                10_000_000,
                FunnelMode::STRICT_ORDER,
                &[
                    (0, &[true, false, false]),
                    (1_000_000, &[true, false, false]),
                    (2_000_000, &[false, true, false]),
                ],
            ),
            (
                10_000_000,
                FunnelMode::STRICT_INCREASE,
                &[
                    (0, &[true, false, false]),
                    (0, &[false, true, false]),
                    (1_000_000, &[false, true, false]),
                    (2_000_000, &[false, false, true]),
                ],
            ),
        ];
        for (idx, &(window, mode, events)) in scenarios.iter().enumerate() {
            let mut a = state_with(window, mode, events);
            let mut b = state_with(window, mode, events);
            let steps = a.finalize();
            let timestamps = b.finalize_events();
            assert_eq!(
                timestamps.len() as i64,
                steps,
                "scenario {idx}: length must equal finalize() step count"
            );
        }
    }
}

#[cfg(test)]
mod infinity_tests {
    use super::*;
    use crate::common::event::Event;

    /// An event at `'infinity'` (`i64::MAX`) is outside any finite window of an
    /// entry at '-infinity'; a wrapped difference would put it inside.
    #[test]
    fn test_scan_window_saturates_at_infinity() {
        let mut state = WindowFunnelState::new();
        state.window_size_us = 3_600_000_000;
        state.update(Event::new(-i64::MAX, 0b01), 2);
        state.update(Event::new(i64::MAX, 0b10), 2);
        assert_eq!(
            state.finalize(),
            1,
            "infinitely distant step must not match"
        );
        assert_eq!(state.finalize_events(), vec![-i64::MAX]);
    }
}

// Expected values checked against ClickHouse 26.9.8.3 (`clickhouse local`,
// `windowFunnel`, whole-second `DateTime` timestamps). Where ClickHouse has
// a defect (a lost chain under `strict_increase`, row-order dependence under
// `strict_deduplication` + `strict_once`), the expected value comes from an
// exhaustive reference that follows ClickHouse's algorithm but keeps every
// chain and orders tied rows canonically; ClickHouse's answer is noted.
#[cfg(test)]
mod clickhouse_parity_tests {
    use super::*;

    const S: i64 = 1_000_000;

    #[test]
    fn ch_entry_row_fills_later_steps() {
        // ClickHouse 26.9.8.3 windowFunnel returns 3.
        let mut s = WindowFunnelState::new();
        s.window_size_us = 10 * S;
        s.mode = FunnelMode::DEFAULT;
        s.update(Event::new(0, 0b11), 3);
        s.update(Event::new(S, 0b100), 3);
        assert_eq!(s.finalize(), 3);
    }

    #[test]
    fn ch_strict_increase_checked_per_step() {
        // ClickHouse 26.9.8.3 windowFunnel returns 2.
        let mut s = WindowFunnelState::new();
        s.window_size_us = 10 * S;
        s.mode = FunnelMode::DEFAULT.with(FunnelMode::STRICT_INCREASE);
        s.update(Event::new(0, 0b1), 3);
        s.update(Event::new(S, 0b110), 3);
        assert_eq!(s.finalize(), 2);
    }

    #[test]
    fn ch_strict_order_later_step_early_stops() {
        // ClickHouse 26.9.8.3 windowFunnel returns 1.
        let mut s = WindowFunnelState::new();
        s.window_size_us = 10 * S;
        s.mode = FunnelMode::DEFAULT.with(FunnelMode::STRICT_ORDER);
        s.update(Event::new(0, 0b1), 3);
        s.update(Event::new(S, 0b100), 3);
        s.update(Event::new(2 * S, 0b10), 3);
        s.update(Event::new(3 * S, 0b100), 3);
        assert_eq!(s.finalize(), 1);
    }

    #[test]
    fn ch_strict_order_entry_repeat_restarts() {
        // ClickHouse 26.9.8.3 windowFunnel returns 3.
        let mut s = WindowFunnelState::new();
        s.window_size_us = 10 * S;
        s.mode = FunnelMode::DEFAULT.with(FunnelMode::STRICT_ORDER);
        s.update(Event::new(0, 0b1), 3);
        s.update(Event::new(S, 0b10), 3);
        s.update(Event::new(2 * S, 0b1), 3);
        s.update(Event::new(3 * S, 0b100), 3);
        assert_eq!(s.finalize(), 3);
    }

    #[test]
    fn ch_strict_order_step_repeat_overwrites() {
        // ClickHouse 26.9.8.3 windowFunnel returns 3.
        let mut s = WindowFunnelState::new();
        s.window_size_us = 10 * S;
        s.mode = FunnelMode::DEFAULT.with(FunnelMode::STRICT_ORDER);
        s.update(Event::new(0, 0b1), 3);
        s.update(Event::new(S, 0b10), 3);
        s.update(Event::new(2 * S, 0b10), 3);
        s.update(Event::new(3 * S, 0b100), 3);
        assert_eq!(s.finalize(), 3);
    }

    #[test]
    fn ch_allow_reentry_skips_early_steps() {
        // ClickHouse 26.9.8.3 windowFunnel returns 3.
        let mut s = WindowFunnelState::new();
        s.window_size_us = 10 * S;
        s.mode = FunnelMode::DEFAULT
            .with(FunnelMode::STRICT_ORDER)
            .with(FunnelMode::ALLOW_REENTRY);
        s.update(Event::new(0, 0b1), 3);
        s.update(Event::new(S, 0b10), 3);
        s.update(Event::new(2 * S, 0b1), 3);
        s.update(Event::new(3 * S, 0b100), 3);
        assert_eq!(s.finalize(), 3);
    }

    #[test]
    fn ch_allow_reentry_keeps_reached_level() {
        // ClickHouse 26.9.8.3 windowFunnel returns 2.
        let mut s = WindowFunnelState::new();
        s.window_size_us = 10 * S;
        s.mode = FunnelMode::DEFAULT
            .with(FunnelMode::STRICT_ORDER)
            .with(FunnelMode::ALLOW_REENTRY);
        s.update(Event::new(0, 0b1), 3);
        s.update(Event::new(S, 0b10), 3);
        s.update(Event::new(2 * S, 0b1), 3);
        assert_eq!(s.finalize(), 2);
    }

    #[test]
    fn ch_dedup_stops_on_repeat_even_if_advancing() {
        // ClickHouse 26.9.8.3 windowFunnel returns 2.
        let mut s = WindowFunnelState::new();
        s.window_size_us = 10 * S;
        s.mode = FunnelMode::DEFAULT.with(FunnelMode::STRICT);
        s.update(Event::new(0, 0b1), 3);
        s.update(Event::new(S, 0b10), 3);
        s.update(Event::new(2 * S, 0b110), 3);
        assert_eq!(s.finalize(), 2);
    }

    #[test]
    fn ch_dedup_ignores_entry_repeats() {
        // ClickHouse 26.9.8.3 windowFunnel returns 2.
        let mut s = WindowFunnelState::new();
        s.window_size_us = S;
        s.mode = FunnelMode::DEFAULT
            .with(FunnelMode::STRICT)
            .with(FunnelMode::STRICT_INCREASE)
            .with(FunnelMode::STRICT_ONCE);
        s.update(Event::new(0, 0b1), 2);
        s.update(Event::new(S, 0b1), 2);
        s.update(Event::new(S, 0b10), 2);
        assert_eq!(s.finalize(), 2);
    }

    #[test]
    fn ch_dedup_sees_expired_levels() {
        // ClickHouse 26.9.8.3 windowFunnel returns 2.
        let mut s = WindowFunnelState::new();
        s.window_size_us = 2 * S;
        s.mode = FunnelMode::DEFAULT.with(FunnelMode::STRICT);
        s.update(Event::new(0, 0b1), 3);
        s.update(Event::new(S, 0b10), 3);
        s.update(Event::new(5 * S, 0b1), 3);
        s.update(Event::new(6 * S, 0b10), 3);
        s.update(Event::new(7 * S, 0b100), 3);
        assert_eq!(s.finalize(), 2);
    }

    #[test]
    fn ch_ties_ordered_by_condition() {
        // ClickHouse 26.9.8.3 windowFunnel returns 4.
        let mut s = WindowFunnelState::new();
        s.window_size_us = 10 * S;
        s.mode = FunnelMode::DEFAULT;
        s.update(Event::new(0, 0b1), 4);
        s.update(Event::new(0, 0b100), 4);
        s.update(Event::new(0, 0b1010), 4);
        assert_eq!(s.finalize(), 4);
    }

    #[test]
    fn ch_strict_once_tie_row() {
        // ClickHouse 26.9.8.3 windowFunnel returns 2.
        let mut s = WindowFunnelState::new();
        s.window_size_us = 10 * S;
        s.mode = FunnelMode::DEFAULT.with(FunnelMode::STRICT_ONCE);
        s.update(Event::new(0, 0b10), 2);
        s.update(Event::new(0, 0b11), 2);
        assert_eq!(s.finalize(), 2);
    }

    #[test]
    fn ch_strict_once_one_step_per_row() {
        // ClickHouse 26.9.8.3 windowFunnel returns 1.
        let mut s = WindowFunnelState::new();
        s.window_size_us = 10 * S;
        s.mode = FunnelMode::DEFAULT.with(FunnelMode::STRICT_ONCE);
        s.update(Event::new(0, 0b11), 2);
        assert_eq!(s.finalize(), 1);
    }

    #[test]
    fn ch_single_condition() {
        // ClickHouse 26.9.8.3 windowFunnel returns 1.
        let mut s = WindowFunnelState::new();
        s.window_size_us = 10 * S;
        s.mode = FunnelMode::DEFAULT;
        s.update(Event::new(3 * S, 0b1), 1);
        assert_eq!(s.finalize(), 1);
    }

    #[test]
    fn ch_window_boundary_inclusive() {
        // ClickHouse 26.9.8.3 windowFunnel returns 2.
        let mut s = WindowFunnelState::new();
        s.window_size_us = 5 * S;
        s.mode = FunnelMode::DEFAULT;
        s.update(Event::new(0, 0b1), 2);
        s.update(Event::new(5 * S, 0b10), 2);
        assert_eq!(s.finalize(), 2);
    }

    #[test]
    fn ch_window_boundary_exclusive() {
        // ClickHouse 26.9.8.3 windowFunnel returns 1.
        let mut s = WindowFunnelState::new();
        s.window_size_us = 5 * S;
        s.mode = FunnelMode::DEFAULT;
        s.update(Event::new(0, 0b1), 2);
        s.update(Event::new(6 * S, 0b10), 2);
        assert_eq!(s.finalize(), 1);
    }

    #[test]
    fn fixed_strict_increase_keeps_earlier_chain() {
        // ClickHouse 26.9.8.3 returns 1 (it loses a valid chain or depends on row order); expected value from the exhaustive reference.
        let mut s = WindowFunnelState::new();
        s.window_size_us = 10 * S;
        s.mode = FunnelMode::DEFAULT.with(FunnelMode::STRICT_INCREASE);
        s.update(Event::new(0, 0b1), 2);
        s.update(Event::new(S, 0b1), 2);
        s.update(Event::new(S, 0b10), 2);
        assert_eq!(s.finalize(), 2);
    }

    #[test]
    fn fixed_strict_increase_keeps_earlier_step() {
        // ClickHouse 26.9.8.3 returns 2 (it loses a valid chain or depends on row order); expected value from the exhaustive reference.
        let mut s = WindowFunnelState::new();
        s.window_size_us = 10 * S;
        s.mode = FunnelMode::DEFAULT.with(FunnelMode::STRICT_INCREASE);
        s.update(Event::new(0, 0b1), 3);
        s.update(Event::new(S, 0b10), 3);
        s.update(Event::new(2 * S, 0b10), 3);
        s.update(Event::new(2 * S, 0b100), 3);
        assert_eq!(s.finalize(), 3);
    }

    #[test]
    fn fixed_dedup_strict_increase_sees_kept_chain() {
        // ClickHouse 26.9.8.3 returns 3 (it loses a valid chain or depends on row order); expected value from the exhaustive reference.
        let mut s = WindowFunnelState::new();
        s.window_size_us = 7 * S;
        s.mode = FunnelMode::DEFAULT
            .with(FunnelMode::STRICT)
            .with(FunnelMode::STRICT_INCREASE);
        s.update(Event::new(2 * S, 0b1), 3);
        s.update(Event::new(5 * S, 0b11), 3);
        s.update(Event::new(7 * S, 0b10), 3);
        s.update(Event::new(8 * S, 0b100), 3);
        assert_eq!(s.finalize(), 2);
    }

    #[test]
    fn fixed_dedup_strict_once_tie_order() {
        // ClickHouse 26.9.8.3 returns 2 (it loses a valid chain or depends on row order); expected value from the exhaustive reference.
        let mut s = WindowFunnelState::new();
        s.window_size_us = S;
        s.mode = FunnelMode::DEFAULT
            .with(FunnelMode::STRICT)
            .with(FunnelMode::STRICT_ONCE);
        s.update(Event::new(2 * S, 0b10), 3);
        s.update(Event::new(2 * S, 0b111), 3);
        s.update(Event::new(3 * S, 0b100), 3);
        assert_eq!(s.finalize(), 2);
    }

    #[test]
    fn dedup_strict_once_result_is_independent_of_row_order() {
        // ClickHouse returns 2 or 3 for these rows depending on insertion order.
        let rows = [(2, 0b010), (2, 0b111), (3, 0b100)];
        let mode = FunnelMode::DEFAULT
            .with(FunnelMode::STRICT)
            .with(FunnelMode::STRICT_ONCE);
        let orders: [[usize; 3]; 6] = [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ];
        let results: Vec<i64> = orders
            .iter()
            .map(|order| {
                let mut s = WindowFunnelState::new();
                s.window_size_us = S;
                s.mode = mode;
                for &i in order {
                    s.update(Event::new(rows[i].0 * S, rows[i].1), 3);
                }
                s.finalize()
            })
            .collect();
        assert!(results.iter().all(|&r| r == results[0]), "{results:?}");
    }

    #[test]
    fn strict_once_large_same_timestamp_group_stays_polynomial() {
        // 200 rows at one timestamp, each matching all 32 conditions. Chain
        // enumeration (ClickHouse's approach) exceeded 6 GiB on a 30-row
        // group; the matching formulation answers directly: 32 distinct rows
        // fill all 32 steps.
        let mut s = WindowFunnelState::new();
        s.window_size_us = S;
        s.mode = FunnelMode::STRICT_ONCE;
        for _ in 0..200 {
            s.update(Event::new(0, u32::MAX), 32);
        }
        assert_eq!(s.finalize(), 32);

        // 31 rows cannot fill 32 steps one row each.
        let mut s = WindowFunnelState::new();
        s.window_size_us = S;
        s.mode = FunnelMode::STRICT_ONCE;
        for _ in 0..31 {
            s.update(Event::new(0, u32::MAX), 32);
        }
        assert_eq!(s.finalize(), 31);
    }

    #[test]
    fn events_follow_the_reported_chain() {
        // c1@0, c1@1, c2@1 under strict_increase: the chain is c1@0 -> c2@1.
        let mut s = WindowFunnelState::new();
        s.window_size_us = 10 * S;
        s.mode = FunnelMode::STRICT_INCREASE;
        s.update(Event::new(0, 0b01), 2);
        s.update(Event::new(S, 0b01), 2);
        s.update(Event::new(S, 0b10), 2);
        assert_eq!(s.finalize_events(), vec![0, S]);
        // One row filling two steps contributes its timestamp twice.
        let mut s = WindowFunnelState::new();
        s.window_size_us = 10 * S;
        s.update(Event::new(5 * S, 0b11), 2);
        assert_eq!(s.finalize_events(), vec![5 * S, 5 * S]);
    }
}

#[cfg(test)]
mod matcher_tests {
    use super::*;
    use proptest::prelude::*;

    /// Brute force: can steps `lo..hi` each get a distinct event of the
    /// group other than `exclude`?
    fn brute(group: &[Event], lo: usize, hi: usize, exclude: usize) -> bool {
        fn assign(group: &[Event], step: usize, hi: usize, used: &mut Vec<usize>) -> bool {
            if step == hi {
                return true;
            }
            for e in 0..group.len() {
                if !used.contains(&e) && group[e].condition(step) {
                    used.push(e);
                    if assign(group, step + 1, hi, used) {
                        return true;
                    }
                    used.pop();
                }
            }
            false
        }
        assign(group, lo, hi, &mut vec![exclude])
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(5_000))]

        /// Every shortcut and the cached matching agree with brute force,
        /// for every step range and excluded event, queried in an order that
        /// exercises the cache.
        #[test]
        fn distinct_events_exist_matches_brute_force(
            masks in prop::collection::vec(0u32..64, 1..7),
            steps in 1usize..=6,
        ) {
            let group: Vec<Event> = masks.iter().map(|&m| Event::new(0, m)).collect();
            let mut matcher = Matcher::new(&group, steps);
            for exclude in 0..group.len() {
                for lo in 0..=steps {
                    for hi in lo..=steps {
                        prop_assert_eq!(
                            matcher.distinct_events_exist(lo, hi, exclude),
                            brute(&group, lo, hi, exclude),
                            "lo={} hi={} exclude={} masks={:?}", lo, hi, exclude, masks
                        );
                    }
                }
            }
        }
    }
}
