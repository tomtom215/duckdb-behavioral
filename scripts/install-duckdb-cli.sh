#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)
# =============================================================================
# install-duckdb-cli.sh — download one DuckDB CLI release.
#
# Usage:
#   scripts/install-duckdb-cli.sh <version> <platform> <dest-dir>
#
#   version   DuckDB release tag, e.g. v2.0.0 or v1.3.2
#   platform  linux-amd64 | linux-arm64 | osx-universal
#   dest-dir  directory that receives the `duckdb` executable
#
# DuckDB 2.0.0 renamed its CLI assets: 2.x ships `duckdb-cli-<platform>.tar.gz`,
# older releases ship `duckdb_cli-<platform>.zip`. Both are tried, newest name
# first, so one script serves the whole compat matrix.
# =============================================================================

set -euo pipefail

if [[ $# -ne 3 ]]; then
    echo "usage: $0 <version> <linux-amd64|linux-arm64|osx-universal> <dest-dir>" >&2
    exit 2
fi
version="$1"
platform="$2"
dest="$3"

base="https://github.com/duckdb/duckdb/releases/download/${version}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$dest"

if curl -sSfL -o "$work/cli.tar.gz" "${base}/duckdb-cli-${platform}.tar.gz"; then
    tar -xzf "$work/cli.tar.gz" -C "$work"
elif curl -sSfL -o "$work/cli.zip" "${base}/duckdb_cli-${platform}.zip"; then
    unzip -q "$work/cli.zip" -d "$work"
else
    echo "no DuckDB CLI asset for ${version} on ${platform} at ${base}" >&2
    exit 1
fi

install -m 0755 "$work/duckdb" "$dest/duckdb"
env -u AI_AGENT -u CLAUDECODE "$dest/duckdb" --version
