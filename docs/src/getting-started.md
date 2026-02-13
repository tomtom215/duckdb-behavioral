# Getting Started

This guide walks you through installing the extension, loading it into DuckDB,
verifying it works, and running your first behavioral analysis from start to
finish.

---

## Installation

### Option 1: Build from Source

Building from source gives you the latest version and works on any platform
where Rust and DuckDB are available.

**Prerequisites:**

- Rust 1.80 or later (`rustup` recommended)
- A C compiler (gcc, clang, or MSVC -- needed for DuckDB system bindings)
- DuckDB CLI v1.4.4 (for running queries)

**Build steps:**

```bash
# Clone the repository
git clone https://github.com/tomtom215/duckdb-behavioral.git
cd duckdb-behavioral

# Build in release mode (required for loading into DuckDB)
cargo build --release
```

The loadable extension will be produced at:

- **Linux**: `target/release/libbehavioral.so`
- **macOS**: `target/release/libbehavioral.dylib`

**Preparing the extension for loading:**

DuckDB loadable extensions require metadata appended to the binary. The
repository includes the necessary tooling:

```bash
# Initialize the submodule (first time only)
git submodule update --init --recursive

# Copy the built library
cp target/release/libbehavioral.so /tmp/behavioral.duckdb_extension

# Append extension metadata
python3 extension-ci-tools/scripts/append_extension_metadata.py \
  -l /tmp/behavioral.duckdb_extension -n behavioral \
  -p linux_amd64 -dv v1.2.0 -ev v0.1.0 --abi-type C_STRUCT \
  -o /tmp/behavioral.duckdb_extension
```

> **Platform note:** Replace `linux_amd64` with your platform identifier
> (`linux_arm64`, `osx_amd64`, `osx_arm64`) and `.so` with `.dylib` on macOS.

### Option 2: Community Extension (Coming Soon)

