#!/usr/bin/env bash
#
# run-benchmarks.sh — Run the full duckdb-behavioral benchmark suite
#
# Runs all 7 Criterion benchmark groups, captures results, and produces
# a summary table. Designed for reproducibility: any machine running this
# script with Rust installed will produce comparable relative numbers.
#
# Usage:
#   ./scripts/run-benchmarks.sh              # Full suite
#   ./scripts/run-benchmarks.sh sessionize   # Single group
#   ./scripts/run-benchmarks.sh --help       # Help
#
# Output:
#   - Criterion results in target/criterion/
#   - Summary table printed to stdout
#   - Full output saved to target/benchmark-results.txt
#
# Requirements:
#   - Rust toolchain (cargo, rustc)
#   - Sufficient RAM: ~16 GB recommended for 100M-element benchmarks
#   - Estimated time: 30-60 minutes for the full suite
#

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly SCRIPT_DIR
PROJECT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
readonly PROJECT_DIR
readonly OUTPUT_FILE="${PROJECT_DIR}/target/benchmark-results.txt"
readonly SUMMARY_FILE="${PROJECT_DIR}/target/benchmark-summary.md"

# All benchmark groups in the suite
readonly ALL_GROUPS=(
    retention
    sequence
    sequence_match_events
    sequence_next_node
    sessionize
    sort
    window_funnel
)

usage() {
    cat <<USAGE
Usage: $(basename "$0") [OPTIONS] [GROUP...]

Run the duckdb-behavioral benchmark suite.

Groups:
  retention              Retention update + combine benchmarks
  sequence               sequence_match + sequence_count + combine
  sequence_match_events  sequence_match_events + combine
  sequence_next_node     sequence_next_node + combine + realistic
  sessionize             sessionize update + combine (includes 1B)
  sort                   Event sort + presorted detection
  window_funnel          window_funnel finalize + combine

Options:
  --help    Show this help message
  --quick   Run only small-scale benchmarks (100 to 100K elements)
  --all     Run all groups (default when no groups specified)

Examples:
  $(basename "$0")                    # Full suite, all groups
  $(basename "$0") sessionize         # Only sessionize benchmarks
  $(basename "$0") sequence sort      # Two groups
  $(basename "$0") --quick            # Quick validation run
USAGE
}

log() {
    printf '[%s] %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$1"
}

check_prerequisites() {
    local missing=0

    if ! command -v cargo >/dev/null 2>&1; then
        log "ERROR: cargo not found. Install Rust: https://rustup.rs"
        missing=1
    fi

    if ! command -v rustc >/dev/null 2>&1; then
        log "ERROR: rustc not found. Install Rust: https://rustup.rs"
        missing=1
    fi

    if [ "${missing}" -ne 0 ]; then
        exit 1
    fi

    log "Rust toolchain: $(rustc --version)"
    log "Cargo: $(cargo --version)"
}

extract_headline() {
    # Extract the median time and throughput for a given benchmark name
    # from Criterion output. Looks for lines like:
    #   benchmark_name  time:   [1.18 s 1.21 s 1.24 s]
    #                   thrpt:  [800 Melem/s 826 Melem/s 850 Melem/s]
    local file="$1"
    local bench_name="$2"

    local time_line
    time_line=$(grep -A1 "^${bench_name}\b" "${file}" | grep "time:" | head -1 || true)
    local thrpt_line
    thrpt_line=$(grep -A2 "^${bench_name}\b" "${file}" | grep "thrpt:" | head -1 || true)

    if [ -z "${time_line}" ]; then
        # Try format where name is on separate line from results
        time_line=$(grep -A2 "^${bench_name}$" "${file}" | grep "time:" | head -1 || true)
        thrpt_line=$(grep -A3 "^${bench_name}$" "${file}" | grep "thrpt:" | head -1 || true)
    fi

    local median_time=""
    local median_thrpt=""

    if [ -n "${time_line}" ]; then
        # Extract middle value from [low median high] format
        median_time=$(echo "${time_line}" | sed -E 's/.*\[.+ (.+) .+\]/\1/')
    fi

    if [ -n "${thrpt_line}" ]; then
        median_thrpt=$(echo "${thrpt_line}" | sed -E 's/.*\[.+ (.+ [MGK]elem\/s) .+\]/\1/')
    fi

    printf '%s\t%s' "${median_time}" "${median_thrpt}"
}

