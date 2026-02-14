# Contributing to duckdb-behavioral

Thank you for your interest in contributing. This document provides a quick
overview of how to get started. For the full guide, see the
[Contributing](https://tomtom215.github.io/duckdb-behavioral/contributing.html)
page in the documentation.

## Quick Start

```bash
# Clone and build
git clone https://github.com/tomtom215/duckdb-behavioral.git
cd duckdb-behavioral
cargo build

# Run all checks (all must pass)
cargo test                     # 434 unit tests + 1 doc-test
cargo clippy --all-targets     # Zero warnings required
cargo fmt -- --check           # Format check
```

## Development Requirements

- Rust 1.80+ (the project's MSRV)
- A C compiler (for DuckDB system bindings)
- DuckDB CLI v1.4.4 (for E2E testing)

## Key Principles

1. **Pure Rust core**: Business logic has zero FFI dependencies. All `unsafe`
   code is confined to `src/ffi/`.
2. **Test everything**: Unit tests for state lifecycle, edge cases, and combine
   correctness. E2E tests against real DuckDB for FFI validation.
3. **Measure performance**: Performance claims require Criterion.rs benchmarks
   with 95% confidence intervals and non-overlapping CIs between before/after.
4. **Document honestly**: Negative results are documented with the same rigor
   as positive results.

## Adding a New Function

1. Create `src/new_function.rs` with `new()`, `update()`, `combine()`,
   `combine_in_place()`, and `finalize()`.
2. Create `src/ffi/new_function.rs` with FFI callbacks.
3. Register in `src/ffi/mod.rs`.
4. Add benchmark, unit tests, and E2E SQL tests.

See the full [Contributing Guide](https://tomtom215.github.io/duckdb-behavioral/contributing.html)
for detailed expectations on testing, benchmarking, and the PR process.

## License

By contributing, you agree that your contributions will be licensed under the
MIT License.
