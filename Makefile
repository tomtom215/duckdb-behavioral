# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

.PHONY: clean clean_all

PROJ_DIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))

EXTENSION_NAME=behavioral

# Stable C API: the extension only calls functions in the stable prefix of
# DuckDB's `duckdb_ext_api_v1` struct (verified by disassembling the release
# build: highest slot used is 306 of the 357-slot stable prefix). The binary is
# therefore stamped `C_STRUCT` and loads into every DuckDB release whose C API
# is at least TARGET_DUCKDB_VERSION, instead of being pinned to one release as a
# `USE_UNSTABLE_C_API=1` (`C_STRUCT_UNSTABLE`) build is.
#
# TARGET_DUCKDB_VERSION is the C API version quack-rs requests at load
# (`quack_rs::DUCKDB_API_VERSION`), not a DuckDB release.
TARGET_DUCKDB_VERSION=v1.2.0

# DuckDB release the SQL logic tests run against (extension-ci-tools defaults to
# the latest release on PyPI). Matches the libduckdb-sys pin in Cargo.toml
# (=1.10506.0 -> DuckDB v1.5.6).
DUCKDB_TEST_VERSION=1.5.6

all: configure debug

# Include makefiles from DuckDB extension-ci-tools
include extension-ci-tools/makefiles/c_api_extensions/base.Makefile
include extension-ci-tools/makefiles/c_api_extensions/rust.Makefile

configure: venv platform extension_version

debug: build_extension_library_debug build_extension_with_metadata_debug
release: build_extension_library_release build_extension_with_metadata_release

test: test_debug
test_debug: test_extension_debug
test_release: test_extension_release

clean: clean_build clean_rust
clean_all: clean_configure clean
