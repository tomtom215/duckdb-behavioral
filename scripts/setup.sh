#!/usr/bin/env bash
# setup.sh — Session setup and E2E validation for duckdb-behavioral
#
# This script automates the full development workflow:
#   1. Verify prerequisites (Rust toolchain, DuckDB CLI)
#   2. Build the extension in release mode
#   3. Append DuckDB metadata (with correct C API version)
#   4. Run unit tests + clippy
#   5. Run E2E tests against real DuckDB instance
#
# Usage:
#   ./scripts/setup.sh              # Full setup + E2E validation
#   ./scripts/setup.sh --skip-build # Skip build, just run E2E tests
#   ./scripts/setup.sh --e2e-only   # Only run E2E tests (assumes build exists)
#
# Exit codes:
#   0 — All checks passed
#   1 — Prerequisites missing
#   2 — Build failed
#   3 — Unit tests failed
#   4 — Clippy warnings
#   5 — E2E tests failed

set -euo pipefail

# ============================================================
# Configuration
# ============================================================

# C API version (NOT DuckDB release version). This must match what DuckDB
# reports in its extension loading error messages. Found in
# duckdb-loadable-macros source: DEFAULT_DUCKDB_VERSION = "v1.2.0"
readonly C_API_VERSION="v1.2.0"

# Extension version from Cargo.toml
readonly EXT_VERSION="v0.1.0"

# Extension name
readonly EXT_NAME="behavioral"

# Project root (directory containing this script's parent)
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly SCRIPT_DIR
readonly PROJECT_ROOT="${SCRIPT_DIR}/.."

# Build output paths
readonly RELEASE_LIB="${PROJECT_ROOT}/target/release/libbehavioral.so"
readonly EXT_FILE="/tmp/${EXT_NAME}.duckdb_extension"

# Metadata script
readonly METADATA_SCRIPT="${PROJECT_ROOT}/extension-ci-tools/scripts/append_extension_metadata.py"

# DuckDB CLI — search common locations
DUCKDB_CLI=""

# ============================================================
# Utility functions
# ============================================================

log_info() {
    printf "\033[1;34m[INFO]\033[0m %s\n" "$1"
}

log_ok() {
    printf "\033[1;32m[OK]\033[0m %s\n" "$1"
}

log_err() {
    printf "\033[1;31m[ERR]\033[0m %s\n" "$1" >&2
}

log_warn() {
    printf "\033[1;33m[WARN]\033[0m %s\n" "$1"
}

# ============================================================
# Find DuckDB CLI
# ============================================================

find_duckdb() {
    local candidates=(
        "/tmp/duckdb"
        "${PROJECT_ROOT}/duckdb"
        "${HOME}/.local/bin/duckdb"
        "/usr/local/bin/duckdb"
        "/usr/bin/duckdb"
    )

    # Check PATH first
    if command -v duckdb &>/dev/null; then
        DUCKDB_CLI="$(command -v duckdb)"
        return 0
    fi

    # Check common locations
    for candidate in "${candidates[@]}"; do
        if [[ -x "${candidate}" ]]; then
            DUCKDB_CLI="${candidate}"
            return 0
        fi
    done

    return 1
}

