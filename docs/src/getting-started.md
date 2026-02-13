# Getting Started

This guide covers building the extension from source and loading it into DuckDB.

## Building

### Prerequisites

- Rust 1.80 or later
- A C compiler (for DuckDB system bindings)

### Compile the Extension

```bash
# Clone the repository
git clone https://github.com/tomtom215/duckdb-behavioral.git
cd duckdb-behavioral

# Build in release mode
cargo build --release
```

The loadable extension will be produced at:

- **Linux**: `target/release/libduckdb_behavioral.so`
- **macOS**: `target/release/libduckdb_behavioral.dylib`

## Loading into DuckDB

```sql
-- Load the extension from a local path
LOAD 'target/release/libduckdb_behavioral.so';
```

Or from the DuckDB CLI:

```bash
duckdb -cmd "LOAD 'target/release/libduckdb_behavioral.so';"
```

Once loaded, all seven functions are available in the current session:
`sessionize`, `retention`, `window_funnel`, `sequence_match`,
`sequence_count`, `sequence_match_events`, and `sequence_next_node`.

## Verifying the Installation

Run a minimal query to confirm the extension is loaded:

```sql
-- Sessionize: should return session ID 1
SELECT sessionize(TIMESTAMP '2024-01-01 10:00:00', INTERVAL '30 minutes')
  OVER () as session_id;

-- Retention: should return [true, false]
SELECT retention(true, false);

-- Window funnel: should return 1
SELECT window_funnel(INTERVAL '1 hour', TIMESTAMP '2024-01-01', true, false);
```

## Running Tests

The extension includes 403 unit tests and 1 doc-test:

```bash
cargo test
```

All tests run in under one second. Zero clippy warnings are enforced:

```bash
cargo clippy --all-targets
```

## Running Benchmarks

Criterion.rs benchmarks cover all functions at scales from 100 to 1 billion
elements:

```bash
# Run all benchmarks
cargo bench

# Run a specific benchmark group
cargo bench -- sessionize
cargo bench -- window_funnel
cargo bench -- sequence_match
```

Results are stored in `target/criterion/` and automatically compared against
previous runs by Criterion.

## Project Structure

```
src/
  lib.rs                  # Custom C entry point (behavioral_init_c_api)
  common/
    event.rs              # Shared Event type (16-byte bitmask)
    timestamp.rs          # Interval-to-microseconds conversion
  pattern/
    parser.rs             # Recursive descent pattern parser
    executor.rs           # NFA-based pattern matcher with fast paths
  sessionize.rs           # Session boundary tracking
  retention.rs            # Bitmask-based cohort retention
  window_funnel.rs        # Greedy forward scan with mode flags
  sequence.rs             # Pattern matching state management
  sequence_next_node.rs   # Next event value after pattern match
  ffi/
    mod.rs                # register_all_raw() dispatcher
    sessionize.rs         # Sessionize FFI callbacks
    retention.rs          # Retention FFI callbacks
    window_funnel.rs      # Window funnel FFI callbacks
    sequence.rs           # Sequence match/count FFI callbacks
    sequence_match_events.rs  # Sequence match events FFI callbacks
    sequence_next_node.rs     # Sequence next node FFI callbacks
```

For a detailed discussion of the architecture, see
[Architecture](./internals/architecture.md).
