# CI/CD

The project uses GitHub Actions for continuous integration, end-to-end testing,
and release management. All workflows are defined in `.github/workflows/`; the
toolchain, cache, and prebuilt-libduckdb handling every job shares lives in one
composite action, `.github/actions/setup-rust`.

## Workflows

### CI (`ci.yml`)

Runs on every push to `main` and every pull request. Twelve jobs cover code
quality across several dimensions, and the single **CI Gate** job is the only
required status check: it fails if any other job (apart from the informational
`coverage`) did not succeed, without listing job names, so a new job cannot be
forgotten. The workflow sets `DUCKDB_DOWNLOAD_LIB=1` so test builds link a
prebuilt libduckdb instead of compiling DuckDB from source, and every `cargo`
call uses `--locked`.

| Job | Purpose | Tool |
|-----|---------|------|
| **fmt** | Formatting verification | `cargo fmt --all -- --check` |
| **clippy** | Zero-warning lint enforcement, including the unsafe-code lints; also type-checks every target | `cargo clippy --all-targets -- -D warnings` |
| **test** | 541 unit tests + 29 in-process integration tests, then the doc-test, on Linux and macOS | `cargo nextest run --all-targets --profile ci`, then `cargo test --doc` |
| **doc** | Documentation builds without warnings | `cargo doc --no-deps --document-private-items` with `-Dwarnings` |
| **msrv** | Minimum Supported Rust Version (1.88) | the `setup-rust` action with `toolchain: "1.88"`, then `cargo check --all-targets` |
| **wasm-check** | Library compiles for DuckDB-WASM | `cargo check --target wasm32-unknown-emscripten --lib` |
| **bench-compile** | Benchmarks compile (no execution) | `cargo bench --no-run` |
| **deny** | License, advisory, and source auditing | `cargo-deny` |
| **hygiene** | Unused dependencies, spelling, workflow lint and security audit, shell scripts, license headers | `cargo machete`, `typos`, `actionlint`, `zizmor`, `shellcheck`, `scripts/add-spdx-headers.sh --check` |
| **coverage** | Code coverage reporting (informational, not in the gate) | `cargo llvm-cov nextest` + Codecov |
| **e2e** | The reusable `e2e.yml` below | |
| **ci-gate** | Required status check gate | `jq` over `toJSON(needs)` |

The repository has no `cargo-semver-checks` job: the crate is not published
(`publish = false`) and its Rust API is internal; the SQL surface is the API,
and the exact overload counts are asserted in `tests/extension_load.rs` and
`scripts/compat-check.sh`.

**One compiler everywhere.** `rust-toolchain.toml` pins the toolchain
(1.97.0 at the time of writing). The `setup-rust` action installs exactly that
and exports `RUSTUP_TOOLCHAIN`, which outranks the toolchain file, so the MSRV
job really runs 1.88. (Before that, a bare `cargo check` after installing 1.87
silently ran the pinned toolchain; see `LESSONS.md`, lesson 19.) Only pushes to
`main` save the cargo cache; pull requests restore it.

### E2E (`e2e.yml`)

A reusable workflow (`workflow_call`, also runnable by hand) that `ci.yml`
calls. It builds the extension with the community Makefile exactly as the
community-extensions CI does, then asserts results.

| Step | What it checks |
|------|----------------|
| `make configure release` | The extension builds and is stamped for the stable C API (`C_STRUCT`, C API v1.2.0) |
| `make test_release` | The SQL logic tests in `test/sql/` pass in DuckDB 2.0.0 |
| `scripts/check-stable-api.py` (Linux) | The unstripped library calls only C API functions inside the stable prefix of `duckdb_ext_api_v1` (76 of 548 slots, highest 306, limit 357), so the C API v1.2.0 stamp stays true |
| `scripts/compat-check.sh` | 21 asserted queries covering all 8 functions, `TIMESTAMPTZ`, NULLs, error messages, the exact overload count (540), and one `GROUP BY` run at 1 and 4 threads that must give identical rows |

