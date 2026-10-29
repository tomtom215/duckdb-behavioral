#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)
# =============================================================================
# bench-sql.sh — end-to-end SQL timings of the eight aggregates.
#
# Usage:
#   scripts/bench-sql.sh <duckdb-cli> <behavioral.duckdb_extension> [reps] [threads]
#
# Builds a deterministic table of 10 million rows in 100,000 groups of 100
# events (timestamps spread over about 66 minutes, with ties; conditions true
# every 3rd, 5th and 7th row; 40 distinct page names), stored in random row
# order, then runs each query `reps` times (default 7) in a fresh process with
# `threads` threads (default 4) and prints the median and range of the wall
# time. The Criterion benchmarks in `benches/` time the Rust state functions
# only; these timings include DuckDB's scan, sort and aggregation.
#
# Compare two builds by running this once per extension on the same machine,
# alternating, and comparing medians against the ranges.
# =============================================================================

set -euo pipefail

if [[ $# -lt 2 ]]; then
    echo "usage: $0 <duckdb-cli> <behavioral.duckdb_extension> [reps] [threads]" >&2
    exit 2
fi
cli="$1"
extension="$(realpath "$2")"
reps="${3:-7}"
threads="${4:-4}"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
db="$work/bench.duckdb"

# AI_AGENT / CLAUDECODE switch the DuckDB CLI into a different output mode.
run() { env -u AI_AGENT -u CLAUDECODE "$cli" -unsigned "$@"; }

run "$db" -c "
CREATE TABLE ev AS
SELECT i % 100000 AS u,
       TIMESTAMP '2024-01-01' + to_seconds((i // 100000) * 40 + (hash(i) % 40)::BIGINT) AS ts,
       i % 3 = 0 AS c1, i % 5 = 0 AS c2, i % 7 = 0 AS c3,
       'page' || (i % 40) AS page
FROM range(10000000) r(i)
ORDER BY hash(i);" > /dev/null

names=(
    "window_funnel, 100k groups"
    "window_funnel_events, 100k groups"
    "sequence_count, 100k groups"
    "sequence_match_events, 100k groups"
    "sequence_next_node, 100k groups"
    "retention, 100k groups"
    "sessionize, OVER (PARTITION BY u ORDER BY ts)"
    "window_funnel, one 10M-row group"
)
queries=(
    "SELECT sum(r) FROM (SELECT window_funnel(INTERVAL '1 hour', ts, c1, c2, c3) AS r FROM ev GROUP BY u)"
    "SELECT sum(len(r)) FROM (SELECT window_funnel_events(INTERVAL '1 hour', ts, c1, c2, c3) AS r FROM ev GROUP BY u)"
    "SELECT sum(r) FROM (SELECT sequence_count('(?1).*(?2).*(?3)', ts, c1, c2, c3) AS r FROM ev GROUP BY u)"
    "SELECT sum(len(r)) FROM (SELECT sequence_match_events('(?1).*(?2)', ts, c1, c2) AS r FROM ev GROUP BY u)"
    "SELECT count(r) FROM (SELECT sequence_next_node('forward', 'first_match', ts, page, c1, c1, c2) AS r FROM ev GROUP BY u)"
    "SELECT sum(len(list_filter(r, lambda x: x))) FROM (SELECT retention(c1, c2, c3) AS r FROM ev GROUP BY u)"
    "SELECT max(s) FROM (SELECT sessionize(ts, INTERVAL '10 minutes') OVER (PARTITION BY u ORDER BY ts) AS s FROM ev)"
    "SELECT window_funnel(INTERVAL '1 hour', ts, c1, c2, c3) FROM ev"
)

printf "%-48s %s\n" "query (wall time in seconds, $threads threads)" "median [min-max] of $reps runs"
for idx in "${!queries[@]}"; do
    times=()
    for ((r = 0; r < reps; r++)); do
        start="$(date +%s.%N)"
        run -readonly "$db" -c "SET threads=$threads; LOAD '$extension'; ${queries[$idx]};" > /dev/null
        end="$(date +%s.%N)"
        times+=("$(awk -v a="$start" -v b="$end" 'BEGIN { printf "%.3f", b - a }')")
    done
    printf '%s\n' "${times[@]}" | sort -n | awk -v name="${names[$idx]}" '
        { v[NR] = $1 }
        END { printf "%-48s %.3f [%.3f-%.3f]\n", name, v[int((NR + 1) / 2)], v[1], v[NR] }'
done
