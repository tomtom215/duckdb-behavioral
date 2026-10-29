#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)
"""Differential test of sequence_match / sequence_count / sequence_match_events
against ClickHouse, with a model of each engine.

Usage:
    scripts/clickhouse-sequence-diff.py --clickhouse PATH --duckdb PATH \
        --extension build/release/behavioral.duckdb_extension [--cases A1/unique,...]
    scripts/clickhouse-sequence-diff.py --self-test        # the model only, no engines

For each case it generates random event groups and random patterns, runs
`clickhouse local` and the extension on the same rows, and compares both with
a Python model of that engine. The point is not "the engines agree" (they
differ on purpose, see docs/src/internals/clickhouse-compatibility.md) but
that every result of each engine, including every difference, is predicted by
a small rule set:

  extension model   lazy depth-first search over events sorted by
                    (timestamp, conditions): the first complete match, else the
                    longest partial chain. A `(?t op N)` gate is measured from
                    the last consumed event and skips events while it can still
                    hold.
  ClickHouse model  the same search with three changes, observed by running
                    ClickHouse (its source was not consulted):
                      1. sequenceMatchEvents reports the first chain, in search
                         order, that is strictly longer than every earlier one,
                         even if that attempt then fails a trailing gate and a
                         later attempt completes the pattern;
                      2. after `.*` a gate is measured from the event where
                         `.*` stopped consuming;
                      3. neither `.*` nor a skipping gate can absorb the last
                         event (backtracking never lets them reach the end);
                    and rows that tie on timestamp stay in arrival order.

Pattern classes: A1 puts gates only after `(?N)` or `.` and never ends in `.*`
or a gate that is vacuous at the end; A2 ends in one of those; B puts a gate
directly after `.*`. Variants: `unique` timestamps; `ties_extorder`, ties
arriving in the extension's order; `ties_shuffled`, ties in random order.
"""

import argparse
import csv
import io
import os
import random
import re
import subprocess
import sys
import tempfile
from collections import Counter

# --------------------------------------------------------------------------
# models
# --------------------------------------------------------------------------
TOKEN = re.compile(r"\(\?t(>=|<=|==|!=|>|<)(\d+)\)|\(\?(\d+)\)|(\.\*)|(\.)")


def parse(pattern):
    steps, pos = [], 0
    for m in TOKEN.finditer(pattern):
        assert m.start() == pos, pattern
        pos = m.end()
        if m.group(1):
            steps.append(("t", m.group(1), int(m.group(2))))
        elif m.group(3):
            steps.append(("c", int(m.group(3))))
        elif m.group(4):
            steps.append(("*",))
        else:
            steps.append((".",))
    assert pos == len(pattern), pattern
    return steps


def gate(op, threshold, last, ts):
    """(satisfied, may_skip_this_event); times are whole seconds."""
    if last is None:
        return True, False
    elapsed = ts - last
    satisfied = {">=": elapsed >= threshold, "<=": elapsed <= threshold, ">": elapsed > threshold,
                 "<": elapsed < threshold, "==": elapsed == threshold, "!=": elapsed != threshold}[op]
    if op in (">=", ">", "!="):
        return satisfied, True
    if op in ("<=", "<"):
        return satisfied, satisfied
    return satisfied, elapsed <= threshold


def _expand(steps, events, state, clickhouse, with_chain):
    """Successor states of one search state, in the order they are tried
    (the last element of the returned list is explored first)."""
    ei, si, last, chain = state
    step = steps[si]
    if ei >= len(events):  # nothing left to consume: trailing items that match the empty rest
        if step[0] == "*" or (step[0] == "t" and (step[1] in ("<=", "<") or (step[1] == ">=" and step[2] == 0))):
            return [(ei, si + 1, last, chain)]
        return []
    ts, mask = events[ei]
    may_consume = not (clickhouse and ei + 1 >= len(events))
    out = []
    if step[0] == "c":
        if mask >> (step[1] - 1) & 1:
            out.append((ei + 1, si + 1, ts, chain + (ts,) if with_chain else chain))
    elif step[0] == "*":
        if may_consume:
            out.append((ei + 1, si, last, chain))
        out.append((ei, si + 1, ts if clickhouse else last, chain))
    elif step[0] == ".":
        out.append((ei + 1, si + 1, ts, chain))
    else:
        advance, skip = gate(step[1], step[2], last, ts)
        if skip and may_consume:
            out.append((ei + 1, si, last, chain))
        if advance:
            out.append((ei, si + 1, last, chain))
    return out


