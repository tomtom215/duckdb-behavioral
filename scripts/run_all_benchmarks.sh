#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)
# Run all benchmarks sequentially and save results.
# Usage: bash scripts/run_all_benchmarks.sh
set -euo pipefail

RESULTS_DIR="/tmp/bench_results"
mkdir -p "$RESULTS_DIR"

echo "=== Starting full benchmark suite ==="
echo "Results will be saved to $RESULTS_DIR/"
echo ""

for bench in sessionize_bench retention_bench window_funnel_bench sequence_bench sort_bench sequence_next_node_bench sequence_match_events_bench; do
    echo "=== Running $bench ==="
    echo "Started at: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    cargo bench --bench "$bench" 2>&1 | tee "$RESULTS_DIR/${bench}.txt"
    echo "Completed at: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo ""
done

echo "=== All benchmarks complete ==="
echo "Results saved to $RESULTS_DIR/"
