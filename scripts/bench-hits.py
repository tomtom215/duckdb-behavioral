#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)
"""Behavioral-analytics benchmark on the public ClickBench `hits` dataset.

Usage:
    scripts/bench-hits.py fetch --partitions 10
    scripts/bench-hits.py run --duckdb "$(command -v duckdb)" \\
        --extension build/release/behavioral.duckdb_extension --partitions 10
    scripts/bench-hits.py run --partitions 1 --reps 1   # smoke test of the harness

The data is ClickBench's `hits` table (web-analytics events, 100M rows, July
2013) in its 100-file Parquet form, ~1M rows per file. `--partitions N` uses
the first N files, so 1, 10 and 100 are 1M, 10M and 100M rows. No data is
stored in the repository; `benches/hits/manifest.json` pins every file's size
and SHA-256, and `fetch` refuses a file that differs.

What it measures, and why it can be trusted or checked:

  * Every query is fixed in this file (QUERY_SET_VERSION) and was written
    before any timing was taken; the exact SQL of each run is stored in the
    result JSON.
  * Each timing is the wall time of a fresh `duckdb` process (which includes
    opening the Parquet files and loading the extension), `--reps` times, in an
    order that alternates each behavioral query with its pure-SQL baseline.
    Every individual time is kept, not just a summary.
  * `scan` runs the same columns through a plain per-user aggregate: it is the
    floor DuckDB itself costs for reading and grouping, so a behavioral query's
    time can be read as that floor plus the function.
  * `sessionize` and `retention` have exact pure-SQL equivalents (window
    functions, `bool_or`); those run on the same engine and data, and their
    results must equal the extension's, which doubles as a correctness check on
    real data. The other functions have no faithful SQL equivalent and are not
    compared with one.
  * Every query's result must be identical across repetitions and, when
    `--verify-threads` is on (the default up to 10 partitions), equal to the
    1-thread result. A mismatch fails the run (exit status 1), and the JSON
    records it.
  * The result file records the machine (CPU model, cores, memory, load average
    before every query), DuckDB version, the extension's SHA-256 and version,
    the git commit and whether the tree was dirty, and the dataset manifest.

No comparison with another engine is made: a number from a different engine on
different hardware is not evidence about this one.
"""

import argparse
import datetime
import hashlib
import json
import os
import pathlib
import platform
import statistics
import subprocess
import sys
import time

ROOT = pathlib.Path(__file__).resolve().parent.parent
MANIFEST_PATH = ROOT / "benches/hits/manifest.json"
BASE_URL = "https://datasets.clickhouse.com/hits_compatible/athena_partitioned/hits_{i}.parquet"
TOTAL_PARTITIONS = 100
QUERY_SET_VERSION = 1

# The columns used, with the conversions ClickBench's own DuckDB scripts apply:
# EventTime is seconds since the epoch, EventDate days since the epoch.
VIEW_SQL = """CREATE VIEW hits AS SELECT
  UserID,
  epoch_ms(EventTime * 1000) AS ts,
  DATE '1970-01-01' + INTERVAL (EventDate) DAY AS d,
  SearchPhrase <> '' AS searched,
  IsLink <> 0 AS clicked,
  IsDownload <> 0 AS downloaded,
  CounterID
FROM read_parquet({files}, binary_as_string = true);"""

DAYS = [f"DATE '2013-07-{day:02d}'" for day in range(2, 9)]
RETENTION_ARGS = ", ".join(f"d = {day}" for day in DAYS)
RETENTION_SUMS = ", ".join(f"sum(r[{i}]::INT) AS d{i - 1}" for i in range(1, 8))
BASELINE_ANY = ", ".join(f"bool_or(d = {day}) AS c{i}" for i, day in enumerate(DAYS, 1))
BASELINE_SUMS = "sum(c1::INT) AS d0, " + ", ".join(f"sum((c1 AND c{i})::INT) AS d{i - 1}" for i in range(2, 8))

