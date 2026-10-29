#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)
# =============================================================================
# compat-check.sh — load one built extension into one DuckDB CLI and assert
# query results.
#
# Usage:
#   scripts/compat-check.sh <duckdb-cli> <behavioral.duckdb_extension>
#
# The extension is stamped for the stable C API, so one binary must load and
# answer identically in every DuckDB release with that C API. CI runs this
# script against each release in the e2e.yml `compat` matrix; run it by hand
# against any other CLI:
#
#   make configure release
#   scripts/compat-check.sh "$(command -v duckdb)" build/release/behavioral.duckdb_extension
#
# Exit status is non-zero if any check fails. Every expected value below was
# derived by hand from the function's documented semantics, not copied from
# the first output.
# =============================================================================

set -euo pipefail

if [[ $# -ne 2 ]]; then
    echo "usage: $0 <duckdb-cli> <behavioral.duckdb_extension>" >&2
    exit 2
fi
duckdb_cli="$1"
extension="$2"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cargo_version="$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)"

# DuckDB 2.x switches to an agent-oriented output format when `AI_AGENT` or
# `CLAUDECODE` is set; the checks below compare plain CSV.
duckdb() { env -u AI_AGENT -u CLAUDECODE "$duckdb_cli" "$@"; }

run() {
    duckdb -unsigned -csv -noheader -c "LOAD '$extension'; $1" 2>&1
}

failures=0

# check <name> <expected output> <sql>
check() {
    local name="$1" expected="$2" sql="$3" actual
    if ! actual="$(run "$sql")"; then
        printf 'FAIL %s\n  query failed: %s\n' "$name" "$(head -c 600 <<<"$actual")"
        failures=$((failures + 1))
    elif [[ "$actual" != "$expected" ]]; then
        printf 'FAIL %s\n  expected: %s\n  actual:   %s\n' "$name" "$expected" "$(head -c 600 <<<"$actual")"
        failures=$((failures + 1))
    else
        printf 'ok   %s\n' "$name"
    fi
}

# check_error <name> <substring the error must contain> <sql>
check_error() {
    local name="$1" needle="$2" sql="$3" actual
    if actual="$(run "$sql")"; then
        printf 'FAIL %s\n  expected an error containing "%s", got: %s\n' "$name" "$needle" "$actual"
        failures=$((failures + 1))
    elif [[ "$actual" != *"$needle"* ]]; then
        printf 'FAIL %s\n  error does not contain "%s": %s\n' "$name" "$needle" "$(head -c 600 <<<"$actual")"
        failures=$((failures + 1))
    else
        printf 'ok   %s\n' "$name"
    fi
}

# Three events ten minutes apart; the label is the only condition source.
events="(VALUES (TIMESTAMP '2024-01-01 00:00', 'a'),
                 (TIMESTAMP '2024-01-01 00:10', 'b'),
                 (TIMESTAMP '2024-01-01 00:20', 'c')) t(ts, e)"

check version "$cargo_version" "SELECT behavioral_version()"

# 8 aggregates and the version scalar register 540 overloads in all
# (CLAUDE.md, "Function sets for variadic signatures").
check overload-count 540 "SELECT count(*) FROM duckdb_functions()
  WHERE function_name IN ('sessionize', 'retention', 'window_funnel',
    'window_funnel_events', 'sequence_match', 'sequence_count',
    'sequence_match_events', 'sequence_next_node', 'behavioral_version')"

check sessionize '"[1, 2, 3]"' "SELECT list(s ORDER BY ts) FROM (
  SELECT ts, sessionize(ts, INTERVAL '5 minutes') OVER (ORDER BY ts) AS s FROM $events)"
check sessionize-one-session '"[1, 1, 1]"' "SELECT list(s ORDER BY ts) FROM (
  SELECT ts, sessionize(ts, INTERVAL '15 minutes') OVER (ORDER BY ts) AS s FROM $events)"

check retention '"[true, true, true]"' \
    "SELECT retention(e = 'a', e = 'b', e = 'c') FROM $events"

check window_funnel 3 \
    "SELECT window_funnel(INTERVAL '1 hour', ts, e = 'a', e = 'b', e = 'c') FROM $events"
# 15-minute window: 'c' is 20 minutes after 'a'.
check window_funnel-short-window 2 \
    "SELECT window_funnel(INTERVAL '15 minutes', ts, e = 'a', e = 'b', e = 'c') FROM $events"
check window_funnel-timestamptz 3 \
    "SELECT window_funnel(INTERVAL '1 hour', ts::TIMESTAMPTZ, e = 'a', e = 'b', e = 'c') FROM $events"
check window_funnel-mode 3 \
    "SELECT window_funnel(INTERVAL '1 hour', 'strict_order, strict_increase', ts,
       e = 'a', e = 'b', e = 'c') FROM $events"
check window_funnel_events \
    "\"['2024-01-01 00:00:00', '2024-01-01 00:10:00', '2024-01-01 00:20:00']\"" \
    "SELECT window_funnel_events(INTERVAL '1 hour', ts, e = 'a', e = 'b', e = 'c') FROM $events"

