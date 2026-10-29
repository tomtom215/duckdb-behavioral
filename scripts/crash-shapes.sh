#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)
# =============================================================================
# crash-shapes.sh — print which query shapes crash which DuckDB releases.
#
# Usage:
#   scripts/crash-shapes.sh <behavioral.duckdb_extension> <duckdb-cli>...
#
# DuckDB releases before 2.0.0 hand every C API aggregate a one-element state
# array with a row count above one for a few query shapes
# (duckdb/duckdb#26109), and the process usually segfaults. This runs each
# shape against `window_funnel` in every CLI you pass and prints ok, CRASH
# (killed by SIGSEGV) or the exit status. It regenerates the table in the
# README's "Known limitations" section.
# =============================================================================

set -uo pipefail

if [[ $# -lt 2 ]]; then
    echo "usage: $0 <behavioral.duckdb_extension> <duckdb-cli>..." >&2
    exit 2
fi
extension="$1"
shift

setup="CREATE TABLE t AS SELECT i % 2 AS g, TIMESTAMP '2024-01-01' + to_seconds(i) AS ts,
  i % 2 = 0 AS a, i % 3 = 0 AS b FROM range(10) r(i);"

shapes=(
    "ORDER BY inside the call|SELECT window_funnel(INTERVAL 1 HOUR, ts, a, b ORDER BY ts) FROM t GROUP BY g"
    "OVER ()|SELECT count(r) FROM (SELECT window_funnel(INTERVAL 1 HOUR, ts, a, b) OVER () AS r FROM t)"
    "UNBOUNDED PRECEDING..UNBOUNDED FOLLOWING|SELECT count(r) FROM (SELECT window_funnel(INTERVAL 1 HOUR, ts, a, b) OVER (ORDER BY ts ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING) AS r FROM t)"
    "OVER (PARTITION BY g)|SELECT count(r) FROM (SELECT window_funnel(INTERVAL 1 HOUR, ts, a, b) OVER (PARTITION BY g) AS r FROM t)"
    "OVER (PARTITION BY 1)|SELECT count(r) FROM (SELECT window_funnel(INTERVAL 1 HOUR, ts, a, b) OVER (PARTITION BY 1) AS r FROM t)"
    "PARTITION BY g ORDER BY ts, bounded frame|SELECT count(r) FROM (SELECT window_funnel(INTERVAL 1 HOUR, ts, a, b) OVER (PARTITION BY g ORDER BY ts ROWS BETWEEN 2 PRECEDING AND CURRENT ROW) AS r FROM t)"
    "PARTITION BY g ORDER BY ts, running frame|SELECT count(r) FROM (SELECT window_funnel(INTERVAL 1 HOUR, ts, a, b) OVER (PARTITION BY g ORDER BY ts) AS r FROM t)"
)

printf '%-46s' "query shape"
for cli in "$@"; do
    printf '%-10s' "$(env -u AI_AGENT -u CLAUDECODE "$cli" --version | awk '{print $1}')"
done
echo

for entry in "${shapes[@]}"; do
    printf '%-46s' "${entry%%|*}"
    for cli in "$@"; do
        # The group's redirect also silences the shell's own "Segmentation
        # fault" notice, which would otherwise interleave with the table.
        {
            env -u AI_AGENT -u CLAUDECODE "$cli" -unsigned -csv -noheader \
                -c "LOAD '$extension'; $setup ${entry#*|};" > /dev/null 2>&1
        } 2> /dev/null
        status=$?
        case $status in
            0) printf '%-10s' ok ;;
            139) printf '%-10s' CRASH ;;
            *) printf '%-10s' "exit $status" ;;
        esac
    done
    echo
done