# Each query returns a handful of numbers, so the output can be compared exactly.
# `shape` says how events group: many small groups (one per user) or one group
# holding every row.
QUERIES = [
    {
        "id": "scan",
        "function": None,
        "shape": "per user",
        "title": "Scan control: the same columns, a plain aggregate per user",
        "sql": """SELECT count(*) AS users, sum(n) AS events, sum(s) AS searches, sum(c) AS clicks, sum(dl) AS downloads
FROM (SELECT UserID, count(*) AS n, count_if(searched) AS s, count_if(clicked) AS c, count_if(downloaded) AS dl,
             min(ts) AS first_seen, max(d) AS last_day, max(CounterID) AS site
      FROM hits GROUP BY UserID);""",
    },
    {
        "id": "sessionize",
        "function": "sessionize",
        "shape": "per user",
        "title": "Sessions per user, 30-minute gap (window function)",
        "sql": """SELECT count(*) AS users, sum(sessions) AS sessions FROM (
  SELECT UserID, max(sid) AS sessions FROM (
    SELECT UserID, sessionize(ts, INTERVAL '30 minutes') OVER (PARTITION BY UserID ORDER BY ts) AS sid FROM hits)
  GROUP BY UserID);""",
        "baseline_sql": """SELECT count(*) AS users, sum(sessions) AS sessions FROM (
  SELECT UserID, max(sid) AS sessions FROM (
    SELECT UserID,
           1 + sum(CASE WHEN gap > INTERVAL '30 minutes' THEN 1 ELSE 0 END)
                 OVER (PARTITION BY UserID ORDER BY ts ROWS UNBOUNDED PRECEDING) AS sid
    FROM (SELECT UserID, ts, ts - lag(ts) OVER (PARTITION BY UserID ORDER BY ts) AS gap FROM hits))
  GROUP BY UserID);""",
    },
    {
        "id": "retention",
        "function": "retention",
        "shape": "per user",
        "title": "Seven-day retention from 2013-07-02",
        "sql": f"""SELECT count(*) AS users, {RETENTION_SUMS}
FROM (SELECT retention({RETENTION_ARGS}) AS r FROM hits GROUP BY UserID);""",
        "baseline_sql": f"""SELECT count(*) AS users, {BASELINE_SUMS}
FROM (SELECT {BASELINE_ANY} FROM hits GROUP BY UserID);""",
    },
    {
        "id": "window_funnel",
        "function": "window_funnel",
        "shape": "per user",
        "title": "Funnel search, link click, download within 1 hour",
        "sql": """SELECT level, count(*) AS users FROM (
  SELECT window_funnel(INTERVAL '1 hour', ts, searched, clicked, downloaded) AS level FROM hits GROUP BY UserID)
GROUP BY level ORDER BY level;""",
    },
    {
        "id": "window_funnel_events",
        "function": "window_funnel_events",
        "shape": "per user",
        "title": "The same funnel, returning each step's timestamp",
        "sql": """SELECT count(*) AS users, sum(len(e)) AS steps FROM (
  SELECT window_funnel_events(INTERVAL '1 hour', ts, searched, clicked, downloaded) AS e FROM hits GROUP BY UserID);""",
    },
    {
        "id": "sequence_match",
        "function": "sequence_match",
        "shape": "per user",
        "title": "Search then link click within 5 minutes",
        "sql": """SELECT m, count(*) AS users FROM (
  SELECT sequence_match('(?1)(?t<=300)(?2)', ts, searched, clicked) AS m FROM hits GROUP BY UserID)
GROUP BY m ORDER BY m;""",
    },
    {
        "id": "sequence_count",
        "function": "sequence_count",
        "shape": "per user",
        "title": "Count of search ... link click pairs",
        "sql": """SELECT count(*) AS users, sum(c) AS matches FROM (
  SELECT sequence_count('(?1).*(?2)', ts, searched, clicked) AS c FROM hits GROUP BY UserID);""",
    },
    {
        "id": "sequence_match_events",
        "function": "sequence_match_events",
        "shape": "per user",
        "title": "Timestamps of the search ... link click pair",
        "sql": """SELECT count(*) AS users, sum(len(e)) AS steps FROM (
  SELECT sequence_match_events('(?1).*(?2)', ts, searched, clicked) AS e FROM hits GROUP BY UserID);""",
    },
    {
        "id": "sequence_next_node",
        "function": "sequence_next_node",
        "shape": "per user",
        "title": "Site visited right after the first search",
        "sql": """SELECT count(nxt) AS with_next, count(*) AS users FROM (
  SELECT sequence_next_node('forward', 'first_match', ts, CAST(CounterID AS VARCHAR), searched, searched) AS nxt
  FROM hits GROUP BY UserID);""",
    },
    {
        "id": "window_funnel_one_group",
        "function": "window_funnel",
        "shape": "one group",
        "title": "The funnel over every row as a single group",
        "sql": """SELECT window_funnel(INTERVAL '1 hour', ts, searched, clicked, downloaded) AS level FROM hits;""",
    },
    {
        "id": "sequence_count_one_group",
        "function": "sequence_count",
        "shape": "one group",
        "title": "search ... link click pairs over every row as a single group",
        "sql": """SELECT sequence_count('(?1).*(?2)', ts, searched, clicked) AS matches FROM hits;""",
    },
]


