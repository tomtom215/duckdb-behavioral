# Benchmark on the ClickBench `hits` data

The [Criterion benchmarks](./performance.md) time the Rust state functions on
synthetic input, and `scripts/bench-sql.sh` times SQL on a synthetic table. This
page is the third kind: the eight functions run as SQL, inside DuckDB, over real
data that anyone can download, with a harness that checks its own answers.

There is no standard benchmark for behavioral-analytics functions.
[ClickBench](https://github.com/ClickHouse/ClickBench) measures analytical SQL on
its `hits` table (100 million web-analytics events from July 2013) and has no
funnel, session or sequence queries; the Billion Row Challenge measured one
group-by aggregate. `scripts/bench-hits.py` takes ClickBench's data and asks
behavioral questions of it. It is **not** a ClickBench submission and makes **no
comparison with another engine**: a number from a different engine on different
hardware is not evidence about this extension.

## Reproduce it

```bash
make configure release                      # the extension, stamped
scripts/bench-hits.py fetch --partitions 10 # 10 of the 100 files, 1.5 GB
scripts/bench-hits.py run --duckdb "$(command -v duckdb)" \
    --extension build/release/behavioral.duckdb_extension --partitions 10
```

`--partitions N` uses the first N of the 100 Parquet files (about 1M rows each),
so 1, 10 and 100 are 1M, 10M and 100M rows (14.7 GB for all of them). The data
stays outside the repository. `benches/hits/manifest.json` pins every file's size
and SHA-256 and `fetch` refuses a file that differs; `run --check-data` re-hashes
them. The result of each run is written to `benches/hits/results/` as JSON (every
individual time, the exact SQL, the machine, the extension's SHA-256, the commit)
and Markdown.

## What is measured, and what keeps it honest

- **The queries were fixed before any timing was taken** (`QUERY_SET_VERSION` in
  the script). They are ordinary questions of web-analytics data: sessions per
  user with a 30-minute gap, seven-day retention, a search, click, download
  funnel within an hour, search followed by a click within five minutes, the site
  visited after a first search, and the same funnel and pair count over all rows
  as one group.
- **Each time is the wall time of a fresh `duckdb` process** (opening the Parquet
  files and loading the extension included), `--reps` times. Every run is kept.
  The first run of a query also pays for filling the operating system's page
  cache and is shown separately.
- **`scan` is the control**: the same columns through a plain aggregate per user.
  It is what DuckDB itself costs for reading and grouping, so a behavioral
  query's time reads as that floor plus the function.
- **Same-engine baselines where an exact equivalent exists.** `sessionize` is also
  written with `lag` and a running sum, and `retention` with `bool_or`, on the
  same data and engine. Their results must equal the extension's. The other
  functions have no faithful pure-SQL equivalent, so none is claimed.
- **Results are checked, not trusted.** A query's result must be identical across
  its repetitions and, when `--verify-threads` is on (the default up to 10
  partitions), equal to its 1-thread result. Any mismatch fails the run with exit
  status 1 and is recorded in the JSON.

The first run of the cross-check found a bug in the benchmark's own baseline, not
in the extension: written with `count_if`, the pure-SQL `sessionize` returned NULL
for the 10,059 single-event users of the first file (`count_if` of a lone NULL gap
is NULL, and `1 + NULL` is NULL), which made its total smaller than the number of
users. It now sums `CASE WHEN gap > ... THEN 1 ELSE 0 END`. A baseline is code
too.

## Results

RESULTS_PLACEHOLDER

## Comparing with ClickHouse or ClickBench

These numbers are **not comparable** with ClickBench's published results or with
ClickHouse, and the harness does not pretend otherwise. Checked against the
ClickBench repository (commit `fc2d3af6`):

| | ClickBench | This benchmark |
|---|---|---|
| Queries | 43 fixed queries, none of them a funnel, session, retention or sequence query | 10 behavioral queries chosen here, plus a scan control |
| Machine | AWS c6a.4xlarge (16 vCPU) for the original results | whatever you run it on; the checked-in runs are a shared 4-vCPU VM |
| Storage | ClickHouse reads a `MergeTree` table ordered by `(CounterID, EventDate, UserID, EventTime, WatchID)` | DuckDB reads the Parquet files |
| Runs | three per query; the first is cold (caches cleared), the summary uses the faster of runs 2 and 3 | five fresh processes; page cache not cleared; the median is reported, the first run shown separately |

A head-to-head that would be fair has to hold everything else equal: the same
machine, the same Parquet files read by both engines (or both loaded into native
tables), the same thread count and cache protocol, each engine's own functions for
the same question (`windowFunnel`, `retention`, `sequenceMatch`, `sequenceCount`,
`sequenceNextNode` in ClickHouse; `sessionize` has no ClickHouse counterpart, so
it would be compared with window-function SQL there), and the per-group results
compared, since the two implementations differ in documented ways
([ClickHouse Compatibility](./clickhouse-compatibility.md)). Even then the
outcome is a same-machine comparison for these queries, not a ClickBench entry.
That arm is not built.

## Limits of these numbers

- **One kind of machine.** A shared 4-vCPU virtual machine, not dedicated
  hardware, so neighbours add noise (the load average before every query is in the
  JSON). Compare only numbers taken on the same machine in the same session;
  rerun on yours with the commands above.
- **The data is what it is.** `EventTime` has one-second resolution, so ties are
  common; users average 5.7 events and a few have thousands; only about 1% are
  active on more than one day, so the retention result is close to degenerate. The
  one-group queries are the opposite extreme and a stress test of the combine
  step, not a typical query.
- **The queries are ours.** They are plausible, not canonical. Changing one bumps
  `QUERY_SET_VERSION`, and results from different versions are not comparable.
- **Correctness is checked here only where the answer can be recomputed** (the
  baselines) or must not depend on threads. Agreement with ClickHouse is checked
  separately, by `scripts/clickhouse-sequence-diff.py` and the differential tests
  described in [ClickHouse Compatibility](./clickhouse-compatibility.md).
