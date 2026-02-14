# Benchmarking

This page documents the benchmark infrastructure, methodology, and
reproduction instructions for the duckdb-behavioral extension.

## Methodology

All benchmarks use [Criterion.rs](https://bheisler.github.io/criterion.rs/book/)
with the following configuration:

- **Sample size:** 100 samples (default), reduced to 10 for inputs >= 100M
- **Confidence level:** 95%
- **Measurement time:** Extended to 30-60 seconds for large inputs
- **Outlier detection:** Criterion's built-in outlier classification
- **Iterations:** Criterion automatically determines iteration count
- **Warm-up:** Criterion's default warm-up period

Results are reported with confidence intervals. A result is considered
statistically significant only when 95% confidence intervals do not overlap.

## Benchmark Suite

Seven benchmark files cover all functions:

| Benchmark | Functions | Max Scale | Memory Limit |
|-----------|-----------|-----------|--------------|
| `sessionize_bench` | update, combine | **1 billion** | O(1) state, no limit |
| `retention_bench` | update, combine | **1 billion** | O(1) state, no limit |
| `window_funnel_bench` | finalize, combine | 100 million | 16 bytes/event |
| `sequence_bench` | match, count, combine | 100 million | 16 bytes/event |
| `sort_bench` | random, presorted | 100 million | 16 bytes/event + clone |
| `sequence_next_node_bench` | update, combine, realistic | 10 million | 32 bytes/event |
| `sequence_match_events_bench` | update, combine | 100 million | 16 bytes/event |

### Scale Limits

Event-collecting functions store events in a `Vec<Event>` (16 bytes per
event). At 100 million events, this requires 1.6 GB of memory. Criterion
clones the data per iteration, requiring approximately 3.2 GB total. On
a 21 GB system, 100 million is the practical maximum for event-collecting
benchmarks.

O(1)-state functions (sessionize, retention) store no per-event data,
enabling benchmarks up to 1 billion elements.

`sequence_next_node` uses 32-byte `NextNodeEvent` structs with `Arc<str>`
string storage, further reducing the maximum feasible scale.

## Running Benchmarks

```bash
# Run all benchmarks (may require several hours for full suite)
cargo bench

# Run a specific benchmark
cargo bench --bench sessionize_bench
cargo bench --bench retention_bench
cargo bench --bench window_funnel_bench
cargo bench --bench sequence_bench
cargo bench --bench sort_bench
cargo bench --bench sequence_next_node_bench
cargo bench --bench sequence_match_events_bench

# Run a specific benchmark group
cargo bench --bench sequence_bench -- sequence_match

# Compile benchmarks without running (for CI)
cargo bench --no-run
```

## Reading Results

Criterion generates HTML reports in `target/criterion/`. Open
`target/criterion/report/index.html` in a browser for interactive
charts.

Command-line output includes:

```
sequence_match/100000000
    time:   [1.0521 s 1.0789 s 1.1043 s]
    thrpt:  [90.551 Melem/s 92.687 Melem/s 95.049 Melem/s]
```

The three values in brackets represent the lower bound, point estimate,
and upper bound of the 95% confidence interval.

## Baseline Validation Protocol

Before any optimization work:

1. Run `cargo bench` 3 times to establish a stable baseline
2. Compare against the documented baseline in [PERF.md](https://github.com/tomtom215/duckdb-behavioral/blob/main/PERF.md)
3. Environment differences (CPU, memory, OS) produce different absolute
   numbers but the same relative improvements

After optimization:

1. Run `cargo bench` 3 times with the optimization
2. Compare 95% CIs against the pre-optimization baseline
3. Accept only when CIs do not overlap (statistically significant)
4. If CIs overlap, revert and document as a negative result

## Benchmark Data Generation

Each benchmark generates synthetic data with realistic patterns:

- **Sessionize:** Events every 5 minutes with a 33-minute gap every 10
  events (simulates real session boundaries)
- **Retention:** Rotating true conditions per event
- **Window funnel:** Conditions fire for 1/3 of events in round-robin
- **Sequence:** Alternating condition patterns for match/count testing
- **Sort:** Reverse-order timestamps with jitter (worst case), and
  forward-order (best case for presorted detection)
- **Sequence next node:** Unique strings per event (worst case) and
  pooled 100-value cardinality (realistic case)