def fail(message: str) -> None:
    sys.exit(f"bench-hits: {message}")


def sha256_file(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for block in iter(lambda: handle.read(1 << 22), b""):
            digest.update(block)
    return digest.hexdigest()


def load_manifest() -> dict:
    if MANIFEST_PATH.exists():
        return json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    return {"source": BASE_URL, "partitions": {}}


def partition_path(data_dir: pathlib.Path, i: int) -> pathlib.Path:
    return data_dir / f"hits_{i}.parquet"


def fetch(args: argparse.Namespace) -> None:
    data_dir = pathlib.Path(args.data_dir)
    data_dir.mkdir(parents=True, exist_ok=True)
    manifest = load_manifest()
    for i in range(args.partitions):
        path = partition_path(data_dir, i)
        pinned = manifest["partitions"].get(str(i))
        if not path.exists():
            subprocess.run(
                ["curl", "-sS", "-f", "-L", "--retry", "3", "-o", f"{path}.part", BASE_URL.format(i=i)],
                check=True,
            )
            os.replace(f"{path}.part", path)
        size, digest = path.stat().st_size, sha256_file(path)
        if pinned is None:
            if not args.record:
                fail(f"hits_{i}.parquet is not pinned in {MANIFEST_PATH.relative_to(ROOT)}; use --record to pin it")
            manifest["partitions"][str(i)] = {"bytes": size, "sha256": digest}
        elif pinned != {"bytes": size, "sha256": digest}:
            fail(f"hits_{i}.parquet differs from the pinned copy (got {size} bytes, sha256 {digest})")
        print(f"hits_{i}.parquet  {size:>11,} bytes  sha256 {digest[:16]}…", flush=True)
    if args.record:
        manifest["partitions"] = dict(sorted(manifest["partitions"].items(), key=lambda kv: int(kv[0])))
        MANIFEST_PATH.parent.mkdir(parents=True, exist_ok=True)
        MANIFEST_PATH.write_text(json.dumps(manifest, indent=1) + "\n", encoding="utf-8")


def cpu_model() -> str:
    try:
        with open("/proc/cpuinfo", encoding="utf-8") as cpuinfo:
            for line in cpuinfo:
                if line.startswith("model name"):
                    return line.split(":", 1)[1].strip()
    except OSError:
        pass
    return platform.processor() or "unknown"


def read_first_line(path: str) -> str | None:
    try:
        with open(path, encoding="utf-8") as handle:
            return handle.readline().strip()
    except OSError:
        return None


def mem_total_gib() -> float | None:
    try:
        with open("/proc/meminfo", encoding="utf-8") as meminfo:
            for line in meminfo:
                if line.startswith("MemTotal"):
                    return round(int(line.split()[1]) / 1024 / 1024, 2)
    except OSError:
        pass
    return None


def git(*args: str) -> str:
    try:
        return subprocess.run(["git", *args], cwd=ROOT, capture_output=True, text=True, check=True).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        return "unknown"


def duckdb_env() -> dict:
    # DuckDB 2.x switches to an agent-oriented output format under these.
    return {k: v for k, v in os.environ.items() if k not in ("AI_AGENT", "CLAUDECODE")}


class Runner:
    def __init__(self, args: argparse.Namespace, files: list[pathlib.Path]):
        self.cli = args.duckdb
        self.extension = str(pathlib.Path(args.extension).resolve())
        self.memory_limit = args.memory_limit
        self.temp_dir = args.temp_dir
        self.view = VIEW_SQL.format(files="[" + ", ".join("'" + str(f) + "'" for f in files) + "]")

    def once(self, sql: str, threads: int) -> tuple[float, str]:
        script = (
            f"SET threads = {threads}; SET memory_limit = '{self.memory_limit}'; "
            f"SET temp_directory = '{self.temp_dir}'; LOAD '{self.extension}'; {self.view} {sql}"
        )
        start = time.perf_counter()
        done = subprocess.run(
            [self.cli, "-unsigned", "-csv", "-noheader", "-c", script],
            capture_output=True, text=True, env=duckdb_env(),
        )
        elapsed = time.perf_counter() - start
        if done.returncode != 0:
            fail(f"duckdb failed ({done.returncode}):\n{done.stderr.strip()}\nquery:\n{sql}")
        return elapsed, done.stdout.strip()


def summarize(times: list[float]) -> dict:
    return {
        "runs": [round(t, 4) for t in times],
        "median_s": round(statistics.median(times), 4),
        "min_s": round(min(times), 4),
        "max_s": round(max(times), 4),
    }


def run(args: argparse.Namespace) -> int:
    data_dir = pathlib.Path(args.data_dir)
    files = [partition_path(data_dir, i) for i in range(args.partitions)]
    manifest = load_manifest()
    for i, path in enumerate(files):
        pinned = manifest["partitions"].get(str(i))
        if not path.exists():
            fail(f"{path} is missing: run `scripts/bench-hits.py fetch --partitions {args.partitions}` first")
        if pinned is not None and path.stat().st_size != pinned["bytes"]:
            fail(f"{path} has {path.stat().st_size} bytes, the manifest pins {pinned['bytes']}")
        if args.check_data and (pinned is None or sha256_file(path) != pinned["sha256"]):
            fail(f"{path} does not match the pinned SHA-256")
    extension = pathlib.Path(args.extension)
    if not extension.exists():
        fail(f"{extension} does not exist: build it with `make configure release`")
    pathlib.Path(args.temp_dir).mkdir(parents=True, exist_ok=True)

    runner = Runner(args, files)
    version = subprocess.run([args.duckdb, "--version"], capture_output=True, text=True, env=duckdb_env()).stdout.strip()
    _, ext_version = runner.once("SELECT behavioral_version();", args.threads)
    _, row_count = runner.once("SELECT count(*) FROM hits;", args.threads)
    rows = int(row_count)
    dirty = bool(git("status", "--porcelain", "--untracked-files=no"))
    environment = {
        "utc": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "cpu": cpu_model(),
        "logical_cpus": os.cpu_count(),
        "memory_gib": mem_total_gib(),
        "cgroup_cpu_max": read_first_line("/sys/fs/cgroup/cpu.max"),
        "kernel": platform.release(),
        "os": platform.platform(),
        "duckdb": version,
        "extension_sha256": sha256_file(extension),
        "extension_bytes": extension.stat().st_size,
        "extension_version": ext_version,
        "git_commit": git("rev-parse", "HEAD"),
        "git_tree_dirty": dirty,
    }
    settings = {
        "partitions": args.partitions,
        "threads": args.threads,
        "reps": args.reps,
        "memory_limit": args.memory_limit,
        "verify_threads": args.verify_threads,
        "query_set_version": QUERY_SET_VERSION,
        "dataset_source": BASE_URL,
        "dataset_bytes": sum(f.stat().st_size for f in files),
        "dataset_sha256_checked": bool(args.check_data),
    }
    print(f"{version}; {environment['cpu']}, {environment['logical_cpus']} logical CPUs; "
          f"extension {ext_version} sha256 {environment['extension_sha256'][:16]}…", flush=True)

    problems: list[str] = []
    results = []
    for query in QUERIES:
        if args.only and query["id"] not in args.only:
            continue
        load = read_first_line("/proc/loadavg")
        own, base, outputs = [], [], set()
        base_outputs = set()
        for rep in range(args.reps):
            elapsed, out = runner.once(query["sql"], args.threads)
            own.append(elapsed)
            outputs.add(out)
            if "baseline_sql" in query:
                elapsed, out = runner.once(query["baseline_sql"], args.threads)
                base.append(elapsed)
                base_outputs.add(out)
        entry = {
            "id": query["id"], "function": query["function"], "shape": query["shape"],
            "title": query["title"], "sql": query["sql"], "load_average_before": load,
            "result": sorted(outputs)[0], "time": summarize(own),
        }
        if len(outputs) != 1:
            problems.append(f"{query['id']}: the result changed between repetitions: {sorted(outputs)}")
        if "baseline_sql" in query:
            entry["baseline_sql"] = query["baseline_sql"]
            entry["baseline_time"] = summarize(base)
            entry["baseline_result"] = sorted(base_outputs)[0]
            entry["matches_baseline"] = len(base_outputs) == 1 and base_outputs == outputs
            if not entry["matches_baseline"]:
                problems.append(f"{query['id']}: result {sorted(outputs)} differs from the pure-SQL baseline {sorted(base_outputs)}")
        if args.verify_threads and args.threads != 1:
            _, single = runner.once(query["sql"], 1)
            entry["matches_single_thread"] = single == entry["result"]
            if not entry["matches_single_thread"]:
                problems.append(f"{query['id']}: {args.threads}-thread result {entry['result']!r} differs from 1-thread {single!r}")
        results.append(entry)
        median = entry["time"]["median_s"]
        extra = f"  pure SQL {entry['baseline_time']['median_s']:.3f} s" if "baseline_time" in entry else ""
        print(f"{query['id']:26s} {median:8.3f} s [{entry['time']['min_s']:.3f}-{entry['time']['max_s']:.3f}]{extra}", flush=True)

    checks = {
        "identical_across_runs": True,
        "pure_sql_baselines_compared": sum("matches_baseline" in q for q in results),
        "single_thread_compared": sum("matches_single_thread" in q for q in results),
        "queries_run": len(results),
    }
    document = {"environment": environment, "settings": settings, "rows": rows, "verified": not problems,
                "checks": checks, "problems": problems, "queries": results}
    out_dir = pathlib.Path(args.out)
    out_dir.mkdir(parents=True, exist_ok=True)
    stamp = environment["utc"].replace(":", "").replace("-", "")
    stem = out_dir / f"hits-{args.partitions}p-{args.threads}t-{stamp}"
    stem.with_suffix(".json").write_text(json.dumps(document, indent=1) + "\n", encoding="utf-8")
    stem.with_suffix(".md").write_text(render(document), encoding="utf-8")
    print(f"wrote {stem.with_suffix('.json')} and .md", flush=True)
    for problem in problems:
        print(f"PROBLEM {problem}", file=sys.stderr)
    return 1 if problems else 0


def render(doc: dict) -> str:
    env, cfg, rows = doc["environment"], doc["settings"], doc["rows"]
    lines = [
        f"# ClickBench `hits` behavioral benchmark: {cfg['partitions']} partition(s), {rows:,} rows",
        "",
        f"- Run at {env['utc']}; query set {cfg['query_set_version']}; "
        f"{cfg['threads']} threads, memory limit {cfg['memory_limit']}, {cfg['reps']} runs per query.",
        f"- {env['cpu']}, {env['logical_cpus']} logical CPUs, {env['memory_gib']} GiB; {env['os']}.",
        f"- {env['duckdb']}; extension {env['extension_version']} (sha256 `{env['extension_sha256'][:16]}…`); "
        f"commit `{env['git_commit'][:10]}`{' (dirty tree)' if env['git_tree_dirty'] else ''}.",
        f"- Checks ({'all passed' if doc['verified'] else '**FAILED**'}): results identical across the runs of "
        f"all {doc['checks']['queries_run']} queries; equal to the pure-SQL baseline for "
        f"{doc['checks']['pure_sql_baselines_compared']}; equal to the 1-thread result for "
        f"{doc['checks']['single_thread_compared']}.",
        "",
        "| Query | Shape | Median s | Min-Max s | First run s | Mrows/s | Pure SQL median s | SQL / ext |",
        "|---|---|---:|---:|---:|---:|---:|---:|",
    ]
    for q in doc["queries"]:
        median = q["time"]["median_s"]
        rate = f"{rows / median / 1e6:.1f}"
        base = q.get("baseline_time", {}).get("median_s")
        lines.append(
            f"| `{q['id']}` | {q['shape']} | {median:.3f} | {q['time']['min_s']:.3f}-{q['time']['max_s']:.3f} | "
            f"{q['time']['runs'][0]:.3f} | {rate} | "
            + (f"{base:.3f} | {base / median:.2f} |" if base else " | |")
        )
    lines += ["", "The SQL of every row is in the JSON file next to this one.", ""]
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    sub = parser.add_subparsers(dest="command", required=True)
    common = argparse.ArgumentParser(add_help=False)
    common.add_argument("--partitions", type=int, default=10, help="first N of 100 files (default 10 = 10M rows)")
    common.add_argument("--data-dir", default=os.environ.get("HITS_DATA_DIR", str(pathlib.Path.home() / ".cache/bench-hits")))
    fetch_parser = sub.add_parser("fetch", parents=[common], help="download and verify the partitions")
    fetch_parser.add_argument("--record", action="store_true", help="pin the sizes and SHA-256 of files not yet in the manifest")
    run_parser = sub.add_parser("run", parents=[common], help="run the queries")
    run_parser.add_argument("--duckdb", default="duckdb")
    run_parser.add_argument("--extension", default=str(ROOT / "build/release/behavioral.duckdb_extension"))
    run_parser.add_argument("--reps", type=int, default=5)
    run_parser.add_argument("--threads", type=int, default=os.cpu_count() or 1)
    run_parser.add_argument("--memory-limit", default="10GB")
    run_parser.add_argument("--temp-dir", default=str(pathlib.Path.home() / ".cache/bench-hits-tmp"))
    run_parser.add_argument("--out", default=str(ROOT / "benches/hits/results"))
    run_parser.add_argument("--only", nargs="*", help="query ids to run (default all)")
    run_parser.add_argument("--check-data", action="store_true", help="re-hash every partition against the manifest")
    run_parser.add_argument("--verify-threads", action=argparse.BooleanOptionalAction, default=None,
                            help="also run each query at 1 thread and compare (default: on up to 10 partitions)")
    args = parser.parse_args()
    if not 1 <= args.partitions <= TOTAL_PARTITIONS:
        fail(f"--partitions must be between 1 and {TOTAL_PARTITIONS}")
    if args.command == "fetch":
        fetch(args)
        return 0
    if args.verify_threads is None:
        args.verify_threads = args.partitions <= 10
    return run(args)


if __name__ == "__main__":
    sys.exit(main())
