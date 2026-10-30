# Releasing

A release is a `vX.Y.Z` tag. Pushing it runs `.github/workflows/release.yml`,
which builds the four platform archives, attests them, and publishes the
GitHub release with the matching `CHANGELOG.md` section as its notes. The
DuckDB Community Extensions repository then needs one pull request.

## 1. Prepare

1. Move the `## [Unreleased]` entries in `CHANGELOG.md` under a new
   `## [X.Y.Z] - YYYY-MM-DD` heading, and fix the comparison links at the
   bottom of the file. The release workflow fails if the version has no
   section.
2. Set the same version in `Cargo.toml` and `description.yml` (`extension:
   version`). Run `cargo check` once so `Cargo.lock` records the new version,
   and commit it: CI builds with `--locked` and fails on a stale lockfile. Do the
   same for the fuzz crate (`cd fuzz && cargo update -p duckdb-behavioral`): its
   lockfile records this crate's version through a path dependency, and
   `scripts/check-docs.py` fails when it differs.
   Command-line examples in the docs name the release (`-ev v0.11.0`, the
   attestation archive name); `scripts/check-docs.py`, which `check.sh` runs,
   fails until they match `Cargo.toml`.
3. Run the checks a contributor runs, plus the ones only a release needs:

   ```bash
   ./scripts/check.sh
   make configure release test_release
   scripts/compat-check.sh "$(command -v duckdb)" build/release/behavioral.duckdb_extension
   ```

   `compat-check.sh` asserts results for all eight functions in the DuckDB CLI
   you pass it; CI runs it against every release in the `e2e.yml` matrix.
4. If the DuckDB release the extension targets changed, follow "Updating
   DuckDB version" in `CLAUDE.md` first.

## 2. Tag

```bash
git tag vX.Y.Z
git push origin vX.Y.Z
```

The workflow checks that the tag, `Cargo.toml`, `description.yml` and
`CHANGELOG.md` agree, builds every platform without a cargo cache, runs the SQL
logic tests and `compat-check.sh` on the platforms it can execute, and attaches
`SHA256SUMS.txt` plus a build-provenance attestation. For a non-pre-release tag
it finishes by committing the tagged commit's SHA into `description.yml`'s
`repo.ref` on `main`.

A tag with a suffix (`v0.11.0-rc.1`) is published as a pre-release and does not
update `description.yml`.

## 3. Publish to the community channel

Open a pull request against
[`duckdb/community-extensions`](https://github.com/duckdb/community-extensions)
that sets `repo.ref` and `extension.version` in
`extensions/behavioral/description.yml` to the values now on `main`. Their CI
builds the extension for every non-excluded platform and runs `test/sql/`.

In the same pull request, copy `docs/function_descriptions.csv` to
`extensions/behavioral/docs/function_descriptions.csv`. The duckdb.org page for
the extension takes each function's description, comment and example from that
file in the community repository (`scripts/generate_md.sh` there joins it to
`duckdb_functions()` by name), because the stable C API has no way to attach
descriptions or parameter names to a function. `tests/extension_load.rs` checks
that the file lists exactly the registered functions and that every example
runs, so a function added without a row fails the tests.

## If the workflow fails

Fix the cause on `main`, delete the remote tag (`git push origin :refs/tags/vX.Y.Z`),
and tag the fixed commit again. Re-running with `workflow_dispatch` and the tag
name rebuilds that existing tag, not the fix.

If only the last job, `update-description-ref`, failed (for example a branch
ruleset refused its push to `main`), the release is already published. Set
`repo.ref` in `description.yml` to the tagged commit by hand or in a pull
request; re-running the workflow does not repeat that job, which runs only for
tag pushes.
