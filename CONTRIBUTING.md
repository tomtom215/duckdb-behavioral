# Contributing to duckdb-behavioral

Thank you for your interest in contributing. This document provides everything
you need to get started, from setup through submitting a pull request.

## Quick Start

```bash
# Clone and build
git clone --recurse-submodules https://github.com/tomtom215/duckdb-behavioral.git
cd duckdb-behavioral
cargo build

# Run all quality checks
./scripts/check.sh

# Or run checks individually
cargo test --lib                                     # 544 unit tests, no libduckdb needed
DUCKDB_DOWNLOAD_LIB=1 cargo test --all-features      # 544 unit + 35 integration + 1 doc-test
cargo clippy --all-targets --all-features            # Zero warnings required
cargo fmt -- --check                                 # Format check
```

The unit tests need nothing but a Rust toolchain. The integration tests
(`tests/extension_load.rs`, feature `e2e`) load the release library into a real
DuckDB, which links a prebuilt libduckdb: `DUCKDB_DOWNLOAD_LIB=1` makes
`libduckdb-sys` download one (cached in `target/duckdb-download/`);
alternatively set `DUCKDB_LIB_DIR` (plus `LD_LIBRARY_PATH` on Linux) to an
existing one. Never set either for release builds. `scripts/check.sh` sets the
variable for you.

## Development Requirements

- Rust 1.88+ (the project's MSRV)
- A C toolchain/linker (gcc or clang)
- Python 3 and `make` (for `make configure release`)
- DuckDB CLI v1.3.2 or later for E2E testing (CI checks v1.3.2, v1.4.4, v1.4.5, v1.5.0, v1.5.6, v2.0.0)

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
│   ├── mod.rs
│   ├── config.rs           # Error messages for per-group constant arguments and out-of-memory
│   ├── event.rs            # Shared Event type (16-byte bitmask, Copy)
│   └── timestamp.rs        # Interval-to-microseconds conversion
├── pattern/
│   ├── mod.rs
│   ├── parser.rs           # Recursive descent pattern parser
│   ├── executor.rs         # Pattern matcher: fast paths + feasibility/greedy matcher
│   └── reference_nfa.rs    # Test-only: original backtracking search (oracle)
├── sessionize.rs           # Session boundary tracking
├── retention.rs            # Bitmask-based cohort retention
├── window_funnel.rs        # Port of ClickHouse windowFunnel, mode flags
├── sequence.rs             # Pattern matching state management
├── sequence_next_node.rs   # Next event value after pattern match
├── oracle_tests.rs         # Test-only: reference and invariance checks for every state
└── ffi/
    ├── mod.rs              # register_all() dispatcher via Registrar trait
    ├── view.rs             # Safe views over DuckDB's raw handles (the only unsafe constructors)
    ├── version.rs          # behavioral_version() scalar
    └── *.rs                # Per-aggregate FFI callbacks (sessionize, retention,
                            # window_funnel, window_funnel_events, sequence,
                            # sequence_match_events, sequence_next_node)
```

**Design pattern**: Each function has a pure Rust state module (business logic)
and a corresponding FFI module (DuckDB C API registration). The state module
has zero FFI dependencies and is fully unit-testable.

## Adding a New Function

1. Create `src/new_function.rs` with state struct (`Default + Send + 'static`),
   plus `update()`, `combine_in_place()`, and `finalize()`.
2. Create `src/ffi/new_function.rs` using quack-rs builders (`FfiState<T>`,
   `VectorReader`, `AggregateFunctionSetBuilder`) and the
   `aggregate_{update,combine,finalize}_callback!` macros for the callbacks
   (they turn a panic into a SQL error).
3. Register in `src/ffi/mod.rs`.
4. Add benchmark, unit tests, `AggregateTestHarness` tests, and E2E SQL tests.

## Quality Checks

All checks must pass before submitting a PR:

| Check | Command | What it validates |
|---|---|---|
| Tests | `DUCKDB_DOWNLOAD_LIB=1 cargo test --all-features` | 544 unit tests + 35 in-process integration tests + 1 doc-test |
| Lints | `cargo clippy --all-targets --all-features` | Zero warnings (pedantic + nursery + cargo) |
| Format | `cargo fmt -- --check` | Code formatting |
| Docs | `cargo doc --no-deps` | Documentation builds without warnings |
| Docs agree with the repo | `python3 -I scripts/check-docs.py` | Test counts, the DuckDB releases CI checks, the pinned `quack-rs` and `libduckdb-sys` versions, links and anchors, and the demo page's results match their sources. Adding a test means updating every count it names |

Run all at once with `./scripts/check.sh`, or add
`--quick` to skip doc and benchmark compilation checks.

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
# Quick E2E test (builds build/release/behavioral.duckdb_extension and runs test/sql/*.test)
git submodule update --init --recursive
make configure && make release && make test_release
```

`DUCKDB_DOWNLOAD_LIB=1 cargo test --all-features` also runs `tests/extension_load.rs`, which
builds the release library, stamps it, and `LOAD`s it into an in-memory
DuckDB.

### Fuzzing

`fuzz/` holds three cargo-fuzz targets (`parse`, `sequence`, `funnel`) for the
pure-Rust core. They run weekly in `scheduled.yml`; to run one by hand:

```bash
cargo install cargo-fuzz        # once; needs a nightly toolchain
cargo +nightly fuzz run parse -- -max_total_time=60 -timeout=5 -dict=fuzz/dict.txt
```

A crashing input is saved under `fuzz/artifacts/`; turn it into a unit test
before fixing the bug.

## Pull Request Process

1. **Branch**: Create a feature branch from `main`
2. **Develop**: Make changes following the architecture and style guidelines
3. **Test**: Run `./scripts/check.sh` — all checks must pass
4. **Benchmark** (if performance-related): Include Criterion data with CIs
5. **Document**: Update docs if function signatures, test counts, or benchmarks changed
6. **Submit**: Open a PR with a clear description

### PR Checklist

- [ ] `DUCKDB_DOWNLOAD_LIB=1 cargo test --all-features` passes (all tests)
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
