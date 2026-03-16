#!/usr/bin/env bash
# =============================================================================
# check.sh — Run all quality checks for duckdb-behavioral
#
# Usage:
#   ./scripts/check.sh          # Run all checks
#   ./scripts/check.sh --quick  # Skip benchmarks and doc build
# =============================================================================

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BOLD='\033[1m'
NC='\033[0m' # No Color

QUICK=false
if [[ "${1:-}" == "--quick" ]]; then
    QUICK=true
fi

passed=0
failed=0
skipped=0

run_check() {
    local name="$1"
    shift
    printf "${BOLD}[CHECK]${NC} %s ... " "$name"
    if "$@" > /dev/null 2>&1; then
        printf "${GREEN}PASS${NC}\n"
        ((passed++))
    else
        printf "${RED}FAIL${NC}\n"
        ((failed++))
        # Re-run to show error output
        "$@" 2>&1 | tail -20
    fi
}

skip_check() {
    local name="$1"
    printf "${BOLD}[SKIP]${NC} %s ${YELLOW}(--quick)${NC}\n" "$name"
    ((skipped++))
}

echo ""
echo "========================================="
echo "  duckdb-behavioral Quality Checks"
echo "========================================="
echo ""

# Core checks (always run)
run_check "Format check"   cargo fmt -- --check
run_check "Clippy lints"   cargo clippy --all-targets
run_check "Unit tests"     cargo test
run_check "Compilation"    cargo check --all-targets

# Extended checks (skip with --quick)
if $QUICK; then
    skip_check "Documentation build"
    skip_check "Benchmark compilation"
else
    run_check "Documentation build" cargo doc --no-deps
    run_check "Benchmark compilation" cargo bench --no-run
fi

echo ""
echo "========================================="
printf "  Results: ${GREEN}%d passed${NC}" "$passed"
if [[ $failed -gt 0 ]]; then
    printf ", ${RED}%d failed${NC}" "$failed"
fi
if [[ $skipped -gt 0 ]]; then
    printf ", ${YELLOW}%d skipped${NC}" "$skipped"
fi
echo ""
echo "========================================="
echo ""

if [[ $failed -gt 0 ]]; then
    exit 1
fi
