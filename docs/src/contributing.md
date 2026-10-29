# Contributing

The contributing guide is [`CONTRIBUTING.md`](https://github.com/tomtom215/duckdb-behavioral/blob/main/CONTRIBUTING.md)
in the repository: setup, the quality checks a pull request must pass, how to
add a function, the testing and benchmark expectations, and the pull request
process. It is kept in one place so that it cannot disagree with itself.

The short version:

```bash
git clone --recurse-submodules https://github.com/tomtom215/duckdb-behavioral.git
cd duckdb-behavioral
cargo test --lib        # unit tests: nothing to install but Rust
./scripts/check.sh      # everything CI runs that works offline
```

Related pages: [Architecture](./internals/architecture.md) for how the code is
organised, [CI/CD](./operations/ci-cd.md) for what each workflow checks, and
[Performance](./internals/performance.md) for the benchmark protocol.
