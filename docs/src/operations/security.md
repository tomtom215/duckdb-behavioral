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

The extension has exactly **one** runtime dependency:

| Crate | Version | Purpose |
|-------|---------|---------|
| `libduckdb-sys` | `=1.4.4` | DuckDB C API bindings for aggregate function registration |

The version is **pinned exactly** (`=1.4.4`) to prevent silent dependency
updates. The `loadable-extension` feature provides runtime function pointer
stubs via global atomic statics.

### Dev-Only Dependencies

These are used only in `#[cfg(test)]` modules and are NOT linked into the
release extension binary:

| Crate | Purpose |
|-------|---------|
| `duckdb` | In-memory connection for unit tests |
| `criterion` | Benchmarking framework |
| `proptest` | Property-based testing |
| `rand` | Random data generation for benchmarks |

### License Compliance

All dependencies are audited via `cargo-deny` in CI:

```
Allowed licenses: MIT, Apache-2.0, BSD-2-Clause, BSD-3-Clause,
CC0-1.0, CDLA-Permissive-2.0, ISC, Unicode-3.0, Zlib
```

## Build Integrity

### Reproducible Builds

Release builds use the following Cargo profile for deterministic output:

```toml
[profile.release]
opt-level = 3
lto = true          # Full link-time optimization
codegen-units = 1   # Single codegen unit
panic = "abort"     # No unwinding overhead
strip = true        # Strip debug symbols
```

Using `codegen-units = 1` and `lto = true` reduces non-determinism from
parallel compilation. The `Cargo.lock` file is committed to the repository
to pin all transitive dependency versions.

### Build Provenance

Release artifacts include:

1. **SHA256 checksums** (`SHA256SUMS.txt`) for every release artifact
2. **GitHub artifact attestations** via `actions/attest-build-provenance@v2`,
   which provides a cryptographic link between the artifact and the GitHub
   Actions build that produced it
3. **Immutable build logs** in GitHub Actions with full command output

### Verification

```bash
# Verify downloaded artifact checksum
sha256sum -c SHA256SUMS.txt

# Verify GitHub attestation (requires gh CLI)
gh attestation verify behavioral-v0.1.0-linux_amd64.tar.gz \
  --repo tomtom215/duckdb-behavioral
```

## Code Safety

### Unsafe Code Confinement

All unsafe code is confined to `src/ffi/` modules. Business logic in
`src/sessionize.rs`, `src/retention.rs`, `src/window_funnel.rs`,
`src/sequence.rs`, `src/sequence_next_node.rs`, and `src/pattern/` is
100% safe Rust.

Every `unsafe` block has a `// SAFETY:` documentation comment explaining
why the invariants are upheld.

### No Network Access

The extension makes zero network calls at runtime. It operates purely on
data provided by DuckDB through the C API.

### No File System Access

The extension does not read from or write to the file system. All state is
managed in-memory through DuckDB's aggregate function lifecycle.

## Extension Loading Security

DuckDB requires the `-unsigned` flag to load locally-built extensions.
The community extension distribution path uses DuckDB's built-in extension
signing and verification system, which the build system handles automatically.

## Reporting Vulnerabilities

Report security issues via GitHub Issues at
[github.com/tomtom215/duckdb-behavioral/issues](https://github.com/tomtom215/duckdb-behavioral/issues).
