#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)
"""Load a built extension into the DuckDB Python package and check results.

Usage:
    python scripts/nightly-smoke.py build/release/behavioral.duckdb_extension

The scheduled workflow runs this against `pip install --pre duckdb` (the next
DuckDB release in preparation), so a change in DuckDB's C API or aggregate
engine that breaks the stamped binary shows up before that release ships.
scripts/compat-check.sh runs the fuller check against released CLIs.
"""

import sys

import duckdb

EVENTS = """(VALUES (TIMESTAMP '2024-01-01 00:00', 'a'),
                   (TIMESTAMP '2024-01-01 00:10', 'b'),
                   (TIMESTAMP '2024-01-01 00:20', 'c')) t(ts, e)"""

# (name, sql, expected single value)
CHECKS = [
    (
        "window_funnel",
        f"SELECT window_funnel(INTERVAL '1 hour', ts, e = 'a', e = 'b', e = 'c') FROM {EVENTS}",
        3,
    ),
    (
        "window_funnel-short-window",
        f"SELECT window_funnel(INTERVAL '15 minutes', ts, e = 'a', e = 'b', e = 'c') FROM {EVENTS}",
        2,
    ),
    (
        "sequence_match",
        f"SELECT sequence_match('(?1).*(?2)', ts, e = 'a', e = 'c') FROM {EVENTS}",
        True,
    ),
    (
        "sequence_count",
        f"SELECT sequence_count('(?1).*(?2)', ts, e = 'a', e = 'c') FROM {EVENTS}",
        1,
    ),
    (
        "retention",
        f"SELECT retention(e = 'a', e = 'b', e = 'c') FROM {EVENTS}",
        [True, True, True],
    ),
    (
        "sequence_next_node",
        f"SELECT sequence_next_node('forward', 'head', ts, e, e = 'a', e = 'a') FROM {EVENTS}",
        "b",
    ),
    (
        "sessionize",
        f"""SELECT list(s ORDER BY ts) FROM (
              SELECT ts, sessionize(ts, INTERVAL '5 minutes') OVER (ORDER BY ts) AS s
              FROM {EVENTS})""",
        [1, 2, 3],
    ),
]


# Query shapes that crashed every C API aggregate before DuckDB 2.0.0
# (duckdb/duckdb#26109, fixed by duckdb/duckdb#26455). The nightly is 2.x, so
# a regression of the fix would kill this process rather than fail a check.
FUNNEL = "window_funnel(INTERVAL '1 hour', ts, e = 'a', e = 'b', e = 'c'"
CRASH_SHAPES = [
    ("shape-order-by-in-call", f"SELECT {FUNNEL} ORDER BY ts) FROM {EVENTS}", 3),
    (
        "shape-over-empty",
        f"SELECT list(r) FROM (SELECT {FUNNEL}) OVER () AS r FROM {EVENTS})",
        [3, 3, 3],
    ),
    (
        "shape-unbounded-frame",
        f"""SELECT list(r) FROM (SELECT {FUNNEL}) OVER (ORDER BY ts
              ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING) AS r
            FROM {EVENTS})""",
        [3, 3, 3],
    ),
    (
        "shape-partition-without-order",
        f"SELECT list(r) FROM (SELECT {FUNNEL}) OVER (PARTITION BY 1) AS r FROM {EVENTS})",
        [3, 3, 3],
    ),
]


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__)
        return 2
    extension = sys.argv[1]
    print(f"DuckDB {duckdb.__version__}")
    checks = list(CHECKS)
    if int(duckdb.__version__.split(".")[0]) >= 2:
        checks += CRASH_SHAPES
    con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
    con.execute(f"LOAD '{extension}'")
    failures = 0
    for name, sql, expected in checks:
        actual = con.execute(sql).fetchone()[0]
        status = "ok  " if actual == expected else "FAIL"
        failures += actual != expected
        print(f"{status} {name}: expected {expected!r}, got {actual!r}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