check sequence_match true \
    "SELECT sequence_match('(?1).*(?2)', ts, e = 'a', e = 'c') FROM $events"
# Events with no true condition are dropped before matching (as in
# ClickHouse), so without a condition for 'b' the events 'a' and 'c' are
# adjacent; with one, 'b' sits between them and (?1)(?2) cannot match.
check sequence_match-dropped-events true \
    "SELECT sequence_match('(?1)(?2)', ts, e = 'a', e = 'c') FROM $events"
check sequence_match-adjacent false \
    "SELECT sequence_match('(?1)(?2)', ts, e = 'a', e = 'c', e = 'b') FROM $events"
check sequence_count 1 \
    "SELECT sequence_count('(?1).*(?2)', ts, e = 'a', e = 'c') FROM $events"
check sequence_match_events "\"['2024-01-01 00:00:00', '2024-01-01 00:10:00']\"" \
    "SELECT sequence_match_events('(?1)(?2)', ts, e = 'a', e = 'b') FROM $events"
check sequence_next_node b \
    "SELECT sequence_next_node('forward', 'head', ts, e, e = 'a', e = 'a') FROM $events"

# A NULL timestamp row is skipped; the other two events still make a funnel.
check null-timestamp 2 "SELECT window_funnel(INTERVAL '1 hour', ts, e = 'a', e = 'b') FROM
  (VALUES (TIMESTAMP '2024-01-01 00:00', 'a'), (NULL::TIMESTAMP, 'b'),
          (TIMESTAMP '2024-01-01 00:10', 'b')) t(ts, e)"

check_error invalid-mode "unknown mode" \
    "SELECT window_funnel(INTERVAL '1 hour', 'bogus', ts, e = 'a') FROM $events"
check_error month-interval "month-based" \
    "SELECT window_funnel(INTERVAL '1 month', ts, e = 'a') FROM $events"
check_error malformed-pattern "pattern" \
    "SELECT sequence_match('(?1', ts, e = 'a', e = 'b') FROM $events"

# The same GROUP BY on one thread and on four must agree row for row
# (parallel partial states combine in a different order on each run).
check thread-determinism '997,0,0' "
  CREATE TABLE big AS SELECT i % 997 AS g,
      TIMESTAMP '2024-01-01' + to_seconds((i * 7919) % 100003) AS ts,
      i % 3 = 0 AS c1, i % 5 = 0 AS c2, i % 7 = 0 AS c3
    FROM range(300000) t(i);
  SET threads = 1;
  CREATE TABLE r1 AS SELECT g, window_funnel(INTERVAL '2 hours', ts, c1, c2, c3) AS f,
      sequence_count('(?1).*(?2).*(?3)', ts, c1, c2, c3) AS n,
      sequence_match_events('(?1).*(?2)', ts, c1, c2) AS m,
      retention(c1, c2, c3) AS r
    FROM big GROUP BY g;
  SET threads = 4;
  CREATE TABLE r4 AS SELECT g, window_funnel(INTERVAL '2 hours', ts, c1, c2, c3) AS f,
      sequence_count('(?1).*(?2).*(?3)', ts, c1, c2, c3) AS n,
      sequence_match_events('(?1).*(?2)', ts, c1, c2) AS m,
      retention(c1, c2, c3) AS r
    FROM big GROUP BY g;
  SELECT (SELECT count(*) FROM r1),
         (SELECT count(*) FROM (SELECT * FROM r1 EXCEPT SELECT * FROM r4)),
         (SELECT count(*) FROM (SELECT * FROM r4 EXCEPT SELECT * FROM r1))"

# Query shapes that crash every C API aggregate before DuckDB 2.0.0
# (duckdb/duckdb#26109; fixed on the 2.0 line by duckdb/duckdb#26455, not on the
# 1.4 and 1.5 branches). On those releases the shapes kill the process, so they
# run only where the fix is present.
duckdb_version="$(duckdb --version 2>&1 | head -1)"
if [[ "$duckdb_version" =~ ^v([0-9]+)\. ]] && ((BASH_REMATCH[1] >= 2)); then
    funnel="window_funnel(INTERVAL '1 hour', ts, e = 'a', e = 'b', e = 'c'"
    check shape-order-by-in-call 3 "SELECT $funnel ORDER BY ts) FROM $events"
    check shape-over-empty '"[3, 3, 3]"' \
        "SELECT list(r) FROM (SELECT $funnel) OVER () AS r FROM $events)"
    check shape-unbounded-frame '"[3, 3, 3]"' \
        "SELECT list(r) FROM (SELECT $funnel) OVER (ORDER BY ts ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING) AS r FROM $events)"
    check shape-partition-without-order '"[3, 3, 3]"' \
        "SELECT list(r) FROM (SELECT $funnel) OVER (PARTITION BY 1) AS r FROM $events)"
fi

if [[ $failures -ne 0 ]]; then
    printf '\n%d check(s) failed against %s\n' "$failures" "$duckdb_version"
    exit 1
fi
printf '\nall checks passed against %s\n' "$duckdb_version"