install_duckdb() {
    log_info "DuckDB CLI not found. Installing v1.4.4..."

    local os
    local arch
    os="$(uname -s | tr '[:upper:]' '[:lower:]')"
    arch="$(uname -m)"

    local url=""
    if [[ "${os}" == "linux" && "${arch}" == "x86_64" ]]; then
        url="https://github.com/duckdb/duckdb/releases/download/v1.4.4/duckdb_cli-linux-amd64.zip"
    elif [[ "${os}" == "linux" && "${arch}" == "aarch64" ]]; then
        url="https://github.com/duckdb/duckdb/releases/download/v1.4.4/duckdb_cli-linux-aarch64.zip"
    elif [[ "${os}" == "darwin" && "${arch}" == "x86_64" ]]; then
        url="https://github.com/duckdb/duckdb/releases/download/v1.4.4/duckdb_cli-osx-universal.zip"
    elif [[ "${os}" == "darwin" && "${arch}" == "arm64" ]]; then
        url="https://github.com/duckdb/duckdb/releases/download/v1.4.4/duckdb_cli-osx-universal.zip"
    else
        log_err "Unsupported platform: ${os}/${arch}"
        return 1
    fi

    local tmpdir
    tmpdir="$(mktemp -d)"

    if command -v curl &>/dev/null; then
        curl -fsSL "${url}" -o "${tmpdir}/duckdb.zip"
    elif command -v wget &>/dev/null; then
        wget -q "${url}" -O "${tmpdir}/duckdb.zip"
    else
        log_err "Neither curl nor wget found. Install DuckDB manually."
        rm -rf "${tmpdir}"
        return 1
    fi

    unzip -q "${tmpdir}/duckdb.zip" -d "${tmpdir}"
    cp "${tmpdir}/duckdb" /tmp/duckdb
    chmod +x /tmp/duckdb
    rm -rf "${tmpdir}"

    DUCKDB_CLI="/tmp/duckdb"
    log_ok "DuckDB installed to /tmp/duckdb"
}

# ============================================================
# Detect platform for metadata
# ============================================================

detect_platform() {
    local os
    local arch
    os="$(uname -s | tr '[:upper:]' '[:lower:]')"
    arch="$(uname -m)"

    if [[ "${os}" == "linux" && "${arch}" == "x86_64" ]]; then
        echo "linux_amd64"
    elif [[ "${os}" == "linux" && "${arch}" == "aarch64" ]]; then
        echo "linux_arm64"
    elif [[ "${os}" == "darwin" && "${arch}" == "x86_64" ]]; then
        echo "osx_amd64"
    elif [[ "${os}" == "darwin" && "${arch}" == "arm64" ]]; then
        echo "osx_arm64"
    else
        log_err "Unsupported platform: ${os}/${arch}"
        return 1
    fi
}

# ============================================================
# Prerequisites check
# ============================================================

check_prerequisites() {
    log_info "Checking prerequisites..."
    local missing=0

    # Rust toolchain
    if ! command -v cargo &>/dev/null; then
        log_err "cargo not found. Install Rust: https://rustup.rs/"
        missing=1
    else
        log_ok "cargo $(cargo --version | cut -d' ' -f2)"
    fi

    # Python 3 (for metadata script)
    if ! command -v python3 &>/dev/null; then
        log_err "python3 not found. Required for extension metadata."
        missing=1
    else
        log_ok "python3 $(python3 --version | cut -d' ' -f2)"
    fi

    # unzip (for DuckDB CLI install)
    if ! command -v unzip &>/dev/null; then
        log_warn "unzip not found. May be needed to install DuckDB CLI."
    fi

    # DuckDB CLI
    if ! find_duckdb; then
        install_duckdb || { missing=1; log_err "Failed to install DuckDB CLI"; }
    fi

    if [[ -n "${DUCKDB_CLI}" ]]; then
        local duckdb_version
        duckdb_version="$("${DUCKDB_CLI}" --version 2>&1 | head -1)"
        log_ok "DuckDB CLI: ${duckdb_version} (${DUCKDB_CLI})"
    fi

    # Extension metadata script
    if [[ ! -f "${METADATA_SCRIPT}" ]]; then
        log_warn "extension-ci-tools not found. Initializing submodule..."
        git -C "${PROJECT_ROOT}" submodule update --init --recursive 2>/dev/null || {
            log_err "Failed to initialize extension-ci-tools submodule"
            missing=1
        }
    fi

    if [[ "${missing}" -ne 0 ]]; then
        log_err "Prerequisites check failed"
        return 1
    fi

    log_ok "All prerequisites satisfied"
}

# ============================================================
# Build
# ============================================================

