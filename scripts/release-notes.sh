#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)
# =============================================================================
# release-notes.sh — print the CHANGELOG.md section of one version.
#
# Usage:
#   scripts/release-notes.sh <version>      # e.g. 0.11.0 (a leading v is fine)
#
# Prints the lines between `## [<version>]` and the next `## [` heading.
# Exits non-zero if the version has no section or the section is empty, so a
# release cannot be cut without notes.
# =============================================================================

set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "usage: $0 <version>" >&2
    exit 2
fi
version="${1#v}"
changelog="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/CHANGELOG.md"

notes="$(awk -v heading="## [${version}]" '
    index($0, heading) == 1 { found = 1; next }
    found && /^## \[/        { exit }
    found                   { print }
' "$changelog")"

if [[ -z "${notes//[[:space:]]/}" ]]; then
    echo "CHANGELOG.md has no non-empty '## [${version}]' section" >&2
    exit 1
fi
printf '%s\n' "$notes"
