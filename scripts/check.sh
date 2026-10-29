#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)
# =============================================================================
# check.sh — run the quality checks CI runs, locally.
#
# Usage:
#   ./scripts/check.sh            # everything
#   ./scripts/check.sh --quick    # skip the doc build and the stable-API check
#
# Tools that are not installed (cargo-nextest, cargo-deny, cargo-machete,
# typos, shellcheck, actionlint, zizmor) are reported as SKIP, not as failures;
# CI always runs them. Not covered here: the MSRV build, the cross-target
# checks, and the macOS jobs. Exit status is non-zero if any check that ran
# failed.
# =============================================================================

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

# The in-process extension-load tests (feature `e2e`) link a prebuilt libduckdb.
# Use the cached download unless the caller points DUCKDB_LIB_DIR at an
# existing library.
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
check "Clippy" cargo clippy --all-targets --all-features --locked -- -D warnings
# `--lib --tests`, not `--all-targets`: that would also run the Criterion
# benchmarks as tests, at full scale.
if command -v cargo-nextest > /dev/null 2>&1; then
    check "Tests" cargo nextest run --lib --tests --all-features --locked
else
    check "Tests" cargo test --lib --tests --all-features --locked
fi
check "Doc-tests" cargo test --doc --locked

if $quick; then
    printf '[SKIP] Documentation build (--quick)\n'
    skipped=$((skipped + 1))
else
    RUSTDOCFLAGS="-Dwarnings" check "Documentation" cargo doc --no-deps --document-private-items --locked
fi

# Builds an unstripped release library (about 40 s), so --quick skips it.
if $quick; then
    printf '[SKIP] Stable C API surface (--quick)\n'
    skipped=$((skipped + 1))
else
    optional "Stable C API surface" objdump bash -c \
        'env -u DUCKDB_DOWNLOAD_LIB -u DUCKDB_LIB_DIR cargo build --profile profiling --lib --locked &&
         python3 -I scripts/check-stable-api.py target/profiling/libbehavioral.so'
fi

optional "Cargo deny" cargo-deny cargo deny check advisories bans licenses sources
optional "Unused dependencies" cargo-machete cargo machete --with-metadata
optional "Spelling" typos typos
optional "Shell scripts" shellcheck shellcheck scripts/*.sh
optional "Workflow lint" actionlint actionlint
optional "Workflow security" zizmor zizmor --offline --no-progress --min-severity medium .github
check "License headers" scripts/add-spdx-headers.sh --check

echo
printf 'Results: %d passed, %d failed, %d skipped\n' "$passed" "$failed" "$skipped"
[[ $failed -eq 0 ]]
