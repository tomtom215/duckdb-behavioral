# Getting Started

This guide walks you through installing the extension, loading it into DuckDB,
verifying it works, and running your first behavioral analysis from start to
finish.

---

## Installation

### Option 1: Community Extension (Recommended)

The extension is listed in the
[DuckDB Community Extensions](https://github.com/duckdb/community-extensions)
repository. Install with a single command:

```sql
INSTALL behavioral FROM community;
LOAD behavioral;
```

No build tools, compilation, or `-unsigned` flag required. This works with any
DuckDB client (CLI, Python, Node.js, Java, etc.).

### Option 2: Build from Source

Building from source gives you the latest development version and works on any
platform where Rust and DuckDB are available.

**Prerequisites:**

- Rust 1.87 or later (`rustup` recommended)
- A C toolchain/linker (gcc or clang) for linking the `cdylib`
- Python 3 and `make` (for `make configure release`, which stamps the metadata footer)
- DuckDB CLI v1.3.2 or later (for running queries; CI checks v1.3.2, v1.4.4, v1.5.0 and v1.5.6)

**Build steps:**

```bash
# Clone the repository (with the extension-ci-tools submodule)
git clone --recurse-submodules https://github.com/tomtom215/duckdb-behavioral.git
cd duckdb-behavioral

# Build in release mode and append the DuckDB metadata footer
make configure release
```

This produces `build/release/behavioral.duckdb_extension`, ready to `LOAD`.

`cargo build --release` alone produces `target/release/libbehavioral.so`
(`.dylib` on macOS), which DuckDB refuses to load: it only accepts files ending
in `.duckdb_extension` that carry the metadata footer. To stamp the footer by
hand instead of using `make`:

```bash
# Initialize the submodule (first time only)
git submodule update --init --recursive

# Copy the built library
cp target/release/libbehavioral.so /tmp/behavioral.duckdb_extension

# Append extension metadata
python3 extension-ci-tools/scripts/append_extension_metadata.py \
  -l /tmp/behavioral.duckdb_extension -n behavioral \
  -p linux_amd64 -dv v1.2.0 -ev v0.10.0 \
  -o /tmp/behavioral.duckdb_extension
```

> **Platform note:** Replace `linux_amd64` with your platform identifier
> (`linux_arm64`, `osx_amd64`, `osx_arm64`) and `.so` with `.dylib` on macOS.
>
> **Version note:** `-dv v1.2.0` is the DuckDB *C API* version, not a DuckDB
> release. The extension uses only the stable C API, so the default ABI type
> (`C_STRUCT`) applies and one build loads into any DuckDB release whose C API
> is v1.2.0 or newer. Do not pass `--abi-type C_STRUCT_UNSTABLE`: that pins the
> binary to the single release named by `-dv`.

---

## Loading the Extension

### From the Community Extension (Recommended)

```sql
-- No special flags needed
INSTALL behavioral FROM community;
LOAD behavioral;
```

This works in any DuckDB client:

```python
import duckdb

conn = duckdb.connect()
conn.execute("INSTALL behavioral FROM community")
conn.execute("LOAD behavioral")
```

### From a Local Build

```bash
# The -unsigned flag is required for locally-built extensions
duckdb -unsigned
```

Then inside the DuckDB prompt:

```sql
LOAD 'build/release/behavioral.duckdb_extension';  -- from `make configure release`
-- or, if you stamped the footer by hand:
LOAD '/tmp/behavioral.duckdb_extension';
```

#### One-liner

```bash
duckdb -unsigned -c "LOAD 'build/release/behavioral.duckdb_extension'; SELECT behavioral_version();"
```

#### From a DuckDB Client Library

```python
import duckdb

conn = duckdb.connect(config={"allow_unsigned_extensions": "true"})
conn.execute("LOAD '/tmp/behavioral.duckdb_extension'")
```

Once loaded, all eight aggregate functions are available in the current
session: `sessionize`, `retention`, `window_funnel`, `window_funnel_events`,
`sequence_match`, `sequence_count`, `sequence_match_events`, and
`sequence_next_node`, plus the `behavioral_version()` scalar.

---

## Verifying the Installation

Run these minimal queries to confirm each function category is working:

```sql
-- Sessionize: should return session IDs 1, 1, 2 (the 50-minute gap starts a new session)
SELECT ts, sessionize(ts, INTERVAL '30 minutes') OVER (ORDER BY ts) AS session_id
FROM (VALUES (TIMESTAMP '2024-01-01 10:00:00'),
             (TIMESTAMP '2024-01-01 10:10:00'),
             (TIMESTAMP '2024-01-01 11:00:00')) AS t(ts);

-- Retention: should return [true, false]
SELECT retention(true, false);

-- Window funnel: should return 1
SELECT window_funnel(INTERVAL '1 hour', TIMESTAMP '2024-01-01', true, false);

-- Sequence match / count over two events: should return true and 1
SELECT sequence_match('(?1).*(?2)', ts, a, b) AS matched,
       sequence_count('(?1).*(?2)', ts, a, b) AS cnt
FROM (VALUES (TIMESTAMP '2024-01-01 10:00:00', true, false),
             (TIMESTAMP '2024-01-01 10:05:00', false, true)) AS t(ts, a, b);
```

A single event cannot fill two pattern steps, so
`sequence_match('(?1).*(?2)', TIMESTAMP '2024-01-01', true, true)` returns
`false` (and `sequence_count` returns `0`).

You can also verify all functions registered correctly by querying DuckDB's
function catalog:

```sql
SELECT function_name FROM duckdb_functions()
WHERE function_name IN (
  'sessionize', 'retention', 'window_funnel', 'window_funnel_events',
  'sequence_match', 'sequence_count',
  'sequence_match_events', 'sequence_next_node', 'behavioral_version'
)
GROUP BY function_name
ORDER BY function_name;
```

This should return all nine function names (eight analytics functions plus
the `behavioral_version()` diagnostic scalar). `SELECT behavioral_version();`
tells you which build is loaded.

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
session). User 3 has two sessions: the three events on Jan 15 are 30 minutes
apart, which does not exceed the threshold, and the overnight gap before
Jan 16 09:00 starts session 2.

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

