#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)
"""Fail if the extension calls a DuckDB C API function outside the stable prefix.

The binary is stamped for the stable C API (`C_STRUCT`, C API v1.2.0) so that one
build loads into every DuckDB release. That only holds while the code calls
functions in the first STABLE_SLOTS slots of `duckdb_ext_api_v1`; a call to a
later slot needs `USE_UNSTABLE_C_API=1`, which pins the binary to one DuckDB
release (it is what dropped v0.9.1 from the community channel).

libduckdb-sys keeps every slot in a static named `___DUCKDB_<NAME>`; `init`
stores to all of them, so a slot is *used* only if some other instruction loads
it. This script disassembles an unstripped build, finds those loads, and maps
each to its position in the struct.

Usage:
    cargo build --profile profiling --lib --locked
    scripts/check-stable-api.py target/profiling/libbehavioral.so

Needs `nm` and `objdump` (binutils), and the libduckdb-sys sources in the cargo
registry (present after any build). Linux only.
"""

import glob
import os
import re
import subprocess
import sys

# First slot after the stable prefix (`duckdb_row_count`); see CLAUDE.md.
STABLE_SLOTS = 357


def bindings_path() -> str:
    """The libduckdb-sys loadable-extension bindings that the lockfile selects."""
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    with open(os.path.join(root, "Cargo.lock"), encoding="utf-8") as lock:
        match = re.search(r'name = "libduckdb-sys"\nversion = "([^"]+)"', lock.read())
    if not match:
        sys.exit("libduckdb-sys not found in Cargo.lock")
    cargo_home = os.environ.get("CARGO_HOME", os.path.expanduser("~/.cargo"))
    pattern = os.path.join(
        cargo_home,
        "registry",
        "src",
        "*",
        f"libduckdb-sys-{match.group(1)}",
        "src",
        "bindgen_bundled_version_loadable.rs",
    )
    found = glob.glob(pattern)
    if not found:
        sys.exit(f"no bindings at {pattern}; run a cargo build first")
    return found[0]


def slot_order(path: str) -> list[str]:
    """Function names in `duckdb_ext_api_v1` field order, upper-cased, no prefix."""
    with open(path, encoding="utf-8") as src:
        lines = src.read().split("\n")
    start = next(i for i, line in enumerate(lines) if line.startswith("pub struct duckdb_ext_api_v1"))
    order = []
    for line in lines[start + 1 :]:
        if line.startswith("}"):
            break
        match = re.match(r"    pub duckdb_(\w+):", line)
        if match:
            order.append(match.group(1).upper())
    return order


def run(*cmd: str) -> str:
    return subprocess.run(cmd, check=True, capture_output=True, text=True).stdout


def main() -> int:
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    library = sys.argv[1]
    order = slot_order(bindings_path())
    position = {name: i for i, name in enumerate(order)}

    statics = {}
    for line in run("nm", library).split("\n"):
        match = re.match(r"([0-9a-f]+) \w \S*___DUCKDB_(\w+?)(?:\.0)?$", line)
        if match:
            statics[int(match.group(1), 16)] = match.group(2)
    if len(statics) != len(order):
        sys.exit(f"{len(statics)} slot statics in the binary, {len(order)} in the bindings: is it stripped?")

    used = set()
    for line in run("objdump", "-d", "--no-show-raw-insn", library).split("\n"):
        target = re.search(r"#\s+([0-9a-f]+)\s+<", line)
        if not target or int(target.group(1), 16) not in statics:
            continue
        instruction = line.split("\t")[-1].strip()
        if re.match(r"mov\w*\s+%\w+,\S*\(%rip\)", instruction):
            continue  # a store: `init` filling the table
        used.add(statics[int(target.group(1), 16)])

    unknown = sorted(used - position.keys())
    if unknown:
        sys.exit(f"slots missing from the bindings: {unknown}")
    outside = sorted((position[name], name.lower()) for name in used if position[name] >= STABLE_SLOTS)
    highest = max(position[name] for name in used)
    print(f"{len(used)} of {len(order)} slots used; highest is {highest}; stable prefix is {STABLE_SLOTS}")
    if outside:
        for slot, name in outside:
            print(f"outside the stable prefix: slot {slot} duckdb_{name}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
