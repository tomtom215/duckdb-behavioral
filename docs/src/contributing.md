# Contributing

Guidelines for contributing to `duckdb-behavioral`.

## Development Setup

### Prerequisites

- Rust 1.80+ (the project's MSRV)
- A C compiler (for DuckDB system bindings)
- DuckDB CLI v1.4.4 (for E2E testing)

### Building

```bash
git clone https://github.com/tomtom215/duckdb-behavioral.git
cd duckdb-behavioral
cargo build
```

### Running Tests

```bash
# Unit tests (411 tests + 1 doc-test, runs in <1 second)
cargo test

# Clippy (zero warnings required)
cargo clippy --all-targets

# Format check
cargo fmt -- --check
```

All three checks must pass before submitting changes.

## Code Style

### Lint Standards

The project enforces **zero clippy warnings** with pedantic, nursery, and cargo
lint groups enabled. See `Cargo.toml` `[lints.clippy]` for the full
configuration and allowed exceptions (FFI callbacks, analytics math casts).

### Architecture

- **Pure Rust core**: Business logic in top-level modules (`sessionize.rs`,
  `retention.rs`, etc.) with zero FFI dependencies.
- **FFI bridge**: DuckDB C API registration confined to `src/ffi/`. Every
  `unsafe` block has a `// SAFETY:` comment.
- **All public items documented**: Functions, structs, and modules must have
  doc comments.

### Adding a New Function

1. Create `src/new_function.rs` with state struct implementing `new()`,
   `update()`, `combine()`, `combine_in_place()`, and `finalize()`.
2. Create `src/ffi/new_function.rs` with FFI callbacks.
   - If using function sets, call `duckdb_aggregate_function_set_name` on
     **each** function in the set.
   - `combine_in_place` must propagate **all** configuration fields from
     the source state (not just events).
3. Register in `src/ffi/mod.rs` `register_all_raw()`.
4. Add `pub mod new_function;` to `src/lib.rs`.
5. Add a benchmark in `benches/`.
6. Write unit tests in the module.
7. **E2E test**: Build release, load in DuckDB CLI, verify SQL produces
   correct results.

## Testing Expectations

### Unit Tests

- Cover state lifecycle: empty state, single update, multiple updates, finalize.
- Cover edge cases: threshold boundaries, NULL handling, empty inputs.
- Cover combine correctness: empty combine, boundary detection, associativity.
- Cover the DuckDB zero-initialized target combine pattern (DuckDB creates fresh
  states and combines source states into them).
- Property-based tests (proptest) for algebraic properties where applicable.

### E2E Tests

Unit tests validate business logic in isolation. E2E tests validate the FFI
boundary, DuckDB's data chunk format, state lifecycle management, and the
extension loading mechanism. Both levels are mandatory.

```bash
# Build release
cargo build --release

# Copy and append metadata
cp target/release/libbehavioral.so /tmp/behavioral.duckdb_extension
python3 extension-ci-tools/scripts/append_extension_metadata.py \
  -l /tmp/behavioral.duckdb_extension -n behavioral \
  -p linux_amd64 -dv v1.2.0 -ev v0.1.0 --abi-type C_STRUCT \
  -o /tmp/behavioral.duckdb_extension

# Load and test
duckdb -unsigned -c "LOAD '/tmp/behavioral.duckdb_extension'; SELECT ..."
```

## Benchmark Protocol

Performance claims must follow the Session Improvement Protocol documented in
[`PERF.md`](https://github.com/tomtom215/duckdb-behavioral/blob/main/PERF.md):

1. **Baseline first**: Run `cargo bench` 3 times before any changes. Record
   baseline with confidence intervals.
2. **One optimization per commit**: Each optimization must be independently
   measurable.
3. **Non-overlapping CIs required**: Accept only when 95% confidence intervals
   do not overlap between before and after measurements.
4. **Negative results documented**: If an optimization produces overlapping CIs
   or regression, revert it and document the result honestly in PERF.md.
5. **Update documentation**: Add an optimization history entry to PERF.md and
   update the current baseline table.

### Running Benchmarks

```bash
# Run all benchmarks
cargo bench

# Run a specific group
cargo bench -- sessionize
cargo bench -- sequence_match_events

# Results stored in target/criterion/
```

## PR Process

1. Create a feature branch from `main`.
2. Make changes following the code style and testing expectations above.
3. Ensure all checks pass: `cargo test`, `cargo clippy --all-targets`,
   `cargo fmt -- --check`.
4. If performance-related, include Criterion benchmark data with confidence
   intervals.
5. Update documentation if function signatures, test counts, or benchmark
   numbers changed.
6. Submit a pull request with a clear description of changes.

## Documentation

- **CLAUDE.md**: AI assistant context. Update test counts, benchmark counts,
  session entries, and lessons learned when making significant changes.
- **PERF.md**: Performance optimization history. Follow the session protocol.
- **README.md**: Public-facing documentation. Keep performance tables and
  test counts current.
- **docs/**: GitHub Pages documentation (mdBook). Keep function signatures
  and performance claims in sync with the source code.