**What to expect:**

| user_id | next_page_after_product |
|---|---|
| 1 | Product |
| 2 | Product |
| 3 | NULL |

The function returns the `page` value of the event immediately following the
matched Home -> Product sequence. For users 1 and 2 that is the add_to_cart
event, which happened on the Product page. With `'first_match'`, the anchor is
the first event satisfying the base condition and `event1` (the first Home
view); the chain must match consecutive events. User 3's first Home view is
followed by Blog, so the chain fails, and it is not retried at the later Home
view: the result is NULL.

---

## Troubleshooting

### Extension fails to load

**"file was built for DuckDB C API version '...' but we can only load extensions built for DuckDB C API '...'"**

The extension declares the minimum DuckDB C API version it needs (`v1.2.0`);
your DuckDB is older than that. Check your version with:

```bash
duckdb --version
```

**"The file was built specifically for DuckDB version '...'"**

The binary was stamped `C_STRUCT_UNSTABLE`, which pins it to one DuckDB
release. Re-append the metadata as shown above, without `--abi-type`.

**"... could not be loaded because its signature is either missing or invalid and unsigned extensions are disabled by configuration."**

This only applies to locally-built extensions. If you installed via
`INSTALL behavioral FROM community`, the extension is already signed and
this error should not occur.

For locally-built extensions, DuckDB rejects unsigned extensions by default.
Use one of these approaches:

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

**"IO Error: Extension ... not found."**

The path to the extension must be an absolute path or a path relative to the
DuckDB working directory. Verify the file exists:

```bash
ls -la build/release/behavioral.duckdb_extension
```

**"DuckDB extensions are files ending with '.duckdb_extension', loading different files is not possible"**

You pointed `LOAD` at the raw `libbehavioral.so`/`.dylib`. Load the stamped
`build/release/behavioral.duckdb_extension` from `make configure release`, or
copy the library and run `append_extension_metadata.py` as shown above.

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

