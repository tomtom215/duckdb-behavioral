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
| **test** | Run 411 unit tests + 1 doc-test | `cargo test` |
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

### CodeQL (`codeql.yml`)

Runs GitHub's CodeQL static analysis for Rust on every push to `main`, every
pull request, and on a weekly schedule (Monday 06:00 UTC). Uses the
`security-and-quality` query suite for comprehensive coverage.

- **Triggers**: push to main, PRs, weekly cron
- **Language**: Rust
- **Action version**: `github/codeql-action` v4.32.3 (SHA-pinned)
- **Permissions**: `security-events: write` (required to upload SARIF results)

**Prerequisite — Disable Default Setup:**

This workflow uses CodeQL's "advanced setup" (explicit workflow file). GitHub
does not allow both Default Setup and advanced setup to be active simultaneously.
If Default Setup is enabled, the SARIF upload will fail with:

> `CodeQL analyses from advanced configurations cannot be processed when the default setup is enabled`

The workflow includes a pre-flight check that detects this conflict and fails
fast with actionable remediation steps. To resolve:

1. Go to **Settings → Code security → Code scanning → CodeQL analysis**
2. Click the **⋯** menu → **Disable CodeQL**
3. Or via CLI: `gh api --method PATCH repos/OWNER/REPO/code-scanning/default-setup -f state=not-configured`

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
