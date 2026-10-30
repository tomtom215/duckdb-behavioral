# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.11.0] - 2026-10-30

No change to the SQL surface (540 overloads). Four results change:
`sequence_match('.*')` over a group whose rows carry no true condition is now
true; a `sequence_next_node` allocation failure is now a SQL error;
`sessionize` over a window that is not ordered by ascending timestamp is now an
error instead of a silently wrong session ID; and the conflicting-mode error
names `strict_deduplication` instead of its alias `strict`.

### Fixed

- `window_funnel` with `strict_once` took seconds on a timestamp that many rows
  share when their conditions overlap: every (step, excluded event) pair was
  matched separately, so the cost was about steps^4 whatever the group size.
  `window_funnel(INTERVAL 1 hour, 'strict_once', ts, <31 true>, false)` over
  64,000 rows with 32 rows per timestamp, one thread: 9.8, 10.6, 10.7 s before,
  0.66, 0.65, 0.56 s after (same result; shared 4-vCPU machine, three runs
  each). Events whose conditions agree inside the step range are
  interchangeable, so the cache is keyed on the excluded event's conditions.
- `sessionize` with `OVER (ORDER BY ts DESC)`, or ordered by another column,
  returned a session per row without an error. It now raises `sessionize: rows
  must arrive in ascending timestamp order`. The same error replaces the wrong
  counts of `sessionize` as a plain aggregate (no `OVER`), where DuckDB merges
  per-thread states in any order: 1,000,000 sorted minutes with a 2-minute gap
  returned 8 or 9 sessions at four threads instead of 1, and now returns 1 or
  the error. All 28 window frames of an audit matrix (ROWS, RANGE, GROUPS, every
  `EXCLUDE`) still match a list-based reference at 1 and 4 threads.
- `sequence_next_node` compared its direction and base strings byte by byte on
  every row (a 20 MB padded direction over 5,000 rows: 5-8.7 s); it now compares
  the 16-byte string slot like the other functions.
- The release workflow's "SQL logic tests" step set `DUCKDB_PLATFORM=linux_amd64`,
  which extension-ci-tools turns into a no-op, so the release gate never ran
  `test/sql` on its main platform.
- The documentation said `TIMESTAMP_NS` is truncated to microseconds (DuckDB 2.0.0
  rounds), that `(?1)(?2)` means no intervening events (rows matching no
  condition are dropped first), that `SET allow_unsigned_extensions` works inside
  a session (it errors), and showed two wrong results on the demo page.