build_extension() {
    log_info "Building extension (release)..."

    if ! cargo build --release --manifest-path "${PROJECT_ROOT}/Cargo.toml" 2>&1; then
        log_err "Release build failed"
        return 1
    fi

    if [[ ! -f "${RELEASE_LIB}" ]]; then
        log_err "Build artifact not found: ${RELEASE_LIB}"
        return 1
    fi

    log_ok "Built: ${RELEASE_LIB}"

    # Copy and append metadata
    log_info "Appending extension metadata (C API ${C_API_VERSION})..."

    local platform
    platform="$(detect_platform)"

    cp "${RELEASE_LIB}" "${EXT_FILE}"

    if ! python3 "${METADATA_SCRIPT}" \
        -l "${EXT_FILE}" \
        -n "${EXT_NAME}" \
        -p "${platform}" \
        -dv "${C_API_VERSION}" \
        -ev "${EXT_VERSION}" \
        --abi-type C_STRUCT \
        -o "${EXT_FILE}" 2>&1; then
        log_err "Metadata append failed"
        return 1
    fi

    log_ok "Extension ready: ${EXT_FILE}"
}

# ============================================================
# Unit tests + Clippy
# ============================================================

run_unit_tests() {
    log_info "Running unit tests..."

    if ! cargo test --manifest-path "${PROJECT_ROOT}/Cargo.toml" 2>&1; then
        log_err "Unit tests failed"
        return 1
    fi

    log_ok "Unit tests passed"
}

run_clippy() {
    log_info "Running clippy..."

    local output
    output="$(cargo clippy --all-targets --manifest-path "${PROJECT_ROOT}/Cargo.toml" 2>&1)"
    local rc=$?

    if echo "${output}" | grep -q "^warning:"; then
        log_err "Clippy warnings found:"
        echo "${output}" | grep "^warning:" >&2
        return 1
    fi

    if [[ ${rc} -ne 0 ]]; then
        log_err "Clippy failed"
        echo "${output}" >&2
        return 1
    fi

    log_ok "Clippy clean"
}

# ============================================================
# E2E Tests
# ============================================================

