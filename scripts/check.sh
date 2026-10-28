#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)
# =============================================================================
# check.sh — run the quality checks CI runs, locally.
#
# Usage:
#   ./scripts/check.sh            # everything
#   ./scripts/check.sh --quick    # skip the doc build and benchmark compilation
#
# Tools that are not installed (cargo-nextest, cargo-deny, cargo-machete,
# typos, shellcheck) are reported as SKIP, not as failures; CI always runs
# them. Exit status is non-zero if any check that ran failed.
# =============================================================================

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

# Dev/test builds link a prebuilt libduckdb (the `duckdb` dev-dependency has no
# `bundled` feature). Use the cached download unless the caller points
# DUCKDB_LIB_DIR at an existing library.
if [[ -z "${DUCKDB_LIB_DIR:-}" ]]; then
    export DUCKDB_DOWNLOAD_LIB="${DUCKDB_DOWNLOAD_LIB:-1}"
fi

quick=false
case "${1:-}" in
    "") ;;
    --quick) quick=true ;;
    *)
        echo "usage: $0 [--quick]" >&2
        exit 2
        ;;
esac

passed=0
failed=0
skipped=0

# check <name> <command...>
check() {
    local name="$1"
    shift
    local log
    log="$(mktemp)"
    printf '[CHECK] %s ... ' "$name"
    if "$@" > "$log" 2>&1; then
        echo "PASS"
        passed=$((passed + 1))
    else
        echo "FAIL"
        failed=$((failed + 1))
        tail -n 25 "$log"
    fi
    rm -f "$log"
}

# optional <name> <tool> <command...>: run only if <tool> is on PATH.
optional() {
    local name="$1" tool="$2"
    shift 2
    if command -v "$tool" > /dev/null 2>&1; then
        check "$name" "$@"
    else
        printf '[SKIP] %s (%s not installed)\n' "$name" "$tool"
        skipped=$((skipped + 1))
    fi
}

check "Format" cargo fmt --all -- --check
check "Clippy" cargo clippy --all-targets --locked -- -D warnings
if command -v cargo-nextest > /dev/null 2>&1; then
    check "Tests" cargo nextest run --all-targets --locked
else
    check "Tests" cargo test --all-targets --locked
fi
check "Doc-tests" cargo test --doc --locked

if $quick; then
    printf '[SKIP] Documentation build (--quick)\n[SKIP] Benchmark compilation (--quick)\n'
    skipped=$((skipped + 2))
else
    RUSTDOCFLAGS="-Dwarnings" check "Documentation" cargo doc --no-deps --document-private-items --locked
    check "Benchmarks compile" cargo bench --no-run --locked
fi

optional "Cargo deny" cargo-deny cargo deny check advisories bans licenses sources
optional "Unused dependencies" cargo-machete cargo machete
optional "Spelling" typos typos
optional "Shell scripts" shellcheck shellcheck scripts/*.sh

echo
printf 'Results: %d passed, %d failed, %d skipped\n' "$passed" "$failed" "$skipped"
[[ $failed -eq 0 ]]