- `sequence_next_node` aborted the process (SIGABRT, "memory allocation of 24
  bytes failed") when it ran out of memory collecting events; every other
  event-collecting function raised a SQL error. All growth is now fallible and
  reports `sequence_next_node: out of memory while collecting N events for one
  group`. Checked under `ulimit -v` with 150M events, 1 and 4 threads.
- `sequence_match` with a pattern of only `.*` returned false for a group where
  no row has a true condition; ClickHouse returns true. `sequence_count` stays
  0, `sequence_match_events` stays empty, and a group with no rows is still NULL.
- The error for an out-of-range INTERVAL (`INTERVAL 2147483647 DAY`) blamed
  months; it now says the value does not fit in 64-bit microseconds.

### Changed

- quack-rs 0.19.0 to 0.20.0 (`libduckdb-sys` stays `=1.20000.1`; quack-rs now
  accepts `>=1.4.4, <1.20100`). The SQL surface is unchanged: 540 overloads on
  DuckDB 1.3.2, 1.4.4, 1.4.5, 1.5.0, 1.5.6 and 2.0.0 (`scripts/compat-check.sh`
  asserts the count on each). Underneath: `LOAD` registers inside one
  transaction, so a `LOAD` that fails part-way leaves no function behind and can
  be retried (the commit path is what `compat-check.sh` and the integration
  tests exercise; the rollback path is quack-rs's, which it validated on 1.4.4,
  1.5.6 and 2.0.0); a build with `panic = "abort"` no longer compiles, where
  before it loaded and killed the process on the first panic; null tests on
  input vectors no longer make one C call per row. End-to-end SQL timings of
  the 0.19.0 build and this build (rustc 1.99.1) on DuckDB 2.0.0 (10M rows,
  100,000 groups, 4 threads, shared 4-vCPU Xeon @ 2.10 GHz, two rounds of 7
  runs with the arm order reversed in round 2): new/old ratios of the medians
  0.92 to 1.04, run ranges overlapping for all eight queries, so no speed
  change is claimed (`PERF.md`, Session 23). quack-rs's ABI check never
  refuses this build on a DuckDB release it does not list, because a build
  without a `duckdb-1-5*` feature is `StableOnly`.
- Rust 1.99.1 is the pinned toolchain. Clippy (pedantic, nursery, `-D warnings`),
  rustdoc and the 544 unit, 35 integration and 1 doc-test pass on it, and the
  MSRV (1.88) was re-checked with quack-rs 0.20.0.
- CI actions: `actions/checkout` v6.1.0 to v7.0.1 and `actions/setup-python`
  v6.3.0 to v7.1.0 (Dependabot #101; both only moved their internals to ESM,
  their inputs and outputs are unchanged). Dependabot #88, #89 and #90 are not
  applied: the tree already pins `codeql-action` v4.39.1 and
  `action-gh-release` v3.0.3, newer than the versions they propose, and #93
  (quack-rs 0.16.0) is superseded. Each pin was checked against its tag with
  `git ls-remote`.
- Dependabot ignores minor and major `libduckdb-sys` bumps: quack-rs 0.20.x
  requires `<1.20100`, so such a bump cannot resolve.
- The in-process extension-load tests are behind a new `e2e` feature, so a fresh
  clone runs `cargo test --lib` (544 tests) with no libduckdb and no arrow build;
  everything is `DUCKDB_DOWNLOAD_LIB=1 cargo test --all-features`. The explicit
  `duckdb` dev-dependency (named in no source file) is gone; `Cargo.lock`
  changes by one line and the release dependency graph is unchanged.
- CI: nextest runs `--lib --tests` (it ran the Criterion benchmarks as tests at
  full scale), the benchmark-compile job is gone (clippy type-checks them), the
  gate no longer waits for coverage, and `wasm-check` became `cross-check`
  (also Windows x86_64, macOS x86_64, Linux ARM64). A stable release tag must be
  a commit of `main` on which `CI Gate` passed, and a build that changes
  `Cargo.lock` fails. `deny.toml` denies unknown registries and git sources.
- DuckDB 1.4.5 joins the compat matrix (the community channel builds 1.4.5,
  1.5.6 and 2.0.0). On it, as on 1.4.4, the duckdb/duckdb#26109 shapes crash;
  the fix (duckdb/duckdb#26455) is on the 2.0 line only.
- `FunnelMode::STRICT` is `STRICT_DEDUPLICATION` and the old
  `STRICT_DEDUPLICATION` is `TIMESTAMP_DEDUP`, matching the SQL names (internal).
- `SECURITY.md` and the security page ask for a private advisory, not a public
  issue. The GitHub Linux archive needs glibc 2.34 (the community build, 2.28).

- `sequence_next_node` keeps a state's values in one byte buffer, and each event
  is a 24-byte record with a range into it (a value equal to the previous
  event's is stored once). Before: an `Arc<str>` per event. DuckDB 2.0.0, 10M
  rows in 100k groups, 4 threads, 9 interleaved runs per build: CPU time median
  5.08 s to 2.66 s. The memory DuckDB leaves behind after `GROUP BY .. LIMIT 10`
  over 300k groups fell from 246 MB to 136 MB (the leak itself is DuckDB's).
- The FFI layer is safe Rust over `src/ffi/view.rs`. `unsafe_code = "deny"`
  everywhere except `mod ffi`, and inside it `clippy::undocumented_unsafe_blocks`
  and `multiple_unsafe_ops_per_block` are denied. `States::pair` now panics
  rather than hand out aliasing references. No measurable change in CPU time
  (8 SQL aggregates, 10M rows, 11 interleaved runs, ratios 0.94 to 1.03 with
  overlapping ranges).
- Edition 2024 (MSRV stays 1.88). `rust-toolchain.toml` pins an exact release
  (1.99.1 now) instead of `stable`. `libduckdb-sys` / `duckdb` 1.20000.0 to 1.20000.1 (identical
  bindings). `publish = false`.
- CI rebuilt: a shared `setup-rust` action, `--locked` everywhere, nextest, a
  hygiene job (cargo-machete, typos, actionlint, zizmor, shellcheck, SPDX
  headers), coverage, a gate that fails on any non-success without listing job
  names, a reusable `e2e.yml` that asserts results, per-job permissions, and
  `release.yml` builds without a cache and takes release notes from this file.
  zizmor findings 264 (39 high) to 25 (none medium or high).

### Added

- Tests: `description.yml` is parsed with quack-rs's community-build validator
  (PyYAML `safe_load` rules, exact platform names, a 40-digit `ref`), and its
  name and version must equal the crate's.
- `scripts/check-docs.py` also fails when a document names a `quack-rs` or
  `libduckdb-sys` version other than the `Cargo.toml` pin (it found
  `libduckdb-sys 1.20000.0` in the engineering page, pinned `1.20000.1`).
- `scripts/bench-hits.py`: a repeatable benchmark of the eight functions as SQL on
  the public ClickBench `hits` data (1M, 10M or 100M rows). It pins every Parquet
  file's size and SHA-256, runs a same-columns scan as a control and pure-SQL
  baselines for `sessionize` and `retention`, fails unless results are identical
  across repetitions, thread counts and baselines, and records every individual
  time, the exact SQL, the machine and the extension's SHA-256 as JSON. Method
  and results: `docs/src/internals/hits-benchmark.md`; `scripts/bench-hits-charts.py`
  draws its three figures from the result JSON. On a shared 4-vCPU virtual machine
  a three-step funnel over all 99,997,497 events took 7.0 s and `sessionize` 9.4 s,
  against 2.4 s for a plain scan of the same columns (3 runs each); `retention`
  was level with its pure-SQL equivalent and `sequence_next_node` was the slowest
  query. Its first cross-check found a NULL bug in its own `sessionize` baseline
  (fixed before any result was recorded); the extension was right.
- `docs/function_descriptions.csv`: the description, comment and a runnable
  example for each of the nine functions, which duckdb.org shows on the
  extension's page when the file is copied into the community-extensions pull
  request (the stable C API cannot attach descriptions to a function:
  duckdb-behavioral#94). `tests/extension_load.rs` checks that it lists exactly
  the registered functions and that every example runs; RELEASING.md step 3
  has the copy.
- `scripts/check-docs.py` (CI, `check.sh`): fails on a stale unit, integration,
  property-test, directive or compat-query count, a CI release list that omits
  a release in `e2e.yml`, a broken link or anchor, a page missing from
  `SUMMARY.md`, a demo result that differs from `test/sql/git_mining.test`, or a
  release version in a command-line example that differs from `Cargo.toml`.
- `fuzz/`: cargo-fuzz targets for the parser, the sequence matcher and the funnel,
  run weekly.
- Tests: the duckdb/duckdb#26109 shapes on all eight functions in-process, in
  `compat-check.sh` and in `nightly-smoke.py` (2.x only); `sessionize` against a
  list reference in nine window frames over 3,000-row partitions; the plain-
  aggregate `sessionize` invariant; awkward `sequence_next_node` values (NUL,
  empty, multi-byte, 100 kB); long patterns (28-69 steps, 65-299 events) against
  the backtracking reference; strict_once candidate truncation against brute
  force; a guard that the Makefile's C API version equals quack-rs's.

- `src/oracle_tests.rs`: `window_funnel` against an exhaustive reference, and
  order, split, combine-tree, fresh-target and double-finalize invariance for
  every state.
- `scripts/compat-check.sh` (21 asserted queries, run on every platform and
  DuckDB release in the matrix), `scripts/crash-shapes.sh` (which query shapes
  crash which DuckDB release), `scripts/install-duckdb-cli.sh`,
  `scripts/release-notes.sh`, `RELEASING.md`, and `scheduled.yml` (weekly
  advisories, next stable and beta toolchains, `cargo update`, DuckDB nightly).
- `scripts/clickhouse-sequence-diff.py`: runs `sequence_match`, `sequence_count`
  and `sequence_match_events` against `clickhouse local` on random groups and
  checks each engine against a Python model of it (57.6 million results over
  two seeds, no exception). Test `test_events_report_the_completing_chain_not_an_abandoned_attempt`
  pins the case where ClickHouse reports a failed attempt that shares its first
  step with the match.
- `scripts/check-stable-api.py`, run in CI: fails if the unstripped library
  calls a C API function outside the stable prefix (77 of 548 slots used,
  highest 306, limit 357), the mistake that once pinned the binary to one
  DuckDB release. `scripts/bench-sql.sh`: end-to-end SQL timings.
- Exact overload counts (540 in all) asserted in `tests/extension_load.rs`.
- Parser tests that arbitrary input never panics.

### Removed

- `community-submission.yml`, `scripts/setup.sh`, `scripts/run_all_benchmarks.sh`,
  the `semver` CI job (the crate is unpublished; the SQL surface is the API),
  the test-only `SessionizeState`, and duplicate or length-only tests the
  invariance checks supersede.

### Documentation

- `TIMESTAMPTZ_NS`, the type DuckDB 2.0.0 added, has no implicit cast to
  `TIMESTAMP` or `TIMESTAMPTZ`, so passing it to any function here is a binder
  error; `ts::TIMESTAMPTZ` works and rounds to the nearest microsecond (checked
  on 2.0.0). Stated on every function page, in the quick reference and in the
  README. `TIMESTAMP_NS`, `_MS`, `_S` and `DATE` convert implicitly.
- The CI job that checks the WebAssembly target is `cross-check`, not
  `wasm-check`.
- Which query shapes crash which DuckDB release is now a measured matrix
  (`OVER (PARTITION BY g)` without `ORDER BY` also crashes 1.3.2 and 1.4.4;
  nothing crashes 2.0.0), and the `GROUP BY .. LIMIT` memory leak is documented.
- ClickHouse parity was re-run against 26.10.2.103 (and `window_funnel` against
  26.9.8.3); counts and the one partly verified explanation are in
  `docs/src/internals/clickhouse-compatibility.md`. ClickHouse's setting is
  `enable_funnel_functions`.
- README rewritten; test, overload and CI descriptions corrected throughout.
- The 299 `sequence_match_events` differences from ClickHouse that the first
  parity run could not explain are explained: ClickHouse reports the first
  chain that reached the longest length even if that attempt failed a later
  time constraint. The compatibility page now states the rules of both engines
  (time constraint after `.*`, ties, failed attempts). Two earlier statements
  were wrong: 160 `sequence_match` differences blamed on ClickHouse's
  out-of-bounds read were the empty-events `.*` case fixed in this release, and
  that read changes answers between runs on ClickHouse 26.9.8.3 but not on
  26.10.2.103.

## [0.10.1] - 2026-10-23

DuckDB 2.0.0 support. No change to the SQL surface or to results.

### Changed

- quack-rs 0.18.0 → 0.19.0. libduckdb-sys / duckdb 1.10506.0 → 1.20000.0
  (DuckDB v1.5.6 → v2.0.0). No source change was needed.
- **MSRV 1.87 → 1.88.** libduckdb-sys and duckdb 1.20000.0 declare 1.88
  (quack-rs 0.19.0 declares 1.86.0). The CI MSRV job now runs `cargo +1.88`.
- Lockfile refreshed (`cargo update`, 69 entries). The only one linked into
  the extension is `smallvec` 1.15.2 → 1.16.3; the rest are dev, test and
  build dependencies. `smallvec` 2.x is still a beta and was not adopted.
- The binary is still stamped `C_STRUCT`, C API `v1.2.0`. It still uses 76
  stable-prefix slots, now of 548, and the highest is still 306. The one
  release build loads and answers a `window_funnel` query correctly in DuckDB
  1.3.2, 1.4.4, 1.5.0, 1.5.6 and 2.0.0, and `compat` in `e2e.yml` gains a
  v2.0.0 leg.
- CI and `scripts/setup.sh`: DuckDB 2.0.0 renamed its CLI assets to
  `duckdb-cli-<platform>.tar.gz` (the `linux-aarch64` zip is now
  `linux-arm64`). The old `duckdb_cli-*.zip` URLs return 404 for v2.0.0.
  `e2e.yml` and `setup.sh` download the new names; `compat` falls back to the
  zip for older releases. The SQL logic tests run against DuckDB 2.0.0
  (`DUCKDB_TEST_VERSION`).

### Documentation

- The "libduckdb-sys's init reads past an older DuckDB's shorter API struct"
  risk is now measured, not conjectured: under valgrind the stamped binary
  loaded into DuckDB 1.3.2 and 1.4.4 and answered a query per function with 0
  invalid reads or writes. The only reports are "conditional jump depends on
  uninitialised value" inside `behavioral_init_c_api` during `LOAD` (59 and
  52), which the same CLIs without the extension, and 2.0.0 with it, do not
  produce. The over-read is in libduckdb-sys's init, not in this crate.
- The three crashing query shapes (duckdb/duckdb#26109: `ORDER BY` inside the
  call, `OVER ()`, an `UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING` frame)
  segfault on DuckDB 1.5.6 (exit 139) and did not crash on 2.0.0 in the same
  test. The warnings now say "1.5.6 and earlier"; the workarounds stay for
  older releases.
- Running window frames: re-measured on 2.0.0 with the same query. Peak
  memory is unchanged (1,243 MB vs 1,248 MB on 1.5.6 at 40,000 rows), so the
  quadratic-frame warning stands.

## [0.10.0] - 2026-10-02

Results change for some inputs (see **Behaviour changes**).

### Fixed

- **A long pattern on a large group no longer aborts DuckDB.**
  - What happened: the sequence matcher's working memory (one byte per
    event per step) sits outside DuckDB's `memory_limit`, and a failed
    allocation aborted the process. `repeat('.', 60000)` over 60,000 rows
    under a 3 GB limit exited with code 134.
  - Changes:
    - The working memory is now one bit per event per step: an 801-step
      pattern over 1M events peaked at 148 MB (819 MB before).
    - Patterns are capped at 1024 steps.
    - Allocation failures in the matcher, and while collecting events in
      update and combine, raise an `out of memory` error and the session
      continues.
- **Configuration arguments that differ between rows of a group are an
  error.**
  - What happened: the pattern, window, mode, direction and base were taken
    from the first row each partial state happened to see. Results depended
    on row order and thread scheduling: the same two rows gave 2 in one
    order and 1 in the other. An invalid value on a later row was ignored.
  - Now: every update and combine compares the values, so the error does
    not depend on order. `NULL`s are still ignored.
- **`window_funnel` with `strict_once` is 19–28× faster.** Measured on
  DuckDB 1.5.6:
  - 250k rows in 1,000 groups with 31 conditions: 4.0–4.1 s → 0.18–0.21 s
    (default mode 0.09–0.10 s).
  - A 1M-row same-timestamp burst: 6.5 s → 0.23 s.
  - Results matched the exhaustive reference in all 648,000 fuzzed cases.
- **Queries with many small groups are 1.4–6.5× faster** (2M groups of 2
  rows on DuckDB 1.5.6): `window_funnel` 0.59–0.81 s → 0.17–0.18 s and
  `sequence_match` 1.16–1.28 s → 0.18–0.19 s with 4 threads. Small groups no
  longer allocate per row, and the pattern is shared between groups instead
  of copied. A single very large group is 5–10% slower (see PERF.md
  Session 20).
- **`window_funnel` with a `NULL` mode on some rows and `'strict_order'` on
  others** dropped rows with no true condition that arrived before the mode,
  so they did not break the chain (2 instead of 1).
- **`sequence_match_events` without a full match is up to 3× faster** for
  gaps of nothing, `.*` or one time constraint other than `==`. A 201-step
  pattern over 1M events took 10.1 s → 3.3 s.
- **`sequence_next_node` with a `NULL` direction and base `'tail'`** returned
  `NULL`. A `NULL` direction means `'forward'`, so this is now the documented
  `forward`/`tail` error. The check runs at finalize, independent of row order.
- **Error messages**:
  - Multi-byte characters are named whole (`'é'` printed as `'Ã'` before).
  - Long patterns are shortened to 60 characters plus their length.
- **CI could not fail**: the clippy, test, semver and coverage steps piped
  into `tee` under `bash -e` without `pipefail`, so their status was always 0.
- **The release workflow's `linux_arm64` artifact was built for x86-64**: the
  community Makefile passes `--target` only for macOS, so the cross-compile
  job built for its own architecture. It now runs on an arm64 runner, and
  every release build checks the library's architecture with `file`.

- **Sequence patterns no longer take quadratic time per group.**
  `sequence_match`, `sequence_count` and `sequence_match_events` matched any
  pattern beyond the two fast-path shapes with a backtracking search that
  re-scanned the rest of the group from every start. That made a group
  quadratic whenever the pattern did not complete, for example after `.*`
  followed by a time constraint, or after a skipping constraint such as
  `(?t>=5)`. It also could not be cancelled. At 32,000 events in one group,
  the old build took 8.3–9.1 s for `(?1).*(?t<5)(?2).*(?3)`, 1.8–2.4 s for
  `(?1)(?t>=5)(?3)`, and 32–35 s for `sequence_match_events` on the first
  pattern. The new matcher takes 0.003–0.007 s on all three, and 0.78–1.29 s
  at 10 million events. These are SQL timings on DuckDB 1.5.6, 3 runs each.
  It computes, per event-consuming step, the positions from which the rest
  of the pattern can still complete, then walks forward greedily. This is
  O(s · n log n) for `n` events and `s` pattern steps, and the results are
  identical:
  - Differential property tests compare it with the original search (kept
    test-only) on 20,000 random pattern/event cases per run. An extended
    400,000-case run with groups of up to 40 events also passed.
  - The ClickHouse differential fuzzer produced byte-identical output with
    the old and new builds over 14,883 groups.

  The exploration-budget error ("pattern exploration budget exceeded") no
  longer exists: patterns that used to hit it now return their result.
  One trade-off: a pattern with a time constraint where every pair matches
  and nothing is skipped (Criterion `sequence_match_time_constraint`,
  `(?1)(?t<=600)(?2)`) went from 14.3 ms to 21.6–23.9 ms at 1 million events
  (two runs), because the matcher always makes its backward pass. See
  PERF.md Session 19.

- **`window_funnel` / `window_funnel_events` now follow ClickHouse's
  `windowFunnel`.** Differential testing against ClickHouse 26.9.8.3 found
  the previous greedy scan disagreeing in 5–30% of random groups for every
  mode combination but one. The engine is now a port of ClickHouse's
  algorithm:
  - An event satisfying several conditions fills several steps, the entry
    step included.
  - `strict_order` is broken by condition-less events and by a step arriving
    early.
  - `strict_deduplication` stops on any repeat of a reached step.
  - `allow_reentry` skips early steps instead of resetting the chain.

  Two ClickHouse defects are deliberately not reproduced:
  - ClickHouse loses valid chains under `strict_increase`.
  - Under `strict_once`, ClickHouse orders tied rows by arrival. The
    extension orders them by bitmask instead, and uses bipartite matching
    rather than exponential chain enumeration.

  Over 1,296,000 cases the extension equals an exhaustive reference.
- **`sequence_match` / `sequence_count`**: patterns mixing adjacent
  conditions with `.*` (`(?1)(?2).*(?3)`) lost the adjacency requirement and
  reported false matches and inflated counts.
- **`sequence_count`** never returned for a pattern that can match zero
  events (`.*`).
- **`sequence_next_node`**: rows tying on (timestamp, value) gave results that
  depended on row order (flipping from 33 tied rows); the sort key is now
  total.
- **`sessionize`**: with a gap that varies per row, results depended on how
  DuckDB's segment tree split the frame. The boundary between segments is
  now judged by the right segment's first row. A ±infinity gap is also now
  computed exactly in combine.
- **`window_funnel(NULL::INTERVAL, ...)`** returned 1 (a zero-length window)
  instead of skipping the row.
- **`interval_to_micros`** rejected a mixed-sign interval whose total fits in
  `i64`.
- **Panics no longer kill DuckDB.** The release profile set
  `panic = "abort"` and every aggregate callback was an unguarded
  `extern "C" fn`. Callbacks now use quack-rs's guarded
  `aggregate_*_callback!` macros under `panic = "unwind"`, so a panic becomes
  a SQL error.
- **The extension loads on any DuckDB release from 1.3.2.** It was built for
  the unstable C API (`C_STRUCT_UNSTABLE`), which pins a binary to one DuckDB
  release (v1.5.5). The community channel therefore stopped serving it when
  DuckDB v1.5.6 shipped. It uses only the stable C API (76 of 546 slots,
  highest 306 of the 357-slot stable prefix) and is now stamped `C_STRUCT` /
  C API v1.2.0. CI loads one binary into DuckDB 1.3.2, 1.4.4, 1.5.0 and
  1.5.6.

### Behaviour changes

- A group whose configuration arguments differ between rows is an error (see
  **Fixed**).
- Patterns longer than 1024 steps are rejected.
- `window_funnel` mode names are case-insensitive, like `sequence_next_node`'s
  direction and base (`'STRICT_ORDER'` was an error).
- `sequence_match` / `sequence_count` return `NULL` for a group whose
  timestamps are all `NULL`, as for an empty group (was `false` / `0`).

- Errors now raised (previously silently accepted, now rejected as in
  ClickHouse or because the input has no defined meaning):
  - `window_funnel` with `'allow_reentry'` but without `'strict_order'`.
  - A `(?N)` larger than the number of conditions passed.
  - A time constraint before any `(?N)` or `.` (it used to be treated as
    always true).
  - `sequence_next_node` with `forward` + `tail` or `backward` + `head`
    (these always returned NULL).
- `window_funnel` / `window_funnel_events` accept a single condition, as
  ClickHouse does.
- `'timestamp_dedup'` is documented as what it always computed: an alias of
  `strict_increase`. `'strict'` stays accepted as an alias of
  `'strict_deduplication'` (ClickHouse 26.9 rejects it).
- `window_funnel_events` returns, when the funnel completes, the
  latest-entry chain completed by the first event that completes it;
  otherwise the chain with the latest entry among those reaching the most
  steps.

### Added

- **`TIMESTAMPTZ` is accepted wherever `TIMESTAMP` is.** Before, it was a
  binder error, and the `::TIMESTAMP` workaround converts to local time,
  which can reorder events around a daylight-saving change.
  `window_funnel_events` and `sequence_match_events` return `TIMESTAMPTZ[]`
  for `TIMESTAMPTZ` input. The extension now registers 540 overloads, and
  `LOAD` still takes 0.03–0.12 s.

### Changed

- quack-rs 0.15.0 → 0.18.0. libduckdb-sys / duckdb 1.10505.0 → 1.10506.0
  (DuckDB v1.5.5 → v1.5.6). State callbacks are installed with
  `ffi_state::<T>()`.
- rustls 0.23.42 → 0.23.45 (RUSTSEC-2026-0285; build-time downloader only).
- CI: the MSRV job now actually runs 1.87 (`cargo +1.87`; the toolchain file
  had redirected it to stable). `e2e.yml` gains a `compat` job.

### Documentation

- The README and docs-index "User Flow Analysis" example used `GROUP BY ALL`
  over aggregates only. That merged all users into one sequence and counted
  events (it returned `Cart | 9` instead of `Cart 2, Search 1`). It now
  groups by user first.
- The crash list is precise. It covers `agg(... ORDER BY ...)`, `OVER ()` and
  `UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING` frames, with verified safe
  alternatives: `OVER (PARTITION BY 1)`, and `OVER (PARTITION BY u)` without
  `ORDER BY`.
- New FAQ and README sections:
  - Running window frames are quadratic for the event-collecting functions
    (40,000 rows: 11.1 s, 1.25 GB).
  - A query cannot be cancelled while one group is finalized; the C API
    exposes no interrupt check.
  - Memory outside `memory_limit`.
- `TIMESTAMP_NS` input is truncated to microseconds.

- Documented DuckDB's C API defect duckdb/duckdb#26109, verified under
  valgrind on 1.5.6: `agg(... ORDER BY ...)`, `OVER ()` and whole-partition
  frames read out of bounds and usually segfault for every C API aggregate.
  The FAQ had suggested `ORDER BY` inside the call for speed.
- Corrected the parity claims to the differential-testing results. Also
  corrected install/load instructions (a bare `.so` cannot be loaded), NULL
  handling, example outputs, counts, and performance-number provenance.


## [0.9.1] - 2026-07-23

### Changed

- **DuckDB v1.5.5 support** — upgraded `libduckdb-sys` from `1.10504.0` to
  `1.10505.0` and `duckdb` (dev) from `1.10504.0` to `1.10505.0`, tracking the
  DuckDB v1.5.5 bugfix release (published one day after v1.5.4). The C extension
  API version is unchanged (`v1.2.0`) and the metadata ABI stays
  `C_STRUCT_UNSTABLE`, so the extension source compiles unchanged; **quack-rs
  stays 0.15.0** (its `libduckdb-sys >=1.4.4, <2` range already covers 1.10505.0)
  and **MSRV stays 1.87**. `TARGET_DUCKDB_VERSION` / `DUCKDB_TEST_VERSION`
  (`Makefile`), `DUCKDB_VERSION` (`e2e.yml`), `DUCKDB_RELEASE_VERSION`
  (`scripts/setup.sh`), and the `DUCKDB_VERSION` metadata constant in
  `tests/extension_load.rs` now pin `v1.5.5`; E2E runs against the DuckDB v1.5.5
  CLI, and metadata-append examples stamp `-dv v1.5.5 -ev v0.9.1`. Cut to keep
  the DuckDB community-extensions listing on the current DuckDB release line —
  `C_STRUCT_UNSTABLE` extensions must match the loading DuckDB version exactly.

### Security

- **Smaller build-dependency tree** — `libduckdb-sys` 1.10505.0 swapped the
  `reqwest` prebuilt-library downloader for the lightweight `ureq` v3, dropping
  ~95 transitive lockfile entries (the whole `hyper` / `quinn` / `tokio` /
  `rustls`-heavy / `icu` subtree, including the `quinn-proto` that needed a
  security patch in 0.9.0). All are build/dev-only — none ever reached the
  shipped `.so` — but the reduced supply-chain surface is a nice side effect.
  The `DUCKDB_DOWNLOAD_LIB=1` dev/CI download path is unaffected (ureq + rustls).
- **Dependency refresh via `cargo update`** — routine dev/transitive bumps
  (`arrow` 58.3 → 58.4, plus `clap`, `libc`, `serde_json`, `syn`, `zerocopy`);
  no advisories this cycle. `cargo deny check advisories bans licenses sources`
  passes clean.

### Documentation

- Updated every version reference to the `v0.9.1` / DuckDB `v1.5.5` baseline
  (quack-rs `0.15.0` and MSRV `1.87` unchanged) across `README.md`, `CLAUDE.md`,
  `SECURITY.md`, `description.yml`, `scripts/setup.sh`, `Makefile`,
  `docs/src/**.md`, the issue templates, the community-submission workflow, and
  the `DUCKDB_VERSION` metadata constant in `tests/extension_load.rs`.

## [0.9.0] - 2026-07-19

### Changed

- **quack-rs v0.15.0** — upgraded from v0.14.0. Every public API this crate uses
  (`AggregateFunctionSetBuilder`, `AggregateFunctionBuilder`,
  `ScalarFunctionBuilder`, `FfiState`, `VectorReader`/`VectorWriter`,
  `ListVector`, `LogicalType::list`, `returns_logical`, `AggregateTestHarness`,
  `Connection`/`Registrar`, `entry_point_v2!`, `testing::InMemoryDb`) is
  unchanged: the extension compiles against the new SDK with **zero changes to
  existing source**. v0.15.0 is purely additive — it adds `Value::as_blob()` and
  fixes `VectorReader::read_blob()` to preserve non-UTF-8 bytes; neither blob API
  is exercised by this aggregate-only extension. MSRV stays 1.87.
- **DuckDB v1.5.4 support** — upgraded `libduckdb-sys` from `1.10503.1` to
  `1.10504.0` and `duckdb` (dev) from `1.10503.1` to `1.10504.0`, tracking the
  DuckDB v1.5.4 bugfix release. The C extension API version is unchanged
  (`v1.2.0`) and the metadata ABI stays `C_STRUCT_UNSTABLE`, so the extension
  source compiles unchanged. `TARGET_DUCKDB_VERSION` / `DUCKDB_TEST_VERSION`
  (`Makefile`), `DUCKDB_VERSION` (`e2e.yml`), and `DUCKDB_RELEASE_VERSION`
  (`scripts/setup.sh`) now pin `v1.5.4`; E2E tests run against the DuckDB v1.5.4
  CLI, and metadata-append examples stamp `-dv v1.5.4 -ev v0.9.0`.

### Security

- **Dependency refresh via `cargo update`** — notably **crossbeam-epoch**
  0.9.18 → 0.9.20 and **quinn-proto** 0.11.14 → 0.11.16, carrying the upstream
  fixes for **RUSTSEC-2026-0204** and **RUSTSEC-2026-0185** respectively. Both
  are dev/transitive crates reached only through `libduckdb-sys`'s build tooling
  and the `duckdb` dev-dependency — neither is linked into the shipped `.so`,
  which contains only `libduckdb-sys` + `quack-rs`. Routine bumps swept the
  dev/transitive tree (`cc` 1.2.63 → 1.3.0, `serde` 1.0.228 → 1.0.229, `tokio`
  1.52.3 → 1.53.0, plus `rustls`, `regex`, `zerocopy`, `wasm-bindgen`, …), and
  `libduckdb-sys` 1.10504.0 drops the `windows-sys` / `windows-targets`
  transitive subtree. `cargo deny check advisories bans licenses sources` passes
  clean; the project MSRV stays 1.87.

### Documentation

- Updated every version reference to the `v0.9.0` / quack-rs `v0.15.0` / DuckDB
  `v1.5.4` baseline (MSRV `1.87` unchanged) across `README.md`, `CLAUDE.md`,
  `SECURITY.md`, `description.yml`, `scripts/setup.sh`, `Makefile`,
  `docs/src/**.md`, the issue templates, the community-submission workflow, and
  the `DUCKDB_VERSION` metadata constant in `tests/extension_load.rs`.
- Corrected pre-existing drift in `README.md`: the CI-job table read `13`
  (omitting `wasm-check`) and now reads `14`, matching `ci.yml` and
  `docs/src/engineering.md`; the E2E row read `11 workflow steps + 59 SQL
  queries` and now reads `12 workflow steps + 8 SQL files, 76 queries`, matching
  the verified `e2e.yml` / `test/sql` counts.

## [0.8.0] - 2026-06-12

### Added

- **`window_funnel_events` function** — `LIST(TIMESTAMP)` of the best funnel
  chain: one timestamp per matched step, in match order (list length always
  equals `window_funnel`'s step count for the same arguments). Companion to
  `window_funnel` for funnel debugging and step-to-step latency analysis;
  extension beyond ClickHouse. Shares `WindowFunnelState` and the
  update/combine FFI callbacks; the greedy scan is generic over a zero-sized
  step recorder so `window_funnel`'s hot path is unchanged.
- **`behavioral_version()` scalar** — returns the loaded extension version
  (the first scalar function; registered via quack-rs `ScalarFunctionBuilder`).
- **Strict configuration validation** — invalid configuration now aborts the
  query with a descriptive SQL error (via
  `duckdb_aggregate_function_set_error`) instead of silently producing wrong
  results or NULL: unknown `window_funnel` mode strings (message lists all
  valid modes), month-based or negative INTERVAL windows/gaps, malformed
  sequence patterns (the parser's position-annotated message), and unknown
  `sequence_next_node` direction/base values. NULL configuration parameters
  remain lenient.
- **`wasm-check` CI job** — the crate compiles for `wasm32-unknown-emscripten`
  (DuckDB-WASM) since quack-rs 0.14.0; CI now enforces it. The community
  descriptor still excludes wasm platforms pending a verified emscripten
  link + load.

### Fixed

- **`sequence_next_node` now matches ClickHouse exactly** (verified against
  `AggregateFunctionSequenceNextNode.cpp`). Three divergences fixed:
  `head`/`tail` anchor at the literal first/last event (which must satisfy
  the base condition) rather than scanning for one; the event chain must
  match **consecutive** events (interleaved non-matching events break it, and
  a failed chain is not retried at other anchors); events sort by
  `(timestamp, value)` so same-timestamp results are deterministic.
- **Timestamp gap arithmetic saturates instead of wrapping** — with DuckDB's
  `±infinity` timestamps (`±i64::MAX` internally), the gap computations in
  `sessionize` (update + combine), `window_funnel`'s window check, and the
  pattern executor's `(?t…)` elapsed-time evaluation previously overflowed
  and wrapped in release builds, silently treating infinitely distant events
  as adjacent. All sites now use `saturating_sub`.
- **Pattern time constraints reject numbers above `i64::MAX`** — values in
  `(i64::MAX, u64::MAX]` previously wrapped negative through an unchecked
  cast, inverting the constraint (e.g. `(?t>=18446744073709551615)` became
  `(?t>=-1)`). Now a position-annotated pattern error.
- **Time constraints now match ClickHouse exactly** (verified against
  `AggregateFunctionSequenceMatch.cpp`): `(?t op N)` gates the next pattern
  step while *skipping* non-matching events in between — previously
  `(?1)(?t<=N)(?2)` required the two conditions to be adjacent and silently
  missed matches ClickHouse finds. Trailing `(?t<=N)`/`(?t<N)`/`(?t>=0)`
  now match the empty remainder, also as in ClickHouse. Elapsed time is
  floored to whole seconds (documented adaptation: ClickHouse compares raw
  column units; the floor generalizes whole-second `DateTime` behavior).
- **`sequence_match_events` returns the longest partial chain on no-match**
  (ClickHouse `sequenceMatchEvents` semantics) instead of an empty list —
  the empty list now means "no condition ever fired". Documentation
  previously mis-classified this function as an extension beyond ClickHouse;
  it is one of the six ClickHouse behavioral functions (and `.` is ClickHouse
  syntax too — only `(?t!=N)` is our pattern-syntax extension).
- **NFA exploration budget no longer causes silent false negatives** — the
  fixed 10,000-iteration cap could make complex patterns (time constraints,
  `.`) report "no match" on groups with more than a few thousand
  non-matching events even when a match existed. The budget now scales with
  input size (`8 × events × steps`), consecutive `.*` wildcards are collapsed
  at parse time (they are semantically one wildcard but each copy multiplied
  the NFA branching factor), and if a truly adversarial pattern still
  exhausts the budget the query fails loudly with a descriptive error
  instead of silently returning a wrong result.

### Changed

- **Deterministic results under parallel aggregation** — shared event sorting
  now uses the full `(timestamp, conditions)` key (with a matching presorted
  check), so `window_funnel`, `window_funnel_events`, `sequence_match`,
  `sequence_count`, and `sequence_match_events` produce identical results
  regardless of thread count or physical row order. Verified by an
  integration probe that hashes results across `threads=1/4` and reversed
  insertion order. (ClickHouse's `windowFunnel` achieves the same via
  stable-sorted `(timestamp, event_index)` pairs.)
- **`sessionize` FFI migrated to quack-rs `AggregateFunctionBuilder`** — the
  last hand-rolled raw `libduckdb-sys` module is gone; every function now
  registers through the `Registrar` trait. Behavior-preserving (identical
  C-API call sequence, default NULL handling); adds the combine-harness tests
  sessionize never had.
- **Dev/test builds link a prebuilt libduckdb** (quack-rs
  `bundled-test-prebuilt` + `DUCKDB_DOWNLOAD_LIB=1`) instead of compiling
  DuckDB's C++ tree from source — cold test setup drops from ~25 minutes to
  the Rust wrapper crates only. Release builds are untouched (verified: no
  libduckdb `NEEDED`/`RUNPATH` in the shipped `.so`).
- **BREAKING (0.x minor)**: `ffi::sessionize::register_sessionize` now takes
  `&impl Registrar` instead of a raw `duckdb_connection`.

### Removed

- **`common::event::merge_sorted_events`** — dead public API (unused since
  the in-place combine rewrite) whose timestamp-only merge contract became
  inconsistent with the new `(timestamp, conditions)` ordering.

### Tests

- 486 unit tests (was 453) and 16 in-process integration tests (was 7),
  including: adversarial probes for `±infinity` timestamps and oversized
  time constraints, a ClickHouse-semantics matrix for all eight
  `sequence_next_node` direction/base combinations, parallel-combine
  determinism hashing, windowed `window_funnel` usage, and SQL error-path
  assertions for all validation errors. 8 E2E SQL files with 76 queries.


## [0.7.0] - 2026-06-07

### Changed

- **quack-rs v0.14.0** — upgraded from v0.13.0. Every public API this crate uses
  (`AggregateFunctionSetBuilder`, `FfiState`, `VectorReader`/`VectorWriter`,
  `ListVector`, `LogicalType::list`, `returns_logical`, `AggregateTestHarness`,
  `Connection`/`Registrar`, `entry_point_v2!`) is unchanged: the extension
  compiles against the new SDK with **zero changes to existing source**. v0.14.0
  is purely additive — it adds `wasm32-unknown-emscripten` (DuckDB-WASM) support,
  a `bundled-test-prebuilt` test feature, and `InMemoryDb::open_unsigned()`. MSRV
  stays 1.87; DuckDB stays v1.5.3 (`libduckdb-sys`/`duckdb` `1.10503.1`, already
  the latest).

### Added

- **In-process extension-load integration test** (`tests/extension_load.rs`, 7
  tests). Builds the real release `cdylib`, appends the DuckDB metadata footer
  (a Rust port of `append_extension_metadata.py`), and `LOAD`s it into an
  in-memory DuckDB via quack-rs 0.14.0's
  `testing::InMemoryDb::open_unsigned()`, then exercises **all seven functions**
  through live SQL. This runs inside `cargo test` with no external `duckdb` CLI
  and closes the long-standing gap where unit tests could pass while the FFI
  registration path was broken (the segfault-on-load / silent-non-registration /
  wrong-result class of bugs). Adds the `quack-rs` `bundled-test` dev-feature,
  which unifies with the existing `duckdb/bundled` dev-dependency — no extra
  DuckDB build.

### Security

- **Dependency refresh via `cargo update`** — notably `tar` 0.4.45 → 0.4.46
  (resolves **GHSA-3pv8-6f4r-ffg2**, "PAX header desynchronization"; `tar` is a
  build-time dependency of `libduckdb-sys`), plus `cc` 1.2.61 → 1.2.63 and
  `shlex` 1.3.0 → 2.0.1, and routine bumps across the dev/transitive tree
  (`arrow` 58.1 → 58.3, `tokio`, `wasm-bindgen`, `zerocopy`, …). All direct
  dependencies are at their latest versions; the sole held-back transitive crate
  is `comfy-table` (constrained by the `duckdb` dev-dependency). All bumped
  crates declare an MSRV ≤ 1.85, so the project MSRV stays 1.87.
- **`deny.toml`** — added a tightly-scoped `CC0-1.0` license exception for
  `tiny-keccak`, a phantom `Cargo.lock` entry behind `ahash`'s optional,
  never-enabled `compile-time-rng` feature (absent from every buildable graph and
  from the shipped `.so`). This lets current `cargo-deny` releases pass the
  license gate, future-proofing CI against the pinned action updating.

### Documentation

- Updated every version reference to the `v0.7.0` / quack-rs `v0.14.0` baseline
  (DuckDB `v1.5.3` and MSRV `1.87` unchanged) across `README.md`, `CLAUDE.md`,
  `SECURITY.md`, `description.yml`, `scripts/setup.sh`, `docs/src/**.md`, the
  issue/PR templates, and the community-submission workflow, and documented the
  new in-process integration-test layer.

## [0.6.0] - 2026-05-24

### Changed

- **DuckDB v1.5.3 support** — upgraded `libduckdb-sys` from `1.10502.0` to
  `1.10503.1` and `duckdb` (dev) from `1.10502.0` to `1.10503.1`. Restores
  community-extension compatibility with the current DuckDB release line (the
  registry had de-listed the extension for targeting an older version)
- **quack-rs v0.13.0** — upgraded from v0.12.0. The public APIs used by this
  crate (`AggregateFunctionSetBuilder`, `FfiState`, `VectorReader`/`VectorWriter`,
  `ListVector`, `LogicalType::list`, `AggregateTestHarness`,
  `Connection`/`Registrar` trait, `entry_point_v2!`) are unchanged: the crate
  compiles against the new SDK with **zero source changes**. New upstream
  capabilities in this range — the `error_data`, `expression`, `file_system`,
  `appender`, `selection_vector`, and `instance_cache` modules, plus
  `TypeId::Variant` / `TypeId::Geometry` behind the `duckdb-1-5-3` feature —
  are available but not consumed: the extension's aggregate-only surface area
  doesn't exercise them
- **MSRV bumped from 1.86 to 1.87** — quack-rs v0.13.0 corrected its declared
  MSRV to 1.87.0 to match `libduckdb-sys`. CI's `cargo check --all-targets`
  MSRV gate now pins 1.87
- E2E tests now run against DuckDB v1.5.3 CLI (previously v1.5.2)

### Fixed

- **Version drift in `scripts/setup.sh`** — the DuckDB CLI download URLs
  hardcoded `v1.5.2` in four places instead of deriving from the
  `DUCKDB_RELEASE_VERSION` constant. Refactored to build every URL from the
  single constant, removing a recurring source of stale references

### Documentation

Audited and updated all version/MSRV references across `README.md`,
`CLAUDE.md`, `CONTRIBUTING.md`, `SECURITY.md`, all of `docs/src/**.md`,
all `.github/workflows/*.yml`, `Makefile`, `description.yml`, and the
issue templates so every cross-reference is consistent with the
`v0.6.0` / DuckDB `v1.5.3` / quack-rs `v0.13.0` / MSRV `1.87` baseline.

A full accuracy pass over every doc against the source also corrected
several pre-existing defects unrelated to the version bump:

- **Broken metadata-append examples** in `docs/src/getting-started.md` and
  `docs/src/contributing.md` stamped `-dv v1.2.0 ... --abi-type C_STRUCT`,
  which produces an extension DuckDB 1.5.x rejects on load. Corrected to the
  real `-dv v1.5.3 ... --abi-type C_STRUCT_UNSTABLE` convention.
- **CI job lists** in `README.md` and `docs/src/engineering.md` claimed "13
  CI jobs" but enumerated only 12 (omitting `ci-gate`); both now list all 13.
- **`sequence_next_node` bases** in `docs/src/quick-reference.md` wrongly
  described `head`/`tail` as aliases of `first_match`/`last_match`; they are
  four distinct strategies (corrected to match the function reference).
- **Spurious `rand` dev-dependency** removed from the `docs/src/operations/security.md`
  dependency table — `rand` is only transitive; benchmarks generate data
  deterministically.
- **`attest-build-provenance@v3`** in `docs/src/operations/security.md`
  corrected to `@v4` (matching `release.yml`), and a stale `v0.2.0`
  verification example bumped to `v0.6.0`.
- Cleared stale "DuckDB 1.5.1" and "C API version v1.2.0" references in
  `README.md`, `docs/src/index.md`, `docs/src/engineering.md`,
  `docs/src/faq.md`, and `docs/src/getting-started.md`.
- Aligned the `AggregateTestHarness` config-propagation claim in `CLAUDE.md`
  to "all 6 aggregate functions" (across 5 FFI test modules).

## [0.5.0] - 2026-05-01

### Changed

- **DuckDB v1.5.2 support** — upgraded `libduckdb-sys` from `1.10501.0` to
  `1.10502.0` and `duckdb` (dev) from `1.10501.0` to `1.10502.0`
- **quack-rs v0.12.0** — upgraded from v0.7.1. Public APIs used by this crate
  (`AggregateFunctionSetBuilder`, `FfiState`, `VectorReader`/`VectorWriter`,
  `ListVector`, `LogicalType::list`, `AggregateTestHarness`,
  `Connection`/`Registrar` trait, `entry_point_v2!`) are unchanged. New
  capabilities introduced upstream during this range — `StructReader`/
  `StructWriter`, `ChunkWriter`, `MapVector`, `Value` RAII wrapper,
  `scalar_callback!` / `table_scan_callback!` panic-safe macros, expanded
  `LogicalType` introspection, `tls`/`warning`/`secrets` modules — are
  available but not consumed: the extension's aggregate-only surface area
  doesn't exercise them
- **MSRV bumped from 1.84.1 to 1.86** — required by the new `libduckdb-sys`
  / `duckdb` (1.85.1) and `criterion` 0.8.2 (1.86) crate metadata. CI's
  `cargo check --all-targets` MSRV gate enforces 1.86
- **`extension-ci-tools` submodule** updated to latest upstream
- **CI dependency updates** — `EmbarkStudios/cargo-deny-action` v2.0.15→v2.0.17,
  `taiki-e/install-action` →v2.75.27, `actions/deploy-pages` v4.0.5→v5.0.0,
  `actions/upload-pages-artifact` v4.0.0→v5.0.0,
  `actions/upload-artifact` v7.0.0→v7.0.1, `github/codeql-action` v4.33.0→v4.35.3,
  `codecov/codecov-action` v5.5.2→v6.0.0,
  `softprops/action-gh-release` v2.6.1→v3.0.0
- E2E tests now run against DuckDB v1.5.2 CLI (previously v1.5.1)
- **Transitive crate refresh** via `cargo update` — notably `rand` 0.8.5→0.8.6,
  `rustls-webpki` 0.103.10→0.103.13. `cargo deny check` reports clean
  advisories, bans, licenses, and sources

### Fixed

- **`scripts/setup.sh` was using a wrong `-dv` value for DuckDB metadata**
  — the `C_API_VERSION="v1.2.0"` constant produced an extension that
  DuckDB v1.5.x rejects with "file was built specifically for DuckDB
  version 'v1.2.0'". Replaced with `DUCKDB_RELEASE_VERSION="v1.5.2"`
  matching the Makefile `TARGET_DUCKDB_VERSION` convention, and aligned
  `--abi-type` to `C_STRUCT_UNSTABLE` (matches `make release`)
- **Stale `EXT_VERSION` in `scripts/setup.sh`** — was pinned at `v0.1.0`,
  now tracks the current release version (`v0.5.0`)
- **Stale DuckDB CLI install URL in `scripts/setup.sh`** — was pinned at
  `v1.5.0`, now installs `v1.5.2` to match `TARGET_DUCKDB_VERSION`
- **Dead `as idx_t` round-trip cast** in `src/ffi/sequence_next_node.rs`
  finalize callback (the `idx` was cast to `idx_t` then immediately back
  to `usize` four times)
- **`deny.toml` license allowances** that no longer matched any picked
  license in the dep graph (`BSD-2-Clause`, `CC0-1.0`); `cargo deny check`
  now reports zero `license-not-encountered` warnings

### Documentation

Audited and updated all version/MSRV references across `README.md`,
`CLAUDE.md`, `CONTRIBUTING.md`, `SECURITY.md`, all of `docs/src/**.md`,
all `.github/workflows/*.yml`, `Makefile`, `description.yml`, and the
issue templates so every cross-reference is consistent with the
`v0.5.0` / DuckDB `v1.5.2` / quack-rs `v0.12.0` / MSRV `1.86` baseline.

### Subsumes

This release closes the following dependabot pull requests, which were
all bumping deps to versions at or below those landed here: #58, #61,
#62, #64, #65, #66, #67.

## [0.4.0] - 2026-03-28

### Changed

- **DuckDB v1.5.1 support** — upgraded `libduckdb-sys` from `1.10500.0` to
  `1.10501.0` and `duckdb` (dev) from `1.10500.0` to `1.10501.0`. This
  restores community extension compatibility (previously only supported v1.4.4)
- **quack-rs v0.7.1** — upgraded from v0.6.0. Includes ARM64/aarch64 build fix,
  five new `duckdb-1-5` feature modules (catalog, client_context, config_option,
  copy_function, table_description), new `TypeId` variants, and
  `ScalarFunctionBuilder` enhancements (varargs, volatile, bind, init)
- **`entry_point_v2!` migration** — entry point now uses `quack_rs::entry_point_v2!`
  with `&Connection` / `Registrar` trait instead of raw `duckdb_connection`. All 6
  aggregate functions register via `con.register_aggregate_set(builder)` with proper
  `Result` error propagation. `sessionize` uses `con.as_raw_connection()` for window
  function FFI
- **CI dependency updates** — `Swatinem/rust-cache` v2.8.2→v2.9.1,
  `github/codeql-action` v4.32.6→v4.33.0, `actions/download-artifact`
  v8.0.0→v8.0.1, `taiki-e/install-action` updated,
  `softprops/action-gh-release` v2.5.0→v2.6.1
- **Transitive dependency updates** — `tar` 0.4.44→0.4.45
- E2E tests now run against DuckDB v1.5.1 CLI (previously v1.5.0)

---

## [0.3.0] - 2026-03-28

### Added

- **SQL Cookbook** documentation with 25+ practical recipes organized by function
  category (funnels, sessions, retention, patterns, user flows, combined analysis)
- **Quick Reference** one-page cheat sheet covering all functions, pattern syntax,
  NULL handling, limits, and common translations
- **6 standalone example SQL scripts** in `examples/` directory — self-contained,
  runnable demonstrations of each function category
- **Developer quality check script** (`scripts/check.sh`) — runs all quality checks
  (fmt, clippy, test, doc, bench) with colored output and `--quick` mode
- **Question issue template** (`.github/ISSUE_TEMPLATE/question.md`) for usage
  questions with documentation checklist
- Enhanced mdBook CSS: admonition/callout styling, heading highlight animation,
  smooth scrolling, better link styling, keyboard shortcut styling, horizontal
  rule polish
- 18 new `AggregateTestHarness` unit tests for combine config-propagation
  across all 5 aggregate functions (435 → 453 tests)

### Changed

- **README overhaul**: Added documentation badge, nav bar, "Choosing the Right
  Function" guide, expanded examples section (8 patterns), integrations section
  (Python, Node.js, dbt, Parquet), and verification one-liners in Quick Start
- **CONTRIBUTING.md rewrite**: Added project architecture diagram, quality check
  table, PR checklist, documentation files reference, and `scripts/check.sh`
  instructions
- **mdBook SUMMARY.md**: Added SQL Cookbook and Quick Reference to Getting Started
  section
- Migrated FFI layer to `quack-rs` v0.6.0 SDK for safe state management, vector
  I/O, and function set registration
- Entry point (`src/lib.rs`) now uses `quack_rs::entry_point!` macro instead
  of ~80 lines of hand-rolled unsafe code
- MSRV bumped from 1.80 to 1.84.1 (required by quack-rs)
- FFI modules use `FfiState<T>`, `VectorReader`, `VectorWriter` from quack-rs
- LIST output uses `ListVector` + `VectorWriter` instead of raw pointer arithmetic
- VARCHAR output uses `VectorWriter::write_varchar()` instead of `CString` + raw FFI
- LIST type construction uses `LogicalType::list()` instead of raw `duckdb_create_list_type`
- `retention` registration: ~45 lines of raw loop → 15-line builder chain
- `sequence_match_events` registration: ~55 lines of raw loop → 15-line builder chain
- `sessionize` FFI remains hand-rolled (window function limitation in quack-rs)

### Removed

- Hand-rolled `read_varchar()` helper in `sequence_next_node` (replaced by
  `VectorReader::read_str()`)
- Raw `duckdb_list_vector_*` pointer arithmetic in retention and
  sequence_match_events (replaced by `ListVector` wrappers)
- Raw `duckdb_create_aggregate_function_set` loops in retention and
  sequence_match_events (replaced by `AggregateFunctionSetBuilder`)
- `CString` sanitization in sequence_next_node (replaced by `write_varchar`)

---

## [0.2.0] - 2026-02-15

### Added

- **`sequence_next_node` function** with full direction (forward/backward) and
  base (head/tail/first_match/last_match) support
- **`sequence_match_events` function** returning matched condition timestamps
  as `LIST(TIMESTAMP)`
- **32-condition support** for all variadic functions, matching ClickHouse's
  limit (expanded from 8 to 32)
- **6 combinable `window_funnel` modes**: `strict`, `strict_order`,
  `strict_deduplication`, `strict_increase`, `strict_once`, `allow_reentry`
- **NFA fast-path classification** dispatching common pattern shapes to
  specialized O(n) linear scans (39--61% improvement for `sequence_count`)
- **Presorted detection** skipping O(n log n) sort for timestamp-ordered input
- **27 end-to-end SQL tests** against real DuckDB v1.4.4 CLI
- **26 property-based tests** (proptest) verifying algebraic properties
- GitHub Pages documentation site (mdBook) with function reference, use cases,
  FAQ, architecture, and performance documentation
- CI/CD pipeline with structured test output (`cargo-nextest`), job summaries,
  E2E workflow, SemVer release validation, 4-platform release builds with
  provenance attestation
- Community extension infrastructure (`description.yml`, `Makefile`,
  `extension-ci-tools` submodule)
- Listed in [DuckDB Community Extensions](https://github.com/duckdb/community-extensions/tree/main/extensions/behavioral)
  repository ([PR #1306](https://github.com/duckdb/community-extensions/pull/1306),
  merged 2026-02-15). Install with `INSTALL behavioral FROM community; LOAD behavioral;`
- `'timestamp_dedup'` mode string for the extension-only timestamp-based
  deduplication mode in `window_funnel`
- ClickHouse parity scope table and known semantic differences documentation

### Changed

- **`'strict_deduplication'` mode mapping**: Now correctly maps to `STRICT`
  (0x01), matching ClickHouse where `'strict'` and `'strict_deduplication'`
  are aliases. The timestamp-based dedup behavior is now available under
  `'timestamp_dedup'`.
- **BREAKING**: All public state structs marked `#[non_exhaustive]` to allow
  future field additions without semver-breaking changes. Affected structs:
  `SessionizeState`, `SessionizeBoundaryState`, `RetentionState`,
  `WindowFunnelState`, `SequenceState`, `SequenceNextNodeState`,
  `NextNodeEvent`, `Event`, `CompiledPattern`, `PatternError`, `MatchResult`.
  Use the provided `new()` constructors instead of struct literal syntax.
- **Custom C entry point** (`behavioral_init_c_api`) replaces fragile
  `duckdb::Connection` extraction, using `duckdb_connect` directly from
  `duckdb_extension_access`
- **`Arc<str>` for `Send+Sync` safety** in `sequence_next_node` (replaced
  `Rc<str>`)
- **Defensive FFI boolean reading**: `*const bool` replaced with `*const u8`
  across all FFI modules for ABI safety
- **No-panic FFI entry point**: removed `.unwrap()` on DuckDB function pointers
  in `lib.rs`
- Runtime dependency reduced from 3 crates to 1 (`libduckdb-sys` only)

### Fixed

- SEGFAULT on extension load (incorrect pointer arithmetic in connection
  extraction)
- 6 of 7 functions silently failing to register (missing
  `duckdb_aggregate_function_set_name` call)
- `window_funnel` returning incorrect results (combine not propagating
  `window_size_us` and `mode`)
- `sequence_next_node` NULL output producing `\0\0\0\0` instead of NULL
  (missing `duckdb_vector_ensure_validity_writable` call)
- Interior null byte handling in `sequence_next_node` FFI output
- `retention` finalize calling `ensure_validity_writable` inside loop
  (hoisted outside)

## [0.1.0] - 2026-02-14

### Added

- Initial release with 7 behavioral analytics functions
- `sessionize` -- window function assigning session IDs based on inactivity gaps
- `retention` -- aggregate function for cohort retention analysis
- `window_funnel` -- conversion funnel step tracking within a time window
- `sequence_match` -- NFA-based pattern matching over event sequences
- `sequence_count` -- count non-overlapping pattern occurrences
- `sequence_match_events` -- return timestamps of matched pattern steps
- `sequence_next_node` -- find next/previous event value after pattern match
- Complete ClickHouse behavioral analytics function parity
- 453 unit tests + 1 doc-test
- 27 E2E SQL integration tests
- Criterion.rs benchmarks for all 7 functions (up to 1 billion elements)
- 88.4% mutation testing kill rate (cargo-mutants)
- MIT license

[Unreleased]: https://github.com/tomtom215/duckdb-behavioral/compare/v0.11.0...HEAD
[0.11.0]: https://github.com/tomtom215/duckdb-behavioral/compare/v0.10.1...v0.11.0
[0.10.1]: https://github.com/tomtom215/duckdb-behavioral/compare/v0.10.0...v0.10.1
[0.10.0]: https://github.com/tomtom215/duckdb-behavioral/compare/v0.9.1...v0.10.0
[0.9.1]: https://github.com/tomtom215/duckdb-behavioral/compare/v0.9.0...v0.9.1
[0.9.0]: https://github.com/tomtom215/duckdb-behavioral/compare/v0.8.0...v0.9.0
[0.8.0]: https://github.com/tomtom215/duckdb-behavioral/compare/v0.6.0...v0.8.0
[0.7.0]: https://github.com/tomtom215/duckdb-behavioral/commit/f50cb24
[0.6.0]: https://github.com/tomtom215/duckdb-behavioral/compare/v0.4.0...v0.6.0
[0.5.0]: https://github.com/tomtom215/duckdb-behavioral/commit/b329106
[0.4.0]: https://github.com/tomtom215/duckdb-behavioral/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/tomtom215/duckdb-behavioral/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/tomtom215/duckdb-behavioral/tree/v0.2.0
[0.1.0]: https://github.com/tomtom215/duckdb-behavioral/commits/v0.2.0