A submission to the [DuckDB Community Extensions](https://community-extensions.duckdb.org/)
registry is in progress. Once accepted, installation will be a single command:

```sql
INSTALL behavioral FROM community;
LOAD behavioral;
```

No build tools or compilation required. Check the
[project repository](https://github.com/tomtom215/duckdb-behavioral) for the
latest status.

---

## Loading the Extension

### From the DuckDB CLI

```bash
# The -unsigned flag is required for locally-built extensions
duckdb -unsigned
```

Then inside the DuckDB prompt:

```sql
LOAD '/tmp/behavioral.duckdb_extension';
```

### One-liner

```bash
duckdb -unsigned -c "LOAD '/tmp/behavioral.duckdb_extension'; SELECT 'behavioral loaded';"
```

### From a DuckDB Client Library

Any DuckDB client (Python, Node.js, Java, etc.) can load the extension:

```python
import duckdb

conn = duckdb.connect(config={"allow_unsigned_extensions": "true"})
conn.execute("LOAD '/tmp/behavioral.duckdb_extension'")
```

Once loaded, all seven functions are available in the current session:
`sessionize`, `retention`, `window_funnel`, `sequence_match`,
`sequence_count`, `sequence_match_events`, and `sequence_next_node`.

---

## Verifying the Installation

Run these minimal queries to confirm each function category is working:

```sql
-- Sessionize: should return session ID 0
SELECT sessionize(TIMESTAMP '2024-01-01 10:00:00', INTERVAL '30 minutes')
  OVER () as session_id;

-- Retention: should return [true, false]
SELECT retention(true, false);

-- Window funnel: should return 1
SELECT window_funnel(INTERVAL '1 hour', TIMESTAMP '2024-01-01', true, false);

-- Sequence match: should return true
SELECT sequence_match('(?1).*(?2)', TIMESTAMP '2024-01-01', true, true);

-- Sequence count: should return 1
SELECT sequence_count('(?1).*(?2)', TIMESTAMP '2024-01-01', true, true);
```

You can also verify all functions registered correctly by querying DuckDB's
function catalog:

```sql
SELECT function_name FROM duckdb_functions()
WHERE function_name IN (
  'sessionize', 'retention', 'window_funnel',
  'sequence_match', 'sequence_count',
  'sequence_match_events', 'sequence_next_node'
)
GROUP BY function_name
ORDER BY function_name;
```

This should return all seven function names.

---

## Your First Analysis

This walkthrough creates sample e-commerce event data and demonstrates
four core use cases: sessions, funnels, retention, and pattern matching.

### Step 1: Create Sample Data

```sql
-- Create an events table with typical e-commerce data
CREATE TABLE events AS SELECT * FROM (VALUES
  (1, TIMESTAMP '2024-01-15 09:00:00', 'page_view',    'Home'),
  (1, TIMESTAMP '2024-01-15 09:05:00', 'page_view',    'Product'),
  (1, TIMESTAMP '2024-01-15 09:08:00', 'add_to_cart',  'Product'),
  (1, TIMESTAMP '2024-01-15 09:12:00', 'checkout',     'Cart'),
  (1, TIMESTAMP '2024-01-15 09:15:00', 'purchase',     'Checkout'),
  (2, TIMESTAMP '2024-01-15 10:00:00', 'page_view',    'Home'),
  (2, TIMESTAMP '2024-01-15 10:10:00', 'page_view',    'Product'),
  (2, TIMESTAMP '2024-01-15 10:20:00', 'add_to_cart',  'Product'),
  (2, TIMESTAMP '2024-01-15 14:00:00', 'page_view',    'Home'),
  (2, TIMESTAMP '2024-01-15 14:05:00', 'page_view',    'Product'),
  (3, TIMESTAMP '2024-01-15 11:00:00', 'page_view',    'Home'),
  (3, TIMESTAMP '2024-01-15 11:30:00', 'page_view',    'Blog'),
  (3, TIMESTAMP '2024-01-15 12:00:00', 'page_view',    'Home'),
  (3, TIMESTAMP '2024-01-16 09:00:00', 'page_view',    'Home'),
  (3, TIMESTAMP '2024-01-16 09:10:00', 'page_view',    'Product'),
  (3, TIMESTAMP '2024-01-16 09:15:00', 'add_to_cart',  'Product'),
  (3, TIMESTAMP '2024-01-16 09:20:00', 'checkout',     'Cart'),
  (3, TIMESTAMP '2024-01-16 09:25:00', 'purchase',     'Checkout')
) AS t(user_id, event_time, event_type, page);
```

### Step 2: Identify User Sessions

Break events into sessions using a 30-minute inactivity threshold:

```sql
SELECT user_id, event_time, event_type,
  sessionize(event_time, INTERVAL '30 minutes') OVER (
    PARTITION BY user_id ORDER BY event_time
  ) as session_id
FROM events
ORDER BY user_id, event_time;
```

**What to expect:** User 1 has a single session (all events within 30 minutes).
User 2 has two sessions (the 3h 40m gap between 10:20 and 14:00 starts a new
session). User 3 has three sessions (gaps between 12:00 and the next day).

### Step 3: Analyze the Conversion Funnel

Track how far each user progresses through the purchase funnel within a 1-hour
window:

```sql
SELECT user_id,
  window_funnel(INTERVAL '1 hour', event_time,
    event_type = 'page_view',
    event_type = 'add_to_cart',
    event_type = 'checkout',
    event_type = 'purchase'
  ) as furthest_step
FROM events
GROUP BY user_id
ORDER BY user_id;
```

**What to expect:**

| user_id | furthest_step | Interpretation |
|---|---|---|
| 1 | 4 | Completed all steps (page_view -> add_to_cart -> checkout -> purchase) |
| 2 | 2 | Reached add_to_cart but never checked out |
| 3 | 4 | Completed all steps (on the second day's session) |

### Step 4: Detect Purchase Patterns

Find which users viewed a product and then purchased (with any events in
between):

```sql
SELECT user_id,
  sequence_match('(?1).*(?2)', event_time,
    event_type = 'page_view',
    event_type = 'purchase'
  ) as viewed_then_purchased
FROM events
GROUP BY user_id
ORDER BY user_id;
```

**What to expect:** Users 1 and 3 return `true` (they both viewed and
purchased). User 2 returns `false` (viewed but never purchased).

### Step 5: User Journey Flow

Discover where users navigate after viewing the Home page then the Product page:

```sql
SELECT user_id,
  sequence_next_node('forward', 'first_match', event_time, page,
    page = 'Home',
    page = 'Home',
    page = 'Product'
  ) as next_page_after_product
FROM events
GROUP BY user_id
ORDER BY user_id;
```

**What to expect:** User 1 goes to Cart (add_to_cart on the Product page).
The function returns the page value of the event immediately following the
matched Home -> Product sequence.

---

## Troubleshooting

### Extension fails to load

**"file was built for DuckDB C API version '...' but we can only load extensions built for DuckDB C API '...'"**

The extension is built against DuckDB C API version `v1.2.0` (used by DuckDB
v1.4.4). You must use a DuckDB CLI version that matches. Check your version
with:

```bash
duckdb --version
```

If you see a different version, either install DuckDB v1.4.4 or rebuild the
extension against your DuckDB version (this requires updating the
`libduckdb-sys` dependency in `Cargo.toml`).

**"Extension ... is not signed!"**

DuckDB rejects unsigned extensions by default. Use one of these approaches:

```bash
# CLI flag
duckdb -unsigned

# Or set inside a session (before LOAD)
SET allow_unsigned_extensions = true;
```

```python
# Python client
conn = duckdb.connect(config={"allow_unsigned_extensions": "true"})
```

**"IO Error: Cannot open file"**

The path to the extension must be an absolute path or a path relative to the
DuckDB working directory. Verify the file exists:

```bash
ls -la /tmp/behavioral.duckdb_extension
```

If you skipped the metadata step, the file may exist but fail to load. Make
sure you ran `append_extension_metadata.py` after copying the built library.

**Platform mismatch**

An extension built on Linux cannot be loaded on macOS, and vice versa. The
extension must be built on the same platform and architecture where DuckDB is
running.

### Functions not found after loading

If `LOAD` succeeds but functions are not available, verify registration by
querying the function catalog:

```sql
SELECT function_name, function_type
FROM duckdb_functions()
WHERE function_name LIKE 'session%'
   OR function_name LIKE 'retention%'
   OR function_name LIKE 'window_funnel%'
   OR function_name LIKE 'sequence%';
```

All seven functions should appear. If some are missing, this may indicate a
version mismatch between the extension and DuckDB. Rebuild the extension from
source against the DuckDB version you are running.

### Query errors

**"Binder Error: No function matches the given name and argument types"**

This usually means the argument types do not match any registered overload.
Common causes:

- **Wrong argument order:** Each function has a specific parameter order. See
  the [Function Reference](./functions/sessionize.md) for exact signatures.
- **Using INTEGER instead of INTERVAL:** The window/gap parameter for
  `sessionize` and `window_funnel` must be a DuckDB `INTERVAL`, not an integer.
  Use `INTERVAL '1 hour'`, not `3600`.
- **Fewer than 2 boolean conditions:** All condition-based functions require at
  least 2 boolean parameters.
- **More than 32 boolean conditions:** The maximum is 32, matching ClickHouse's
  limit.

**"NULL results when expecting values"**

- Rows with NULL timestamps are silently ignored during aggregation.
- NULL boolean conditions are treated as `false`.
- `sequence_next_node` returns NULL when no pattern match is found or when no
  adjacent event exists after the match.

### Build errors

**"error: linker 'cc' not found"**

Install a C compiler. On Ubuntu/Debian: `sudo apt install build-essential`.
On macOS: `xcode-select --install`.

**"failed to run custom build command for libduckdb-sys"**

The `libduckdb-sys` crate needs a C compiler and CMake to build DuckDB from
source (for the test suite). Install CMake: `sudo apt install cmake` (Linux)
or `brew install cmake` (macOS).

---

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

---

## Next Steps

- **[Function Reference](./functions/sessionize.md)** -- detailed documentation
  for each function, including all parameters, modes, and edge case behavior
- **[FAQ](./faq.md)** -- answers to common questions about patterns, modes, NULL
  handling, and performance
- **[ClickHouse Compatibility](./internals/clickhouse-compatibility.md)** -- how
  each function maps to its ClickHouse equivalent, with syntax translation examples
- **[Contributing](./contributing.md)** -- development setup, testing
  expectations, and the PR process for contributing changes