run_benchmarks() {
    local groups=("$@")

    log "Starting benchmark suite: ${#groups[@]} group(s)"
    log "Output: ${OUTPUT_FILE}"
    log "Working directory: ${PROJECT_DIR}"

    mkdir -p "${PROJECT_DIR}/target"

    # Clear previous output
    : > "${OUTPUT_FILE}"

    local start_time
    start_time=$(date +%s)

    for group in "${groups[@]}"; do
        log "Running benchmark group: ${group}"
        local group_start
        group_start=$(date +%s)

        if ! cargo bench --bench "${group}_bench" 2>&1 | tee -a "${OUTPUT_FILE}"; then
            log "WARNING: Benchmark group '${group}' failed"
        fi

        local group_end
        group_end=$(date +%s)
        local group_elapsed=$(( group_end - group_start ))
        log "Completed ${group} in ${group_elapsed}s"
    done

    local end_time
    end_time=$(date +%s)
    local total_elapsed=$(( end_time - start_time ))

    log "All benchmarks completed in ${total_elapsed}s"
}

generate_summary() {
    log "Generating summary table..."

    # Define headline benchmarks (name, scale label)
    local -a headlines=(
        "sessionize_update/1000000000|1 billion"
        "retention_combine/100000000|100 million"
        "window_funnel_finalize/events=100000000,conds=8/100000000|100 million"
        "sequence_match/100000000|100 million"
        "sequence_count/100000000|100 million"
        "sequence_match_events/100000000|100 million"
        "sequence_next_node/10000000|10 million"
    )

    {
        printf '# Benchmark Summary\n\n'
        printf 'Generated: %s\n' "$(date -u '+%Y-%m-%d %H:%M:%S UTC')"
        printf 'Rust: %s\n' "$(rustc --version)"
        printf 'Platform: %s\n\n' "$(uname -srm)"
        printf '## Headline Numbers\n\n'
        printf '| Function | Scale | Wall Clock | Throughput |\n'
        printf '|---|---|---|---|\n'
    } > "${SUMMARY_FILE}"

    for entry in "${headlines[@]}"; do
        local bench_name="${entry%%|*}"
        local scale="${entry##*|}"
        local func_name="${bench_name%%/*}"

        local result
        result=$(extract_headline "${OUTPUT_FILE}" "${bench_name}")
        local time_val="${result%%	*}"
        local thrpt_val="${result##*	}"

        if [ -n "${time_val}" ] && [ -n "${thrpt_val}" ]; then
            printf '| `%s` | %s | %s | %s |\n' \
                "${func_name}" "${scale}" "${time_val}" "${thrpt_val}" \
                >> "${SUMMARY_FILE}"
        else
            printf '| `%s` | %s | (not found) | (not found) |\n' \
                "${func_name}" "${scale}" \
                >> "${SUMMARY_FILE}"
        fi
    done

    printf '\nFull results: `target/benchmark-results.txt`\n' >> "${SUMMARY_FILE}"
    printf 'Criterion HTML reports: `target/criterion/`\n' >> "${SUMMARY_FILE}"

    log "Summary written to ${SUMMARY_FILE}"
    printf '\n'
    cat "${SUMMARY_FILE}"
}

main() {
    local quick=0
    local groups=()

    while [ $# -gt 0 ]; do
        case "$1" in
            --help|-h)
                usage
                exit 0
                ;;
            --quick)
                quick=1
                shift
                ;;
            --all)
                shift
                ;;
            -*)
                log "ERROR: Unknown option: $1"
                usage
                exit 1
                ;;
            *)
                groups+=("$1")
                shift
                ;;
        esac
    done

    # Default to all groups
    if [ ${#groups[@]} -eq 0 ]; then
        groups=("${ALL_GROUPS[@]}")
    fi

    # Validate group names
    for group in "${groups[@]}"; do
        local valid=0
        for known in "${ALL_GROUPS[@]}"; do
            if [ "${group}" = "${known}" ]; then
                valid=1
                break
            fi
        done
        if [ "${valid}" -eq 0 ]; then
            log "ERROR: Unknown benchmark group: ${group}"
            log "Valid groups: ${ALL_GROUPS[*]}"
            exit 1
        fi
    done

    cd "${PROJECT_DIR}"

    check_prerequisites

    if [ "${quick}" -eq 1 ]; then
        log "Quick mode: running benchmarks with --quick flag"
        log "Note: Criterion does not support scale filtering via CLI."
        log "Running full benchmarks but with reduced measurement time."
        export CRITERION_MEASUREMENT_TIME=3
    fi

    run_benchmarks "${groups[@]}"
    generate_summary
}

main "$@"