def search_events(steps, events, clickhouse):
    """(completing chain or None, first strictly-longest chain seen) of `(?N)` timestamps."""
    best = ()
    for start in range(len(events)):
        stack = [(start, 0, None, ())]
        while stack:
            state = stack.pop()
            if len(state[3]) > len(best):
                best = state[3]
            if state[1] >= len(steps):
                return state[3], best
            stack.extend(_expand(steps, events, state, clickhouse, True))
    return None, best


def match_end(steps, events, start, clickhouse):
    """Index after the first complete match from `start`, or None."""
    stack = [(start, 0, None, ())]
    while stack:
        state = stack.pop()
        if state[1] >= len(steps):
            return state[0]
        stack.extend(_expand(steps, events, state, clickhouse, False))
    return None


def match_count(steps, events, clickhouse):
    """(matched, non-overlapping count); an empty match still advances one event."""
    if not events:
        return bool(steps) and all(s[0] == "*" for s in steps), 0
    total, start = 0, 0
    while start < len(events):
        end = match_end(steps, events, start, clickhouse)
        if end is None:
            start += 1
        else:
            total += 1
            start = max(end, start + 1)
    return total > 0, total


def predict(steps, events, clickhouse):
    """(match '0'/'1', count, events string) as the engines print them."""
    done, best = search_events(steps, events, clickhouse)
    matched, count = match_count(steps, events, clickhouse)
    chain = best if clickhouse or done is None else done
    return "1" if matched else "0", str(count), ",".join(map(str, chain))


def self_test():
    """The documented examples; no engine needed."""
    rows = [(0, 1), (4, 1), (7, 3), (8, 2), (13, 3), (15, 1), (21, 3), (24, 2), (26, 1), (29, 2)]
    stale = parse("(?2).*(?1)(?t==9)")
    assert predict(stale, rows, False) == ("1", "1", "7,15"), predict(stale, rows, False)
    assert predict(stale, rows, True) == ("1", "1", "7,13"), predict(stale, rows, True)
    two = [(0, 1), (5, 2)]
    for pattern, ext, ch in [("(?1).*(?t>0)(?2)", "1", "0"), ("(?1).*(?t<=2)(?2)", "0", "1"), ("(?1).*(?t>=5)(?2)", "1", "0")]:
        assert predict(parse(pattern), two, False)[0] == ext, pattern
        assert predict(parse(pattern), two, True)[0] == ch, pattern
    print("model self-test passed")


# --------------------------------------------------------------------------
# data generation
# --------------------------------------------------------------------------
def gen_mask(rng, k):
    r = rng.random()
    if r < 0.12:
        return 0
    if r < 0.40:
        mask = 0
        while mask == 0:
            mask = rng.getrandbits(k)
        return mask
    return 1 << rng.randrange(k)


def gen_groups(seed, n_groups, k, tsmax, unique_ts):
    """Random event groups of (timestamp, condition mask), in random arrival order."""
    rng = random.Random(seed)
    groups = []
    for _ in range(n_groups):
        n = rng.randint(1, 14)
        if unique_ts:
            n = min(n, tsmax + 1)
            stamps = rng.sample(range(tsmax + 1), n)
        else:
            stamps = [rng.randint(0, tsmax) for _ in range(n)]
        guided = rng.random() < 0.5
        ordered = sorted(stamps)
        events = []
        for ts in stamps:
            mask = gen_mask(rng, k)
            if guided and mask != 0 and rng.random() < 0.65:
                mask = 1 << (ordered.index(ts) % k)  # bias towards the pattern's order
            rng.randrange(4)  # the value and base-condition draws of the original generator
            rng.random()
            events.append((ts, mask))
        rng.shuffle(events)
        groups.append(events)
    return groups


