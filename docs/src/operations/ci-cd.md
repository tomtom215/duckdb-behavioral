# CI/CD

The project uses GitHub Actions for continuous integration, end-to-end testing,
and release management. All workflows are defined in `.github/workflows/`.

## Workflows

### CI (`ci.yml`)

Runs on every push to `main` and every pull request. 13 independent jobs
ensure code quality across multiple dimensions.

| Job | Purpose | Tool |
|-----|---------|------|
| **check** | Verify compilation | `cargo check --all-targets` |
| **test** | Run 403 unit tests + 1 doc-test | `cargo test` |
| **clippy** | Zero-warning lint enforcement | `cargo clippy` with `-D warnings` |
| **fmt** | Formatting verification | `cargo fmt --check` |
| **doc** | Documentation builds without warnings | `cargo doc` with `-Dwarnings` |
| **msrv** | Minimum Supported Rust Version (1.80) | `cargo check` with pinned toolchain |
| **bench-compile** | Benchmarks compile (no execution) | `cargo bench --no-run` |
| **deny** | License, advisory, and source auditing | `cargo-deny` |
| **semver** | Semver compatibility check | `cargo-semver-checks` |
| **coverage** | Code coverage reporting | `cargo-tarpaulin` + Codecov |
| **cross-platform** | Linux + macOS test matrix | `cargo test` on both OSes |
| **extension-build** | Community extension packaging | `make configure && make release` |

### E2E Tests (`e2e.yml`)

Runs on every push to `main` and every pull request. Builds the extension
using the community extension Makefile and tests it against a real DuckDB
instance.

**Test coverage:**
- Extension loading verification
- All 7 functions (sessionize, retention, window_funnel, sequence_match,
  sequence_count, sequence_match_events, sequence_next_node)
- Mode parameters (strict_increase)
- GROUP BY aggregation
- Load test with 100K events across all aggregate functions
- No-match and empty-input edge cases

**Platforms tested:** Linux x86_64, macOS ARM64

### Release (`release.yml`)

Triggered on git tag push (`v*`) or manual dispatch. Builds the extension
for 4 platform targets, runs SQL integration tests, and creates a GitHub
release with SHA256 checksums and build provenance attestations.

**Build targets:**
| Platform | Runner | Target |
|----------|--------|--------|
| Linux x86_64 | ubuntu-latest | `x86_64-unknown-linux-gnu` |
| Linux ARM64 | ubuntu-22.04 | `aarch64-unknown-linux-gnu` (cross-compiled) |
| macOS x86_64 | macos-latest | `x86_64-apple-darwin` |
| macOS ARM64 | macos-latest | `aarch64-apple-darwin` |

**Supply chain security:**
- SHA256 checksums for all artifacts
- GitHub artifact attestation via `actions/attest-build-provenance@v2`
- Immutable artifacts with 30-day retention
- Build provenance tied to specific git commit

### Pages (`pages.yml`)

Deploys mdBook documentation to GitHub Pages on push to `main`. Uses
mdBook v0.4.40 with custom CSS styling.

## Reproducing CI Locally

```bash
# Run the same checks as CI
cargo check --all-targets
cargo test --all-targets && cargo test --doc
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
RUSTDOCFLAGS=-Dwarnings cargo doc --no-deps --document-private-items
cargo deny check advisories bans licenses sources

# Build extension (requires submodule)
git submodule update --init
make configure
make release

# Run SQL integration tests
make test_release
```
