<p align="center">
  <img src="assets/banner.svg" alt="duckdb-behavioral" width="600">
</p>

<p align="center">
  <strong>Behavioral analytics functions for DuckDB, inspired by ClickHouse.</strong>
</p>

<p align="center">
  <a href="https://github.com/tomtom215/duckdb-behavioral/actions/workflows/ci.yml"><img src="https://github.com/tomtom215/duckdb-behavioral/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-MIT-blue.svg" alt="License: MIT"></a>
  <a href="https://tomtom215.github.io/duckdb-behavioral/"><img src="https://img.shields.io/badge/docs-mdBook-blue.svg" alt="Documentation"></a>
</p>

<p align="center">
  <a href="#quick-start">Quick Start</a> &bull;
  <a href="#functions">Functions</a> &bull;
  <a href="#examples">Examples</a> &bull;
  <a href="#known-limitations">Limitations</a> &bull;
  <a href="#performance">Performance</a> &bull;
  <a href="https://tomtom215.github.io/duckdb-behavioral/">Documentation</a>
</p>

---

Funnels, retention, sessions and "what happens next" over an event table, as
SQL aggregate and window functions in a loadable [DuckDB](https://duckdb.org/)
extension written in Rust. No ClickHouse knowledge is needed. If you do know
its [parametric behavioral functions](https://clickhouse.com/docs/en/sql-reference/aggregate-functions/parametric-functions),
six of the eight functions here follow them and were compared with
`clickhouse local` on random event groups; the
[few deliberate differences](#clickhouse-parity) are listed. `sessionize` and
`window_funnel_events` are additions.

## Quick start

```sql
INSTALL behavioral FROM community;
LOAD behavioral;
```

How far did each user get through view → cart → buy, with 1 hour to finish?

```sql
CREATE TABLE events AS SELECT * FROM (VALUES
  (1, TIMESTAMP '2024-01-01 10:00', 'view'), (1, TIMESTAMP '2024-01-01 10:05', 'cart'),
  (1, TIMESTAMP '2024-01-01 10:20', 'buy'),  (2, TIMESTAMP '2024-01-01 11:00', 'view'),
  (2, TIMESTAMP '2024-01-01 11:10', 'cart'), (3, TIMESTAMP '2024-01-01 12:00', 'view'),
  (3, TIMESTAMP '2024-01-01 15:00', 'cart')
) AS t(user_id, event_time, event_type);

SELECT user_id,
       window_funnel(INTERVAL '1 hour', event_time,
                     event_type = 'view', event_type = 'cart', event_type = 'buy') AS furthest_step
FROM events GROUP BY user_id ORDER BY user_id;
```

```
┌─────────┬───────────────┐
│ user_id │ furthest_step │
│  int32  │     int32     │
├─────────┼───────────────┤
│       1 │             3 │
│       2 │             2 │
│       3 │             1 │
└─────────┴───────────────┘
```

User 3 stops at step 1 because the cart came 3 hours after the view.

The functions sort by timestamp themselves: no `ORDER BY` is needed inside
the call, and `GROUP BY` is the normal way to use all but `sessionize`.

### Installing

| | |
|---|---|
| **Community channel** (`INSTALL behavioral FROM community`) | DuckDB 1.4.4 and later; Linux x86_64 and ARM64, macOS x86_64 and ARM64, Windows x86_64 |
| **Build from source or a release archive** | DuckDB 1.3.2 and later; Linux and macOS. The Linux release archive needs glibc 2.34 or later (Ubuntu 22.04, Debian 12, RHEL 9); the community build needs only 2.28. On an older system, build from source |

One binary serves every DuckDB release: it uses only DuckDB's stable C API
(stamped `C_STRUCT`, C API v1.2.0), and CI loads the same build into DuckDB 1.3.2,
1.4.4, 1.4.5, 1.5.0, 1.5.6 and 2.0.0 and asserts the results. To build from source:

```bash
git submodule update --init --recursive
make configure release
duckdb -unsigned -c "LOAD 'build/release/behavioral.duckdb_extension'; SELECT behavioral_version();"
```

DuckDB loads only `.duckdb_extension` files that carry its metadata footer, which
`make` appends; a plain `cargo build --release` library is refused. Needs Rust 1.88+,
a C compiler and Python 3.

## Functions

| Function | Signature | Returns | Description |
|---|---|---|---|
| `sessionize` | `(TIMESTAMP, INTERVAL)` | `BIGINT` | Which session is each event in? Window function: a new session after an inactivity gap |
| `retention` | `(BOOLEAN, BOOLEAN, ...)` | `BOOLEAN[]` | Who came back? Was condition *i* met, given condition 1? |
| `window_funnel` | `(INTERVAL [, VARCHAR], TIMESTAMP, BOOLEAN, ...)` | `INTEGER` | How far did each user get through ordered steps within a time limit? Furthest step reached, with [6 combinable modes](https://tomtom215.github.io/duckdb-behavioral/functions/window-funnel.html) |
| `window_funnel_events` | `(INTERVAL [, VARCHAR], TIMESTAMP, BOOLEAN, ...)` | `TIMESTAMP[]` | When did each of those steps happen? The best funnel chain's timestamps |
| `sequence_match` | `(VARCHAR, TIMESTAMP, BOOLEAN, ...)` | `BOOLEAN` | Did an [event pattern](https://tomtom215.github.io/duckdb-behavioral/functions/sequence-match.html) occur? |
| `sequence_count` | `(VARCHAR, TIMESTAMP, BOOLEAN, ...)` | `BIGINT` | How many times, without overlap? |
| `sequence_match_events` | `(VARCHAR, TIMESTAMP, BOOLEAN, ...)` | `TIMESTAMP[]` | When did each pattern step happen? |
| `sequence_next_node` | `(VARCHAR, VARCHAR, TIMESTAMP, VARCHAR, BOOLEAN, ...)` | `VARCHAR` | What did the user do right after (or before) a pattern? |

Every `TIMESTAMP` argument also accepts `TIMESTAMPTZ`. `behavioral_version()`
returns the loaded version. `window_funnel` and `window_funnel_events` take 1 to
32 conditions, `retention` and the `sequence_*` pattern functions 2 to 32, and
`sequence_next_node` a base condition plus 1 to 32 event conditions (DuckDB's
own error for a 33rd lists every overload; the limit is 32).

What to expect from all of them:

- **Errors, not silent wrong answers.** Unknown modes, malformed patterns,
  out-of-range condition numbers, month-based or negative intervals, and
  configuration that differs between rows of one group raise descriptive SQL
  errors. A `NULL` timestamp skips the row; a `NULL` condition counts as false.
- **Deterministic under parallelism.** Events sort by total keys, so thread
  count and row order do not change a result. Property tests check every
  aggregate for order, split and combine-tree invariance (`src/oracle_tests.rs`),
  and CI runs a `GROUP BY` at 1 and 4 threads and requires identical rows.
  ClickHouse's own results depend on row order in two cases; see
  [ClickHouse parity](#clickhouse-parity).
- **Exact at the edges.** Gaps touching DuckDB's `±infinity` timestamps are
  computed exactly instead of wrapping.

Reference pages, with edge cases per function:
[Function Reference](https://tomtom215.github.io/duckdb-behavioral/functions/sessionize.html).

## Examples

The examples share one table. The [SQL Cookbook](https://tomtom215.github.io/duckdb-behavioral/cookbook.html)
and [Use Cases](https://tomtom215.github.io/duckdb-behavioral/use-cases.html)
have many more, [`examples/`](examples/) holds runnable scripts, and a
[static tour](https://tomtom215.github.io/duckdb-behavioral/demo.html) applies
the functions to a git history.

```sql
CREATE TABLE events AS SELECT * FROM (VALUES
  (1, TIMESTAMP '2024-01-15 09:00', 'page_view',   'Home'),
  (1, TIMESTAMP '2024-01-15 09:05', 'page_view',   'Product'),
  (1, TIMESTAMP '2024-01-15 09:08', 'add_to_cart', 'Product'),
  (1, TIMESTAMP '2024-01-15 09:12', 'checkout',    'Cart'),
  (1, TIMESTAMP '2024-01-15 09:15', 'purchase',    'Checkout'),
  (2, TIMESTAMP '2024-01-15 10:00', 'page_view',   'Home'),
  (2, TIMESTAMP '2024-01-15 10:10', 'page_view',   'Product'),
  (2, TIMESTAMP '2024-01-15 10:20', 'add_to_cart', 'Product'),
  (2, TIMESTAMP '2024-01-15 14:00', 'page_view',   'Home'),
  (2, TIMESTAMP '2024-01-15 14:05', 'page_view',   'Product'),
  (3, TIMESTAMP '2024-01-15 11:00', 'page_view',   'Home'),
  (3, TIMESTAMP '2024-01-15 11:30', 'page_view',   'Blog'),
  (3, TIMESTAMP '2024-01-15 12:00', 'page_view',   'Home')
) AS t(user_id, event_time, event_type, page);
```

**Funnel** — how far did each user get through four steps, with an hour from
the first? (Count users per step with `GROUP BY step` over this result.)

```sql
SELECT user_id,
       window_funnel(INTERVAL '1 hour', event_time,
                     event_type = 'page_view', event_type = 'add_to_cart',
                     event_type = 'checkout',  event_type = 'purchase') AS furthest_step
FROM events GROUP BY user_id ORDER BY user_id;
```

| user_id | furthest_step |
|---|---|
| 1 | 4 |
| 2 | 2 |
| 3 | 1 |

**Step timestamps** — when was each step reached? This is the best chain: the
latest start that still completes the furthest step, which is why user 1's
chain starts at 09:05 (the second page view), not 09:00.

```sql
SELECT user_id,
       window_funnel_events(INTERVAL '1 hour', event_time,
                     event_type = 'page_view', event_type = 'add_to_cart',
                     event_type = 'checkout',  event_type = 'purchase') AS step_times
FROM events GROUP BY user_id ORDER BY user_id;
```

| user_id | step_times |
|---|---|
| 1 | [2024-01-15 09:05:00, 2024-01-15 09:08:00, 2024-01-15 09:12:00, 2024-01-15 09:15:00] |
| 2 | [2024-01-15 10:10:00, 2024-01-15 10:20:00] |
| 3 | [2024-01-15 12:00:00] |

**Sessions** — a new session after 30 minutes of silence (user 2 shown):

```sql
SELECT user_id, event_time,
       sessionize(event_time, INTERVAL '30 minutes') OVER (
         PARTITION BY user_id ORDER BY event_time) AS session_id
FROM events WHERE user_id = 2 ORDER BY event_time;
```

| user_id | event_time | session_id |
|---|---|---|
| 2 | 2024-01-15 10:00:00 | 1 |
| 2 | 2024-01-15 10:10:00 | 1 |
| 2 | 2024-01-15 10:20:00 | 1 |
| 2 | 2024-01-15 14:00:00 | 2 |
| 2 | 2024-01-15 14:05:00 | 2 |

**Patterns** — did a view lead to a purchase within an hour (thresholds in
`(?t…)` are seconds), and how many times did a view lead to an add-to-cart?

```sql
SELECT user_id,
       sequence_match('(?1).*(?t<=3600)(?2)', event_time,
                      event_type = 'page_view', event_type = 'purchase') AS bought_within_an_hour,
       sequence_count('(?1).*(?2)', event_time,
                      event_type = 'page_view', event_type = 'add_to_cart') AS view_then_cart
FROM events GROUP BY user_id ORDER BY user_id;
```

| user_id | bought_within_an_hour | view_then_cart |
|---|---|---|
| 1 | true | 1 |
| 2 | false | 1 |
| 3 | false | 0 |

**What comes next** — the page after Home → Product, per user (group by user
first, or all users' events form one sequence):

```sql
SELECT user_id,
       sequence_next_node('forward', 'first_match', event_time, page,
                          page = 'Home', page = 'Home', page = 'Product') AS page_after_home_product
FROM events GROUP BY user_id ORDER BY user_id;
```

| user_id | page_after_home_product |
|---|---|
| 1 | Product |
| 2 | Product |
| 3 | NULL |

**Retention** — on which of days 0, 1 and 7 was each user active (a separate
table of activity dates)?

```sql
SELECT user_id,
       retention(d = DATE '2024-01-01', d = DATE '2024-01-02', d = DATE '2024-01-08') AS day0_day1_day7
FROM (VALUES (1, DATE '2024-01-01'), (1, DATE '2024-01-02'), (2, DATE '2024-01-01'),
             (2, DATE '2024-01-08'), (3, DATE '2024-01-02')) t(user_id, d)
GROUP BY user_id ORDER BY user_id;
```

| user_id | day0_day1_day7 |
|---|---|
| 1 | [true, true, false] |
| 2 | [true, false, true] |
| 3 | [false, false, false] |

Pattern syntax: `(?N)` an event satisfying condition *N*; `.` exactly one
event; `.*` any number; `(?t>=S)`, `(?t<=S)`, `(?t>S)`, `(?t<S)`, `(?t==S)` a
time constraint of *S* seconds between the neighbouring steps. Events that
satisfy none of the passed conditions are dropped before matching, so with
conditions for a view and a purchase only, a cart event between them is
invisible and `(?1)(?2)` still matches (add a final always-true condition to
make every row count; see [Which events count](https://tomtom215.github.io/duckdb-behavioral/functions/sequence-match.html#which-events-count)).

From Python, Node.js, dbt, or files:

```python
import duckdb
con = duckdb.connect()
con.execute("INSTALL behavioral FROM community"); con.execute("LOAD behavioral")
con.sql("SELECT user_id, window_funnel(INTERVAL '1 hour', event_time, event_type = 'view', "
        "event_type = 'purchase') FROM read_parquet('events/*.parquet') GROUP BY user_id").show()
```

```javascript
import { DuckDBInstance } from '@duckdb/node-api';   // the maintained Node client (`duckdb` is legacy)
const connection = await (await DuckDBInstance.create(':memory:')).connect();
await connection.run('INSTALL behavioral FROM community');
await connection.run('LOAD behavioral');
```

```yaml
# dbt-duckdb profiles.yml
extensions:
  - name: behavioral
    repo: community
```

## Why not plain SQL?

Sessions are easy in plain SQL: `lag()` and a running `sum()` give the same
answer as `sessionize`. Funnels are where hand-written SQL goes wrong. The usual
query takes each step's first timestamp and compares them, which scores a user
who viewed at 09:00 and again at 10:30, added to cart at 10:40 and bought at
10:50 as 1: the 09:00 view is too early and nothing tries the second one.
`window_funnel(INTERVAL '1 hour', …)` tries every start and returns 3. Time
limits between steps, repeated steps and "what happened next" take self-joins or
recursive queries in SQL; here each is one aggregate call whose result does not
depend on thread count or row order.

## Known limitations

- **`TIMESTAMPTZ_NS` columns need a cast.** DuckDB 2.0.0 added the type and gave
  it no implicit cast to `TIMESTAMP` or `TIMESTAMPTZ` (`TIMESTAMP_NS` has one), so
  passing it to any function here is a binder error. Write
  `ts::TIMESTAMPTZ` (rounded to the nearest microsecond; checked on 2.0.0).
- **Crashes in DuckDB's C API aggregate path before 2.0.** Some query shapes
  make DuckDB read out of bounds for *every* C API aggregate, not just this
  extension ([duckdb/duckdb#26109](https://github.com/duckdb/duckdb/issues/26109),
  fixed by [#26455](https://github.com/duckdb/duckdb/pull/26455) on the 2.0 line
  only): `ORDER BY` inside the call, `OVER ()`, a frame written `BETWEEN
  UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING`, and, on 1.3.2 through 1.4.5,
  `OVER (PARTITION BY g)` with no `ORDER BY`. **The community channel builds
  this extension for DuckDB 1.4.5 and 1.5.6 as well as 2.0.0, so on those two
  releases these crashes are yours.** Which releases crash was tested per shape:

  | Query shape | 1.3.2 | 1.4.4 | 1.4.5 | 1.5.0 | 1.5.6 | 2.0.0 |
  |---|---|---|---|---|---|---|
  | `f(.. ORDER BY ts)`, `OVER ()`, `UNBOUNDED..UNBOUNDED` | crash | crash | crash | crash | crash | works |
  | `OVER (PARTITION BY g)` or `OVER (PARTITION BY 1)` (no `ORDER BY`) | **crash** | **crash** | **crash** | works | works | works |
  | `OVER (PARTITION BY g ORDER BY ts)` with any bounded frame | works | works | works | works | works | works |

  Run for `window_funnel` on every release shown (`scripts/crash-shapes.sh`
  prints this table); the first two rows were also run for the other seven
  functions on 1.3.2 and 2.0.0, and on 2.0.0 CI asserts all eight functions on
  these shapes. None of these shapes is needed: the functions sort by timestamp
  themselves, so on DuckDB older than 2.0 use `GROUP BY` (and a join to put the
  result back on each row) or a bounded frame. Details:
  [FAQ](https://tomtom215.github.io/duckdb-behavioral/faq.html#which-query-shapes-crash-duckdb).
- **`GROUP BY … LIMIT n` without `ORDER BY` leaks memory.** When DuckDB stops a
  grouped scan early (`LIMIT`, an interrupt) it never destroys the aggregate
  states it did not reach, so each unreached group's collected events stay
  allocated until the process exits. Measured: ~77 MB lost per query for
  300,000 groups of 10 events (`window_funnel_events`, `sequence_count`), 136 MB
  for `sequence_next_node`. Put `ORDER BY` before `LIMIT`, or materialise the
  aggregate first; a long-lived process that runs such queries repeatedly will
  grow. This is DuckDB's behaviour for every aggregate that owns heap memory.
- **`sessionize` needs an ordered window.** Use `OVER (… ORDER BY ts)` with an
  ascending order: a descending order, or another column, is an error
  (`rows must arrive in ascending timestamp order`), and as a plain aggregate
  (no `OVER`) a run is correct or that error, depending on thread scheduling.
- **Running window frames are quadratic.** In `OVER (ORDER BY ts)` every row is a
  separate aggregate over all earlier rows, and DuckDB holds about 2,048 such
  frames at once: one 40,000-row partition took 11–12 s and 1.2 GB for
  `window_funnel` (DuckDB 1.5.6 and 2.0.0, one thread). Partition by user, bound
  the frame (`ROWS 1000 PRECEDING`), or use `GROUP BY`. `sessionize` is the
  exception; its state is constant-size.
- **No cancellation within one group.** DuckDB's C API gives an aggregate no way
  to observe an interrupt, so Ctrl-C takes effect once the group being finalized
  is done: 0.8–1.3 s for 10 million events and a 3- to 5-step `sequence_*`
  pattern, longer for very long patterns (capped at 1,024 steps).
- **Memory outside `memory_limit`.** Collected events (16 bytes each; 24 plus the
  value bytes for `sequence_next_node`) live on the Rust heap, which DuckDB does
  not account for or spill. An allocation that fails raises an `out of memory`
  error instead of crashing.

## Performance

Measured in [`PERF.md`](PERF.md) Session 22 on a shared 4-vCPU Intel Xeon
@ 2.10 GHz, Linux, Rust 1.97.0, DuckDB 2.0.0, release build.

[`scripts/bench-sql.sh`](scripts/bench-sql.sh) builds 10 million rows in 100,000
groups of 100 events and runs each query in a fresh `duckdb` process with 4
threads (wall time in seconds, median [min-max] of 9 runs, 25 for `sessionize`;
includes DuckDB's scan and grouping):

| Query | Time |
|---|---|
| `window_funnel`, 3 steps | 1.07 [0.99-1.41] |
| `window_funnel_events` | 1.02 [0.96-1.09] |
| `sequence_count('(?1).*(?2).*(?3)')` | 1.20 [1.06-1.47] |
| `sequence_match_events('(?1).*(?2)')` | 1.10 [1.06-1.21] |
| `sequence_next_node` | 2.60 [2.43-2.93] |
| `retention`, 3 conditions | 0.26 [0.24-0.29] |
| `sessionize` over `PARTITION BY u ORDER BY ts` | 1.42 [1.31-1.58] |
| `window_funnel` over one 10-million-row group | 0.41 [0.39-0.48] |

Run alternately with the build from before this release's changes on the same
machine, no query differed beyond the spread of repeated runs (ratios 0.93 to
1.07; `PERF.md` has both builds side by side). Compare only with numbers taken
on one machine: a different machine gave faster times for the same queries
despite a slower CPU. The Criterion microbenchmarks of the Rust state functions
(a billion `sessionize` updates, 100 million events for the others, with their
method and history) are in [`PERF.md`](PERF.md). Reproduce with `cargo bench`
and `scripts/bench-sql.sh <duckdb> <extension>`.

## ClickHouse parity

All six ClickHouse behavioral functions are implemented. Each was compared with
`clickhouse local` 26.10.2.103 (`window_funnel` also on 26.9.8.3, with identical
results) on random event groups: ties, events matching several conditions,
every mode combination, random patterns. For the `sequence_*` functions a small
model of each engine predicts every one of 57.6 million results (two seeds)
([`scripts/clickhouse-sequence-diff.py`](scripts/clickhouse-sequence-diff.py)
reruns it). Counts and the remaining differences are in
[ClickHouse Compatibility](https://tomtom215.github.io/duckdb-behavioral/internals/clickhouse-compatibility.html).

| Function | Result |
|---|---|
| `retention` | Identical in 200,000 groups |
| `window_funnel` | Identical for 15 of the 24 mode combinations in every variant; the 6 that use `strict_increase` without `strict_once` differ by design, and the 3 that pair `strict_deduplication` with `strict_once` differ only when ties arrive in a different order than the extension's sort |
| `sequence_match`, `sequence_count` | Identical unless a time constraint directly follows `.*` (ClickHouse measures it from a different event) or tied rows reach ClickHouse in another order than the extension's |
| `sequence_match_events` | As above, except that when an earlier attempt matched every step and then failed a later time constraint, ClickHouse reports that attempt and the extension reports the chain that matched (5,174 of 600,000 results for patterns with no constraint after `.*`, distinct timestamps) |
| `sequence_next_node` | Identical with distinct (timestamp, value) per event; with ties, identical whenever ClickHouse receives rows in the extension's tie order |

Deliberate differences, each because ClickHouse is defective or depends on row order:

- `window_funnel` with `strict_increase` (without `strict_once`): ClickHouse keeps
  one chain per level and loses a valid one (`c1@0, c1@1, c2@1` gives 1; the
  extension gives 2).
- `window_funnel` with `strict_deduplication` + `strict_once`, and
  `sequence_next_node` with rows tying on timestamp and value: ClickHouse's answer
  changes with row order; the extension orders ties deterministically.
- `sequence_*` time constraints after `.*` are measured from the last matched
  event; ClickHouse measures from the event after it. A constraint before any
  `(?N)` or `.` is an error here.

Also: NULL conditions count as false (ClickHouse skips the row), timestamps are
microseconds with `(?t)` thresholds in seconds, windows are `INTERVAL`s, and
`retention` over zero rows returns `[]`. The complete list is in
[ClickHouse Compatibility](https://tomtom215.github.io/duckdb-behavioral/internals/clickhouse-compatibility.html).

## Quality

| | |
|---|---|
| Unit tests | 544, plus 1 doc-test, including 39 property tests (proptest). The `window_funnel` engine is compared with an exhaustive reference and every state with order- and combine-tree-invariance checks (`src/oracle_tests.rs`) |
| Integration tests | 34, in process: the release library is loaded into a real DuckDB and every function runs through SQL, including error paths, `±infinity` timestamps, and parallel determinism |
| End-to-end | `make test_release` (8 SQL logic files in `test/sql/`) and 21 asserted queries (`scripts/compat-check.sh`; 25 on DuckDB 2.x, where the crash shapes above also run) on Linux and macOS, then the same build in DuckDB 1.3.2, 1.4.4, 1.4.5, 1.5.0 and 1.5.6 |
| Differential | Against ClickHouse, see above |
| Mutation testing | cargo-mutants: 93.9% of viable mutants detected across the 9 non-FFI modules (0.10.0 audit, before the test changes in this release) |
| Lints | Clippy pedantic + nursery + cargo groups with `-D warnings`; `unsafe_code` denied outside `src/ffi`, every `unsafe` block one operation with a `// SAFETY:` note (lint-enforced) |
| CI | Format, clippy, tests on 2 OSes, docs, MSRV 1.88, compile checks for Windows, macOS x86_64, Linux ARM64 and WASM, `cargo deny`, workflow and shell lint, a documentation-consistency check, coverage; weekly toolchain, dependency, DuckDB-nightly and fuzzing canaries |
| Fuzzing | cargo-fuzz targets for the parser, the sequence matcher and the funnel (`fuzz/`), run weekly |
| Releases | 4 platforms, SHA-256 checksums and build-provenance attestations; a stable release must be a CI-green commit of `main` |

## Building and contributing

```bash
cargo test --lib                    # unit tests: no libduckdb, no C++ build
DUCKDB_DOWNLOAD_LIB=1 cargo test --all-features   # + loads the build into a real DuckDB
./scripts/check.sh                  # everything CI runs that works offline
make configure release test_release # the extension, as the community CI builds it
```

[`CONTRIBUTING.md`](CONTRIBUTING.md) has the workflow, [`RELEASING.md`](RELEASING.md)
the release steps, [`PERF.md`](PERF.md) the benchmark protocol and history, and
[`CHANGELOG.md`](CHANGELOG.md) the release notes. This project follows
[Semantic Versioning](https://semver.org/); the
[versioning policy](https://tomtom215.github.io/duckdb-behavioral/operations/security.html#versioning)
applies it to SQL signatures.

## Documentation

[Getting Started](https://tomtom215.github.io/duckdb-behavioral/getting-started.html) ·
[Function Reference](https://tomtom215.github.io/duckdb-behavioral/functions/sessionize.html) ·
[Use Cases](https://tomtom215.github.io/duckdb-behavioral/use-cases.html) ·
[SQL Cookbook](https://tomtom215.github.io/duckdb-behavioral/cookbook.html) ·
[Quick Reference](https://tomtom215.github.io/duckdb-behavioral/quick-reference.html) ·
[Engineering Overview](https://tomtom215.github.io/duckdb-behavioral/engineering.html) ·
[Performance](https://tomtom215.github.io/duckdb-behavioral/internals/performance.html) ·
[ClickHouse Compatibility](https://tomtom215.github.io/duckdb-behavioral/internals/clickhouse-compatibility.html)

## Project status

> **Personal project.** This is a personal project developed on my own time. It
> is not affiliated with, endorsed by, or related to my employer or
> professional role in any way.
>
> **AI-assisted development.** Built with Claude (Anthropic). Correctness is
> validated by automated testing — not assumed from AI output. See
> [Quality](#quality).

## License

MIT