def gen_pattern(rng, k, pattern_class, tmax):
    ops = ["<", "<=", ">", ">=", "=="]

    def gate_token():
        return f"(?t{rng.choice(ops)}{rng.randint(0, tmax)})"

    while True:
        tokens = []
        for _ in range(rng.randint(1, 5)):
            r = rng.random()
            tokens.append(f"(?{rng.randint(1, k)})" if r < 0.55 else "." if r < 0.75 else ".*")
            if pattern_class in ("A1", "A2"):
                if tokens[-1] != ".*" and rng.random() < 0.35:
                    tokens.append(gate_token())
            else:
                consumed = any(t == "." or (t.startswith("(?") and not t.startswith("(?t")) for t in tokens)
                if rng.random() < 0.35 and consumed:
                    tokens.append(gate_token())
        if any(tokens[i].startswith("(?t") and tokens[i + 1].startswith("(?t") for i in range(len(tokens) - 1)):
            continue  # ClickHouse rejects two gates in a row
        last = tokens[-1]
        vacuous_end = last == ".*" or (last.startswith("(?t") and (last.startswith("(?t<") or last == "(?t>=0)"))
        if pattern_class == "A1" and vacuous_end:
            continue
        if pattern_class == "A2" and not vacuous_end:
            continue
        if pattern_class == "B" and not any(t.startswith("(?t") and i > 0 and tokens[i - 1] == ".*" for i, t in enumerate(tokens)):
            continue
        return "".join(tokens)


# --------------------------------------------------------------------------
# engines
# --------------------------------------------------------------------------
def run_clickhouse(clickhouse, sql):
    out = subprocess.run([clickhouse, "local", "--max_threads=1", "--input_format_parallel_parsing=0", "-q", sql],
                         capture_output=True, text=True, check=False)
    if out.returncode:
        sys.exit("clickhouse local failed: " + out.stderr[:500])
    return {int(line.split("\t")[0]): line.split("\t")[1:] for line in out.stdout.split("\n") if line}


def run_duckdb(duckdb, extension, sql):
    env = {key: value for key, value in os.environ.items() if key not in ("AI_AGENT", "CLAUDECODE")}
    out = subprocess.run([duckdb, "-unsigned", "-csv", "-c", f"LOAD '{extension}'; {sql}"],
                         capture_output=True, text=True, env=env, check=False)
    if out.returncode or "Error" in out.stderr:
        sys.exit("duckdb failed: " + (out.stderr + out.stdout)[:500])
    return {int(row[0]): row[1:] for row in list(csv.reader(io.StringIO(out.stdout)))[1:]}


def clickhouse_columns(pattern, k):
    conds = ", ".join(f"bitAnd(c, {1 << i}) != 0" for i in range(k))
    return [f"sequenceMatch('{pattern}')(toDateTime(ts), {conds})",
            f"sequenceCount('{pattern}')(toDateTime(ts), {conds})",
            "ifNull(arrayStringConcat(arrayMap(x -> toString(toUnixTimestamp(x)), "
            f"sequenceMatchEvents('{pattern}')(toDateTime(ts), {conds})), ','), 'NULL')"]


def duckdb_columns(pattern, k):
    conds = ", ".join(f"(c & {1 << i}) != 0" for i in range(k))
    return [f"CASE sequence_match('{pattern}', t, {conds}) WHEN true THEN '1' WHEN false THEN '0' ELSE 'NULL' END",
            f"COALESCE(sequence_count('{pattern}', t, {conds})::VARCHAR, 'NULL')",
            "COALESCE(array_to_string(list_transform("
            f"sequence_match_events('{pattern}', t, {conds}), lambda x: epoch(x)::BIGINT::VARCHAR), ','), 'NULL')"]