`build` runs these on Linux x86_64 and macOS ARM64. The `compat` job then loads
the Linux binary into DuckDB v1.3.2, v1.4.4, v1.5.0, and v1.5.6 and runs the
same script, so one binary is shown to answer identically on every release with
that C API. `scripts/install-duckdb-cli.sh` downloads the CLIs (DuckDB 2.0.0
renamed its assets: `.tar.gz` for 2.x, `.zip` before).

### Scheduled (`scheduled.yml`)

Weekly (Monday 04:23 UTC) and on demand. A red run is a heads-up, not a blocked
merge:

| Job | Question it answers |
|-----|---------------------|
| **advisories** | Did a RustSec advisory land for a locked dependency? (`cargo deny check advisories`) |
| **toolchain** (stable, beta) | Would the next Rust release break clippy or the tests? |
| **latest-deps** | Do the tests pass after `cargo update` (what a build without `--locked` gets)? |
| **duckdb-nightly** | Does the stamped binary still load and answer in the DuckDB nightly (`pip install --pre duckdb`, `scripts/nightly-smoke.py`)? |

### CodeQL (`codeql.yml`)

Runs GitHub's CodeQL static analysis for Rust on every push to `main`, every
pull request, and on a weekly schedule (Monday 06:00 UTC). Uses the
`security-and-quality` query suite for comprehensive coverage.

- **Triggers**: push to main, PRs, weekly cron
- **Language**: Rust
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

### Release (`release.yml`)

Triggered on a `vX.Y.Z` tag push, or by hand for an existing tag. The steps and
the follow-up pull request to the community repository are in
[`RELEASING.md`](https://github.com/tomtom215/duckdb-behavioral/blob/main/RELEASING.md).

1. **validate**: the tag is SemVer, and `Cargo.toml`, `description.yml` and a
   `CHANGELOG.md` section all carry the same version.
2. **build** (4 platforms): the community Makefile, an architecture check with
   `file`, then on the platforms that can run the binary the SQL logic tests and
   `scripts/compat-check.sh`. Builds use no cargo cache, so a cache written by a
   pull request cannot influence an attested binary.
3. **release**: a GitHub release whose notes are the `CHANGELOG.md` section,
   with `SHA256SUMS.txt` and a build-provenance attestation per archive.
4. **update-description-ref**: commits the tagged commit's SHA into
   `description.yml`'s `repo.ref` on `main`.

| Platform | Runner | Target |
|----------|--------|--------|
| Linux x86_64 | `ubuntu-latest` | `x86_64-unknown-linux-gnu` |
| Linux ARM64 | `ubuntu-24.04-arm` (native) | `aarch64-unknown-linux-gnu` |
| macOS x86_64 | `macos-latest` | `x86_64-apple-darwin` |
| macOS ARM64 | `macos-latest` | `aarch64-apple-darwin` |

Inputs reach shell steps through `env:` (never interpolated into the script),
and each job holds only the permissions it needs.

### Documentation site (`pages.yml`)

Builds the mdBook (v0.4.40 with `mdbook-mermaid` v0.14.0) on every pull request
that touches `docs/` or `demo/`, so broken links and preprocessor errors fail
before merge, and deploys it to GitHub Pages on pushes to `main`.

### Dependabot

Weekly updates for Cargo and GitHub Actions, with a 7-day cooldown and grouped
pull requests. Every `uses:` is pinned to a commit SHA. `libduckdb-sys`,
`duckdb` and `quack-rs` are pinned with `=` and move with the DuckDB release the
bindings come from; review those pull requests against "Updating DuckDB
version" in `CLAUDE.md`.

## Reproducing CI Locally

```bash
# The CI checks that run offline, in one command (tools that are not
# installed are skipped; CI always runs them)
./scripts/check.sh

# Or one at a time
DUCKDB_DOWNLOAD_LIB=1 cargo test --locked   # 541 unit + 29 integration + 1 doc-test
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --all -- --check
RUSTDOCFLAGS=-Dwarnings cargo doc --no-deps --document-private-items --locked
cargo deny check advisories bans licenses sources

# Build the extension (requires the submodule); do not set DUCKDB_DOWNLOAD_LIB here
git submodule update --init --recursive
make configure release
make test_release        # SQL logic tests

# Assert results through any DuckDB CLI
scripts/compat-check.sh "$(command -v duckdb)" build/release/behavioral.duckdb_extension
```
