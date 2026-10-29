# Security & Supply Chain

This page documents the security model and supply chain integrity measures
for the duckdb-behavioral extension.

## Versioning

This project follows [Semantic Versioning](https://semver.org/) (SemVer).

**Version format:** `MAJOR.MINOR.PATCH` (e.g., `0.1.0`, `1.0.0`, `1.2.3`)

| Component | Bumped when |
|-----------|-------------|
| MAJOR | Breaking changes to SQL function signatures, return types, or behavior |
| MINOR | New functions, new modes, new pattern syntax operators |
| PATCH | Bug fixes, performance improvements, documentation updates |

**Pre-1.0 policy:** While the version is `0.x.y`, MINOR bumps may include
breaking changes per [SemVer section 4](https://semver.org/#spec-item-4).

**Version sources:** The version is maintained in three files that must
stay in sync:
- `Cargo.toml` (`version = "X.Y.Z"`)
- `description.yml` (`version: X.Y.Z`)
- Git tag (`vX.Y.Z`)

The release workflow validates that all three match before building.

## Dependency Audit

### Runtime Dependencies

The extension has exactly **three** runtime dependencies:

| Crate | Version | Purpose |
|-------|---------|---------|
| `quack-rs` | `=0.19.0` | Rust SDK for DuckDB loadable extensions — entry point macro, aggregate builders, safe state management, vector I/O |
| `libduckdb-sys` | `=1.20000.1` | DuckDB C API bindings (re-exported by quack-rs; pinned explicitly because the FFI callbacks use its raw types) |
| `smallvec` | `1` | Inline storage for the first two events of a group, so the many tiny groups of a typical query make no heap allocation (locked by `Cargo.lock`) |

`quack-rs` and `libduckdb-sys` are **pinned exactly** to prevent silent updates.
`quack-rs` provides `entry_point_v2!`, `Connection`/`Registrar` trait,
`AggregateFunctionSetBuilder`, `FfiState<T>`, `VectorReader`/`VectorWriter`,
the `aggregate_{update,combine,finalize}_callback!` macros (panic guard that
reports a panic as a SQL error), and `AggregateTestHarness`.
`libduckdb-sys` with the `loadable-extension` feature provides runtime
function pointer stubs via global atomic statics.

### Dev-Only Dependencies

These are used only by tests and benchmarks and are NOT linked into the
release extension binary:

| Crate | Purpose |
|-------|---------|
| `quack-rs` (`bundled-test-prebuilt`, feature `e2e`) | `InMemoryDb` for the in-process extension-load integration test; it brings in the `duckdb` crate, which links a prebuilt libduckdb (`DUCKDB_DOWNLOAD_LIB=1` or `DUCKDB_LIB_DIR`). Off by default: `cargo test --lib` builds without it |
| `criterion` | Benchmarking framework |
| `proptest` | Property-based testing |

### License Compliance

All dependencies are audited via `cargo-deny` in CI. The allow list in
`deny.toml`:

```
Allowed licenses: MIT, Apache-2.0, BSD-3-Clause, CDLA-Permissive-2.0,
ISC, Unicode-3.0, Zlib
```

One crate-scoped exception allows `CC0-1.0` for `tiny-keccak`, a lockfile-only
transitive entry that is not part of any buildable dependency graph.

## Build Integrity

### Reproducible Builds

Release builds use the following Cargo profile for deterministic output:

```toml
[profile.release]
opt-level = 3
lto = true          # Full link-time optimization
codegen-units = 1   # Single codegen unit
panic = "unwind"    # Required: quack-rs turns callback panics into SQL errors
strip = true        # Strip debug symbols
```

Using `codegen-units = 1` and `lto = true` reduces non-determinism from
parallel compilation. The `Cargo.lock` file is committed to the repository
to pin all transitive dependency versions.

### Build Provenance

Release artifacts include:

1. **SHA256 checksums** (`SHA256SUMS.txt`) for every release artifact
2. **GitHub artifact attestations** via `actions/attest-build-provenance` v4.1.0 (SHA-pinned),
   which provides a cryptographic link between the artifact and the GitHub
   Actions build that produced it
3. **Immutable build logs** in GitHub Actions with full command output

### Verification

```bash
# Verify downloaded artifact checksum
sha256sum -c SHA256SUMS.txt

# Verify GitHub attestation (requires gh CLI)
gh attestation verify behavioral-v0.10.1-linux_amd64.tar.gz \
  --repo tomtom215/duckdb-behavioral
```

## Code Safety

### Unsafe Code Confinement

All hand-written unsafe code is confined to the 10 files in `src/ffi/` (the
`quack_rs::entry_point_v2!` macro in `src/lib.rs` generates the
`extern "C"` entry point). Business logic in
`src/sessionize.rs`, `src/retention.rs`, `src/window_funnel.rs`,
`src/sequence.rs`, `src/sequence_next_node.rs`, `src/common/`, and
`src/pattern/` is 100% safe Rust.

The lints enforce it. `unsafe_code = "deny"` in `Cargo.toml` covers every
module except `ffi`, and `clippy::undocumented_unsafe_blocks` and
`clippy::multiple_unsafe_ops_per_block` are denied crate-wide, so every
`unsafe` block is a single operation with a `// SAFETY:` note explaining why
the invariants are upheld. The aggregate callbacks do not touch raw pointers
themselves: each raw handle DuckDB passes becomes a type in `src/ffi/view.rs`
(`Chunk` and its typed columns, `States<T>`, `Output<K>`) through one
documented `unsafe` constructor, and the methods that read or write rows check
the row index. The update, combine, and finalize callbacks are generated by
quack-rs macros that catch panics and report them as SQL errors; this relies
on `panic = "unwind"` in the release profile.

### No Network Access

The extension makes zero network calls at runtime. It operates purely on
data provided by DuckDB through the C API.

### No File System Access

The extension does not read from or write to the file system. All state is
managed in-memory through DuckDB's aggregate function lifecycle.

## Extension Loading Security

DuckDB requires the `-unsigned` flag (or `allow_unsigned_extensions`) to load
locally-built extensions, and only loads files ending in `.duckdb_extension`
that carry the metadata footer (`make configure release` produces
`build/release/behavioral.duckdb_extension`).
The community extension distribution path uses DuckDB's built-in extension
signing and verification system, which the build system handles automatically.

## Reporting Vulnerabilities

Report a suspected vulnerability privately, through a
[GitHub Security Advisory](https://github.com/tomtom215/duckdb-behavioral/security/advisories/new).
Do not open a public issue for it. A defect that is not a vulnerability (a
wrong result, a crash on valid input) belongs in a normal
[issue](https://github.com/tomtom215/duckdb-behavioral/issues).