All eight aggregate functions should appear. If some are missing, check that
you loaded the current build (`SELECT behavioral_version();`) and that your
DuckDB is v1.3.2 or newer.

### Query errors

**"Binder Error: No function matches the given name and argument types"**

This usually means the argument types do not match any registered overload.
Common causes:

- **Wrong argument order:** Each function has a specific parameter order. See
  the [Function Reference](./functions/sessionize.md) for exact signatures.
- **Using INTEGER instead of INTERVAL:** The window/gap parameter for
  `sessionize` and `window_funnel` must be a DuckDB `INTERVAL`, not an integer.
  Use `INTERVAL '1 hour'`, not `3600`.
- **Too few boolean conditions:** `retention`, `sequence_match`,
  `sequence_count`, and `sequence_match_events` require at least 2;
  `window_funnel` and `window_funnel_events` at least 1; `sequence_next_node`
  a base condition plus at least 1 event condition.
- **More than 32 conditions:** The maximum is 32 (for `sequence_next_node`,
  32 event conditions after the base condition).

**"NULL results when expecting values"**

- Rows with NULL timestamps are silently ignored during aggregation.
- NULL boolean conditions are treated as `false`.
- `sequence_next_node` returns NULL when no pattern match is found or when no
  adjacent event exists after the match.

### Build errors

**"error: linker 'cc' not found"**

Install a C compiler. On Ubuntu/Debian: `sudo apt install build-essential`.
On macOS: `xcode-select --install`.

**Test build fails to link against libduckdb**

The `duckdb` dev-dependency is built without the `bundled` feature, so a bare
`cargo test` has no DuckDB library to link. Set `DUCKDB_DOWNLOAD_LIB=1` to have
`libduckdb-sys` download a prebuilt libduckdb (cached in
`target/duckdb-download/`), or point `DUCKDB_LIB_DIR` (plus `LD_LIBRARY_PATH`
on Linux) at an existing one. Neither is needed for `cargo build --release`.

---

## Running Tests

The extension includes 545 unit tests, 28 in-process integration tests
(`tests/extension_load.rs`), and 1 doc-test:

```bash
DUCKDB_DOWNLOAD_LIB=1 cargo test
```

The integration tests build the release `cdylib`, stamp it, and `LOAD` it into
an in-memory DuckDB, so a cold run takes about 20 seconds. Zero clippy
warnings are enforced:

```bash
cargo clippy --all-targets
```

## Running Benchmarks

Criterion.rs benchmarks cover the Rust state of every aggregate function, at
scales from 100 elements up to 1 billion (`sessionize_update`), 100 million
(most others), or 10 million (`sequence_next_node`):

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
  lib.rs                  # Entry point via quack_rs::entry_point_v2! macro
  common/
    mod.rs
    event.rs              # Shared Event type (16-byte bitmask)
    timestamp.rs          # Interval-to-microseconds conversion
  pattern/
    mod.rs
    parser.rs             # Recursive descent pattern parser
    executor.rs           # Pattern matcher: fast paths + feasibility/greedy matcher
    reference_nfa.rs      # Test-only: original backtracking search (oracle)
  sessionize.rs           # Session boundary tracking
  retention.rs            # Bitmask-based cohort retention
  window_funnel.rs        # Port of ClickHouse windowFunnel, mode flags
  sequence.rs             # Pattern matching state management
  sequence_next_node.rs   # Next event value after pattern match
  ffi/
    mod.rs                # register_all() dispatcher
    sessionize.rs         # Sessionize FFI callbacks
    retention.rs          # Retention FFI callbacks
    window_funnel.rs      # Window funnel FFI callbacks
    window_funnel_events.rs   # Window funnel events FFI callbacks
    sequence.rs           # Sequence match/count FFI callbacks
    sequence_match_events.rs  # Sequence match events FFI callbacks
    sequence_next_node.rs     # Sequence next node FFI callbacks
    version.rs            # behavioral_version() scalar
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
