#!/usr/bin/env bash
# Script to add SPDX license headers and copyright notices to all source files.
# This script is idempotent: it skips files that already contain an SPDX header.
#
# Usage: bash scripts/add-spdx-headers.sh [--check]
#   --check  Only verify headers exist; exit 1 if any are missing.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

COPYRIGHT_HOLDER="Tom F."
REPO_URL="https://github.com/tomtom215/duckdb-behavioral"
YEAR="2026"
CHECK_ONLY=false

if [[ "${1:-}" == "--check" ]]; then
    CHECK_ONLY=true
fi

MISSING=()

add_header_hash() {
    local file="$1"
    if grep -q "SPDX-License-Identifier" "$file" 2>/dev/null; then
        return 0
    fi
    if $CHECK_ONLY; then
        MISSING+=("$file")
        return 0
    fi
    local tmp
    tmp=$(mktemp)
    # Preserve shebang if present
    if head -1 "$file" | grep -q '^#!'; then
        head -1 "$file" > "$tmp"
        echo "# SPDX-License-Identifier: MIT" >> "$tmp"
        echo "# Copyright (c) ${YEAR} ${COPYRIGHT_HOLDER} (${REPO_URL})" >> "$tmp"
        tail -n +2 "$file" >> "$tmp"
    else
        echo "# SPDX-License-Identifier: MIT" > "$tmp"
        echo "# Copyright (c) ${YEAR} ${COPYRIGHT_HOLDER} (${REPO_URL})" >> "$tmp"
        # Add blank line separator if file doesn't start with blank line or comment
        if head -1 "$file" | grep -qvE '^(#|$)'; then
            echo "" >> "$tmp"
        fi
        cat "$file" >> "$tmp"
    fi
    cp "$tmp" "$file"
    rm "$tmp"
    echo "  Added header: $file"
}

add_header_rust() {
    local file="$1"
    if grep -q "SPDX-License-Identifier" "$file" 2>/dev/null; then
        return 0
    fi
    if $CHECK_ONLY; then
        MISSING+=("$file")
        return 0
    fi
    local tmp
    tmp=$(mktemp)
    echo "// SPDX-License-Identifier: MIT" > "$tmp"
    echo "// Copyright (c) ${YEAR} ${COPYRIGHT_HOLDER} (${REPO_URL})" >> "$tmp"
    # Add blank line if file doesn't start with blank line
    if head -1 "$file" | grep -qvE '^$'; then
        echo "" >> "$tmp"
    fi
    cat "$file" >> "$tmp"
    cp "$tmp" "$file"
    rm "$tmp"
    echo "  Added header: $file"
}

add_header_html() {
    local file="$1"
    if grep -q "SPDX-License-Identifier" "$file" 2>/dev/null; then
        return 0
    fi
    if $CHECK_ONLY; then
        MISSING+=("$file")
        return 0
    fi
    local tmp
    tmp=$(mktemp)
    # Preserve DOCTYPE/html tag if present on line 1
    if head -1 "$file" | grep -qi '<!doctype\|<html'; then
        head -1 "$file" > "$tmp"
        echo "<!-- SPDX-License-Identifier: MIT -->" >> "$tmp"
        echo "<!-- Copyright (c) ${YEAR} ${COPYRIGHT_HOLDER} (${REPO_URL}) -->" >> "$tmp"
        tail -n +2 "$file" >> "$tmp"
    else
        echo "<!-- SPDX-License-Identifier: MIT -->" > "$tmp"
        echo "<!-- Copyright (c) ${YEAR} ${COPYRIGHT_HOLDER} (${REPO_URL}) -->" >> "$tmp"
        cat "$file" >> "$tmp"
    fi
    cp "$tmp" "$file"
    rm "$tmp"
    echo "  Added header: $file"
}

echo "Adding SPDX headers..."

# Rust source files
for f in $(find src benches -name '*.rs' | sort); do
    add_header_rust "$f"
done

# YAML files
for f in $(find .github -name '*.yml' -o -name '*.yaml' | sort) description.yml; do
    add_header_hash "$f"
done

# TOML files
for f in Cargo.toml deny.toml rust-toolchain.toml; do
    [ -f "$f" ] && add_header_hash "$f"
done

# Makefile
[ -f Makefile ] && add_header_hash "Makefile"

# Shell scripts
for f in $(find scripts -name '*.sh' | sort); do
    add_header_hash "$f"
done

# SQL test files
for f in $(find test -name '*.test' | sort); do
    add_header_hash "$f"
done

# HTML files
for f in $(find demo -name '*.html' | sort); do
    add_header_html "$f"
done

if $CHECK_ONLY; then
    if [ ${#MISSING[@]} -gt 0 ]; then
        echo ""
        echo "ERROR: ${#MISSING[@]} file(s) missing SPDX headers:"
        for f in "${MISSING[@]}"; do
            echo "  - $f"
        done
        exit 1
    else
        echo "All files have SPDX headers."
    fi
else
    echo ""
    echo "Done. All files have SPDX headers."
fi