# --------------------------------------------------------------------------
# driver
# --------------------------------------------------------------------------
def run_case(args, pattern_class, variant, workdir):
    totals = Counter()
    for k in (2, 3, 4, 5):
        seed = args.seed * 100 + k
        unique = variant == "unique"
        groups = gen_groups(seed, args.groups, k, 30 if unique else 12, unique)
        if variant == "ties_extorder":
            groups = [sorted(events) for events in groups]
        path = os.path.join(workdir, f"{pattern_class}_{variant}_{k}.csv")
        with open(path, "w", encoding="utf-8") as out:
            for g, events in enumerate(groups):
                for ts, mask in events:
                    out.write(f"{g},{ts},{mask}\n")
        usable = [[e for e in events if e[1]] for events in groups]
        extension_order = [sorted(events) for events in usable]
        arrival_order = [sorted(events, key=lambda e: e[0]) for events in usable]  # stable: ties keep arrival order
        rng = random.Random(seed * 31 + 7)
        patterns, seen = [], set()
        while len(patterns) < args.patterns:
            candidate = gen_pattern(rng, k, pattern_class, 12 if unique else 6)
            if candidate not in seen:
                seen.add(candidate)
                patterns.append(candidate)
        source_ch = f"file('{path}', 'CSV', 'g UInt32, ts UInt32, c UInt32')"
        source_dd = (f"(SELECT g, make_timestamp(ts::BIGINT * 1000000) AS t, c FROM read_csv('{path}', header=false, "
                     "columns={'g':'INTEGER','ts':'INTEGER','c':'BIGINT'}))")
        for i in range(0, len(patterns), 25):
            chunk = patterns[i:i + 25]
            ch_rows = run_clickhouse(args.clickhouse, "SELECT g, " + ", ".join(c for p in chunk for c in clickhouse_columns(p, k))
                                     + f" FROM {source_ch} GROUP BY g ORDER BY g")
            dd_rows = run_duckdb(args.duckdb, args.extension, "SELECT g, " + ", ".join(
                f"{c} AS r{n}" for n, c in enumerate(c for p in chunk for c in duckdb_columns(p, k)))
                + f" FROM {source_dd} GROUP BY g ORDER BY g;")
            for pi, pattern in enumerate(chunk):
                steps = parse(pattern)
                for g in range(len(groups)):
                    mine = predict(steps, extension_order[g], False)
                    theirs = predict(steps, arrival_order[g], True)
                    ch = tuple(ch_rows[g][pi * 3:pi * 3 + 3])
                    dd = tuple(dd_rows[g][pi * 3:pi * 3 + 3])
                    totals["groups x patterns"] += 1
                    for name, got, want in (("extension", dd, mine), ("clickhouse", ch, theirs)):
                        for part, label in enumerate(("match", "count", "events")):
                            totals[f"{name} {label} = model"] += got[part] == want[part]
                    totals["events: clickhouse != extension"] += ch[2] != dd[2]
                    totals["any function: clickhouse != extension"] += ch != dd
    return totals


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--clickhouse")
    parser.add_argument("--duckdb")
    parser.add_argument("--extension")
    parser.add_argument("--cases", default="A1/unique,A1/ties_extorder,A1/ties_shuffled,A2/unique,A2/ties_extorder,"
                                           "A2/ties_shuffled,B/unique,B/ties_extorder")
    parser.add_argument("--patterns", type=int, default=150, help="patterns per condition count (2 to 5)")
    parser.add_argument("--groups", type=int, default=1000)
    parser.add_argument("--seed", type=int, default=1)
    args = parser.parse_args()
    self_test()
    if args.self_test:
        return 0
    if not (args.clickhouse and args.duckdb and args.extension):
        parser.error("--clickhouse, --duckdb and --extension are required unless --self-test")
    args.extension = os.path.abspath(args.extension)
    failures = 0
    with tempfile.TemporaryDirectory() as workdir:
        for case in args.cases.split(","):
            pattern_class, variant = case.split("/")
            totals = run_case(args, pattern_class, variant, workdir)
            cells = totals["groups x patterns"]
            print(f"{case:18} {cells} group x pattern cells; "
                  f"extension = model: " + "/".join(str(totals[f"extension {x} = model"]) for x in ("match", "count", "events"))
                  + "; ClickHouse = model: " + "/".join(str(totals[f"clickhouse {x} = model"]) for x in ("match", "count", "events"))
                  + f" (match/count/events); events differ from ClickHouse in {totals['events: clickhouse != extension']}, "
                  f"any function in {totals['any function: clickhouse != extension']}", flush=True)
            failures += sum(cells - totals[f"{e} {x} = model"] for e in ("extension", "clickhouse") for x in ("match", "count", "events"))
    print("every result of both engines is predicted by its model" if not failures
          else f"{failures} results are not predicted by the models")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
