# Security Policy

## Reporting a Vulnerability

If you discover a security vulnerability in this project, please report it via
[GitHub Issues](https://github.com/tomtom215/duckdb-behavioral/issues).

For sensitive disclosures, please use a private advisory via GitHub's
[Security Advisory](https://github.com/tomtom215/duckdb-behavioral/security/advisories/new)
feature.

## Security Model

### Attack Surface

This extension operates within DuckDB's process and has no independent attack
surface beyond what DuckDB itself exposes. Specifically:

- **No network access** -- the extension makes zero network calls at runtime
- **No file system access** -- all state is managed in-memory through DuckDB's
  aggregate function lifecycle
- **No dynamic code execution** -- pattern strings are parsed by a
  deterministic recursive descent parser with no eval-like behavior

### Unsafe Code Confinement

All `unsafe` code is confined to `src/ffi/` (6 files). Business logic in
`src/sessionize.rs`, `src/retention.rs`, `src/window_funnel.rs`,
`src/sequence.rs`, `src/sequence_next_node.rs`, and `src/pattern/` is 100%
safe Rust. Every `unsafe` block has a `// SAFETY:` documentation comment.

### Dependency Audit

The extension has exactly **one** runtime dependency (`libduckdb-sys = "=1.4.4"`,
pinned exactly). All dependencies are audited via `cargo-deny` in CI for known
advisories and license compliance.

### Build Integrity

Release builds use deterministic settings (LTO, single codegen unit, `Cargo.lock`
committed). Release artifacts include SHA256 checksums and GitHub artifact
attestations via `actions/attest-build-provenance`.

For the full security model, see the
[Security & Supply Chain](https://tomtom215.github.io/duckdb-behavioral/operations/security.html)
documentation.

## Supported Versions

| Version | Supported |
|---------|-----------|
| 0.2.x   | Yes       |
| 0.1.x   | Yes       |
