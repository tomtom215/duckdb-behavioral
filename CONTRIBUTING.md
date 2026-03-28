# Contributing to duckdb-behavioral

Thank you for your interest in contributing. This document provides everything
you need to get started, from setup through submitting a pull request.

## Quick Start

```bash
# Clone and build
git clone https://github.com/tomtom215/duckdb-behavioral.git
cd duckdb-behavioral
cargo build

# Run all quality checks
./scripts/check.sh

# Or run checks individually
cargo test                     # 453 unit tests + 1 doc-test
cargo clippy --all-targets     # Zero warnings required
cargo fmt -- --check           # Format check
```

## Development Requirements

- Rust 1.84.1+ (the project's MSRV)
- A C compiler (for DuckDB system bindings)
- DuckDB CLI v1.5.1 (for E2E testing)

## Key Principles

1. **Pure Rust core**: Business logic has zero FFI dependencies. All `unsafe`
   code is confined to `src/ffi/`.
2. **Test everything**: Unit tests for state lifecycle, edge cases, and combine
   correctness. E2E tests against real DuckDB for FFI validation.
3. **Measure performance**: Performance claims require Criterion.rs benchmarks
   with 95% confidence intervals and non-overlapping CIs between before/after.
4. **Document honestly**: Negative results are documented with the same rigor
   as positive results.

## Project Architecture

```
src/
├── lib.rs                  # Entry point (quack_rs::entry_point_v2! macro)
├── common/
│   ├── event.rs            # Shared Event type (16-byte bitmask, Copy)
│   └── timestamp.rs        # Interval-to-microseconds conversion
├── pattern/
│   ├── parser.rs           # Recursive descent pattern parser
│   └── executor.rs         # NFA-based pattern matcher
├── sessionize.rs           # Session boundary tracking
├── retention.rs            # Bitmask-based cohort retention
├── window_funnel.rs        # Greedy forward scan with mode flags
├── sequence.rs             # Pattern matching state management
├── sequence_next_node.rs   # Next event value after pattern match
└── ffi/
    ├── mod.rs              # register_all() dispatcher via Registrar trait
    └── *.rs                # Per-function FFI callbacks
```

**Design pattern**: Each function has a pure Rust state module (business logic)
and a corresponding FFI module (DuckDB C API registration). The state module
has zero FFI dependencies and is fully unit-testable.

## Adding a New Function

1. Create `src/new_function.rs` with state struct (`Default + Send + 'static`),
   plus `update()`, `combine_in_place()`, and `finalize()`.
2. Create `src/ffi/new_function.rs` using quack-rs builders (`FfiState<T>`,
   `VectorReader`, `AggregateFunctionSetBuilder`).
3. Register in `src/ffi/mod.rs`.
4. Add benchmark, unit tests, `AggregateTestHarness` tests, and E2E SQL tests.

## Quality Checks

All checks must pass before submitting a PR:

| Check | Command | What it validates |
|---|---|---|
| Tests | `cargo test` | 453 unit tests + 1 doc-test |
| Lints | `cargo clippy --all-targets` | Zero warnings (pedantic + nursery + cargo) |
| Format | `cargo fmt -- --check` | Code formatting |
| Docs | `cargo doc --no-deps` | Documentation builds without warnings |

Run all at once with `./scripts/check.sh` or `./scripts/check.sh --quick` to
skip doc and benchmark compilation checks.

## Testing Expectations

### Unit Tests

- State lifecycle: empty state, single update, multiple updates, finalize
- Edge cases: threshold boundaries, NULL handling, empty inputs
- Combine correctness: empty combine, boundary detection, associativity
- Config propagation: DuckDB creates zero-initialized target states, then
  combines source states into them — all config fields must be propagated
- Property-based tests (proptest) for algebraic properties where applicable

### E2E Tests

Unit tests validate business logic. E2E tests validate the FFI boundary,
DuckDB's data chunk format, and the extension loading mechanism. Both are
mandatory.

```bash
# Quick E2E test
cargo build --release
git submodule update --init
make configure && make release && make test_release
```

## Pull Request Process

1. **Branch**: Create a feature branch from `main`
2. **Develop**: Make changes following the architecture and style guidelines
3. **Test**: Run `./scripts/check.sh` — all checks must pass
4. **Benchmark** (if performance-related): Include Criterion data with CIs
5. **Document**: Update docs if function signatures, test counts, or benchmarks changed
6. **Submit**: Open a PR with a clear description

### PR Checklist

- [ ] `cargo test` passes (all tests)
- [ ] `cargo clippy --all-targets` produces zero warnings
- [ ] `cargo fmt -- --check` passes
- [ ] E2E test against real DuckDB (if FFI or registration changes)
- [ ] Benchmarks checked for regressions (if performance-sensitive)
- [ ] Documentation updated (if public API changed)

## Benchmark Protocol

Performance claims must follow the protocol in [`PERF.md`](PERF.md):

1. **Baseline first**: Run `cargo bench` 3 times before changes
2. **One optimization per commit**: Each must be independently measurable
3. **Non-overlapping CIs required**: Accept only when 95% CIs don't overlap
4. **Document negative results**: If an optimization regresses, revert and
   document it in PERF.md

## Documentation Files

| File | Purpose | When to update |
|---|---|---|
| `CLAUDE.md` | AI assistant context | Test counts, benchmark numbers, architecture changes |
| `PERF.md` | Performance history | Any optimization session |
| `README.md` | Public-facing docs | Performance tables, test counts, function signatures |
| `CHANGELOG.md` | Release notes | Any user-visible change |
| `docs/` | mdBook documentation | Function docs, examples, architecture |

## License

By contributing, you agree that your contributions will be licensed under the
MIT License.