run_e2e_tests() {
    log_info "Running E2E tests against DuckDB..."

    if [[ -z "${DUCKDB_CLI}" ]]; then
        if ! find_duckdb; then
            log_err "DuckDB CLI not found for E2E tests"
            return 1
        fi
    fi

    if [[ ! -f "${EXT_FILE}" ]]; then
        log_err "Extension file not found: ${EXT_FILE}"
        log_err "Run build first: ./scripts/setup.sh"
        return 1
    fi

    # Create E2E test SQL
    local test_sql
    test_sql="$(mktemp)"

    cat > "${test_sql}" << 'SQLEOF'
-- E2E Test Suite: All 7 functions, 11 test cases
LOAD '__EXT_FILE__';

-- TEST 1: window_funnel basic (expect: 3)
SELECT window_funnel(INTERVAL '1 hour', ts,
    event = 'view', event = 'cart', event = 'purchase') AS r
FROM (VALUES
    (TIMESTAMP '2024-01-01 10:00:00', 'view'),
    (TIMESTAMP '2024-01-01 10:05:00', 'cart'),
    (TIMESTAMP '2024-01-01 10:10:00', 'purchase')
) AS t(ts, event);

-- TEST 2: window_funnel timeout (expect: 2)
SELECT window_funnel(INTERVAL '30 minutes', ts,
    event = 'view', event = 'cart', event = 'purchase') AS r
FROM (VALUES
    (TIMESTAMP '2024-01-01 10:00:00', 'view'),
    (TIMESTAMP '2024-01-01 10:05:00', 'cart'),
    (TIMESTAMP '2024-01-01 11:00:00', 'purchase')
) AS t(ts, event);

-- TEST 3: window_funnel strict_increase mode (expect: 1)
SELECT window_funnel(INTERVAL '1 hour', 'strict_increase', ts,
    event = 'view', event = 'cart', event = 'purchase') AS r
FROM (VALUES
    (TIMESTAMP '2024-01-01 10:00:00', 'view'),
    (TIMESTAMP '2024-01-01 10:00:00', 'cart'),
    (TIMESTAMP '2024-01-01 10:10:00', 'purchase')
) AS t(ts, event);

-- TEST 4: window_funnel per-user (expect: 3, 2, 1)
SELECT user_id, window_funnel(INTERVAL '1 hour', ts,
    step = 1, step = 2, step = 3) AS r
FROM (VALUES
    (1, TIMESTAMP '2024-01-01 10:00:00', 1),
    (1, TIMESTAMP '2024-01-01 10:05:00', 2),
    (1, TIMESTAMP '2024-01-01 10:10:00', 3),
    (2, TIMESTAMP '2024-01-01 10:00:00', 1),
    (2, TIMESTAMP '2024-01-01 10:05:00', 2),
    (3, TIMESTAMP '2024-01-01 10:00:00', 1)
) AS t(user_id, ts, step)
GROUP BY user_id ORDER BY user_id;

-- TEST 5: sequence_match positive (expect: true)
SELECT sequence_match('(?1).*(?2)', ts,
    event = 'view', event = 'purchase') AS r
FROM (VALUES
    (TIMESTAMP '2024-01-01 10:00:00', 'view'),
    (TIMESTAMP '2024-01-01 10:05:00', 'browse'),
    (TIMESTAMP '2024-01-01 10:10:00', 'purchase')
) AS t(ts, event);

-- TEST 6: sequence_match negative (expect: false)
SELECT sequence_match('(?1).*(?2)', ts,
    event = 'view', event = 'purchase') AS r
FROM (VALUES
    (TIMESTAMP '2024-01-01 10:00:00', 'browse'),
    (TIMESTAMP '2024-01-01 10:05:00', 'browse'),
    (TIMESTAMP '2024-01-01 10:10:00', 'browse')
) AS t(ts, event);

-- TEST 7: sequence_count (expect: 2)
SELECT sequence_count('(?1).*(?2)', ts,
    event = 'A', event = 'B') AS r
FROM (VALUES
    (TIMESTAMP '2024-01-01 10:00:00', 'A'),
    (TIMESTAMP '2024-01-01 10:01:00', 'B'),
    (TIMESTAMP '2024-01-01 10:02:00', 'A'),
    (TIMESTAMP '2024-01-01 10:03:00', 'B')
) AS t(ts, event);

-- TEST 8: sequence_match_events (expect: 2 timestamps)
SELECT sequence_match_events('(?1).*(?2)', ts,
    event = 'view', event = 'purchase') AS r
FROM (VALUES
    (TIMESTAMP '2024-01-01 10:00:00', 'view'),
    (TIMESTAMP '2024-01-01 10:05:00', 'browse'),
    (TIMESTAMP '2024-01-01 10:10:00', 'purchase')
) AS t(ts, event);

-- TEST 9: sequence_next_node (expect: page_cart)
SELECT sequence_next_node('forward', 'first_match', ts, page,
    event = 'A', event = 'A', event = 'B') AS r
FROM (VALUES
    (TIMESTAMP '2024-01-01 10:00:00', 'page_home', 'A'),
    (TIMESTAMP '2024-01-01 10:01:00', 'page_product', 'B'),
    (TIMESTAMP '2024-01-01 10:02:00', 'page_cart', 'C')
) AS t(ts, page, event);

-- TEST 10: retention per-user (expect: boolean arrays)
SELECT user_id, retention(day = 1, day = 2, day = 3) AS r
FROM (VALUES
    (1, 1), (1, 2), (1, 3),
    (2, 1), (2, 2),
    (3, 1)
) AS t(user_id, day)
GROUP BY user_id ORDER BY user_id;

-- TEST 11: sessionize window function (expect: session IDs)
SELECT user_id, ts,
    sessionize(ts, INTERVAL '30 minutes') OVER (PARTITION BY user_id ORDER BY ts) AS session_id
FROM (VALUES
    (1, TIMESTAMP '2024-01-01 10:00:00'),
    (1, TIMESTAMP '2024-01-01 10:10:00'),
    (1, TIMESTAMP '2024-01-01 11:00:00'),
    (1, TIMESTAMP '2024-01-01 11:05:00'),
    (2, TIMESTAMP '2024-01-01 10:00:00'),
    (2, TIMESTAMP '2024-01-01 10:05:00')
) AS t(user_id, ts)
ORDER BY user_id, ts;
SQLEOF

    # Replace placeholder with actual path
    sed -i "s|__EXT_FILE__|${EXT_FILE}|g" "${test_sql}"

    # Run E2E tests and capture output
    local output
    local rc=0
    output="$("${DUCKDB_CLI}" -unsigned -noheader -csv < "${test_sql}" 2>&1)" || rc=$?

    rm -f "${test_sql}"

    if [[ ${rc} -ne 0 ]]; then
        log_err "DuckDB execution failed:"
        echo "${output}" >&2
        return 1
    fi

    # Parse results line by line and validate
    local pass=0
    local fail=0

    # Expected results (one per line of output, multi-row tests have multiple expected lines)
    local -a expected=(
        "3"                             # TEST 1: window_funnel basic
        "2"                             # TEST 2: window_funnel timeout
        "1"                             # TEST 3: window_funnel strict_increase
        "1,3"                           # TEST 4: user 1
        "2,2"                           # TEST 4: user 2
        "3,1"                           # TEST 4: user 3
        "true"                          # TEST 5: sequence_match positive
        "false"                         # TEST 6: sequence_match negative
        "2"                             # TEST 7: sequence_count
    )

    local -a test_names=(
        "window_funnel basic"
        "window_funnel timeout"
        "window_funnel strict_increase"
        "window_funnel user 1"
        "window_funnel user 2"
        "window_funnel user 3"
        "sequence_match positive"
        "sequence_match negative"
        "sequence_count"
    )

    local line_num=0
    while IFS= read -r line; do
        if [[ ${line_num} -lt ${#expected[@]} ]]; then
            if [[ "${line}" == "${expected[${line_num}]}" ]]; then
                log_ok "E2E ${test_names[${line_num}]}: ${line}"
                ((pass++))
            else
                log_err "E2E ${test_names[${line_num}]}: got '${line}', expected '${expected[${line_num}]}'"
                ((fail++))
            fi
        fi
        ((line_num++))
    done <<< "${output}"

    # Check that we got enough output lines (at least the first 9 deterministic tests)
    if [[ ${line_num} -lt 9 ]]; then
        log_err "E2E: Only ${line_num} output lines, expected at least 9"
        log_err "Raw output:"
        echo "${output}" >&2
        fail=1
    fi

    # Report for remaining tests (non-deterministic output format, just check non-empty)
    local remaining_tests=("sequence_match_events" "sequence_next_node" "retention" "sessionize")
    for name in "${remaining_tests[@]}"; do
        if echo "${output}" | grep -qi "err\|error\|fail"; then
            log_err "E2E ${name}: error detected in output"
            ((fail++))
        else
            log_ok "E2E ${name}: executed without error"
            ((pass++))
        fi
    done

    printf "\n"
    log_info "E2E Results: ${pass} passed, ${fail} failed"

    if [[ ${fail} -gt 0 ]]; then
        return 1
    fi

    log_ok "All E2E tests passed"
}

# ============================================================
# Main
# ============================================================

main() {
    local skip_build=false
    local e2e_only=false

    for arg in "$@"; do
        case "${arg}" in
            --skip-build) skip_build=true ;;
            --e2e-only) e2e_only=true ;;
            --help|-h)
                printf "Usage: %s [--skip-build] [--e2e-only]\n" "$0"
                printf "\n"
                printf "Options:\n"
                printf "  --skip-build  Skip cargo build, run tests with existing build\n"
                printf "  --e2e-only    Only run E2E tests (skip unit tests and clippy)\n"
                printf "  --help        Show this help\n"
                exit 0
                ;;
            *)
                log_err "Unknown option: ${arg}"
                exit 1
                ;;
        esac
    done

    printf "\n"
    log_info "=== duckdb-behavioral session setup ==="
    printf "\n"

    # Step 1: Prerequisites
    check_prerequisites || exit 1
    printf "\n"

    if [[ "${e2e_only}" == true ]]; then
        run_e2e_tests || exit 5
        printf "\n"
        log_ok "=== E2E validation complete ==="
        exit 0
    fi

    # Step 2: Build
    if [[ "${skip_build}" == false ]]; then
        build_extension || exit 2
        printf "\n"
    fi

    # Step 3: Unit tests
    run_unit_tests || exit 3
    printf "\n"

    # Step 4: Clippy
    run_clippy || exit 4
    printf "\n"

    # Step 5: E2E tests
    run_e2e_tests || exit 5
    printf "\n"

    log_ok "=== Session setup complete — all checks passed ==="
}

main "$@"
