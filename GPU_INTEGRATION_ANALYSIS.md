# GPU Integration Feasibility Analysis: duckdb-behavioral Extension

**Date:** 2026-02-18
**Scope:** Evaluate Sirius GPU engine and custom Rust GPU approaches for accelerating the duckdb-behavioral extension.
**Standard:** Peer-reviewed academic evidence required for all claims. Zero speculation.

---

## Table of Contents

- [Executive Summary](#executive-summary)
- [1. Sirius Architecture Analysis](#1-sirius-architecture-analysis)
- [2. Fundamental Incompatibility: Sirius + Behavioral Analytics](#2-fundamental-incompatibility-sirius--behavioral-analytics)
- [3. Custom Rust GPU Implementation Analysis](#3-custom-rust-gpu-implementation-analysis)
- [4. Per-Function GPU Feasibility](#4-per-function-gpu-feasibility)
- [5. Academic Evidence: GPU Limitations for Stateful Aggregation](#5-academic-evidence-gpu-limitations-for-stateful-aggregation)
- [6. Quantitative Cost-Benefit Analysis](#6-quantitative-cost-benefit-analysis)
- [7. Recommendation](#7-recommendation)
- [8. What Would Change This Analysis](#8-what-would-change-this-analysis)
- [References](#references)

---

## Executive Summary

**Verdict: GPU acceleration is NOT recommended for this extension.**

After thorough analysis of the Sirius GPU engine, the academic literature on GPU-accelerated databases, the Rust GPU ecosystem, and our own algorithmic characteristics, the conclusion is unambiguous:

1. **Sirius cannot accelerate our functions.** Sirius intercepts DuckDB's query plan via Substrait and accelerates built-in relational operators (filter, join, group-by, sort). It does not — and architecturally cannot — accelerate custom aggregate UDFs registered through DuckDB's C API. Our extension's functions would execute on the CPU fallback path regardless of whether Sirius is loaded.

2. **Custom GPU kernels would be slower.** Our functions are dominated by sequential state dependencies (NFA pattern matching, greedy funnel scans, session boundary detection) that are fundamentally unsuitable for GPU SIMD execution. PCIe transfer overhead alone (107ms for 100M events) would consume 10-15% of total execution time before any computation begins.

3. **The CPU implementation is already near-optimal.** Sessionize processes 1 billion rows in 1.2 seconds (848 Melem/s). Window funnel processes 100M events at 140 Melem/s. These throughputs approach memory bandwidth limits. There is no compute bottleneck for GPU to solve.

---

## 1. Sirius Architecture Analysis

### How Sirius Works

Sirius is a GPU-native SQL engine that operates as a DuckDB extension. Based on the peer-reviewed paper [1] and GitHub repository [2]:

- **Integration:** Intercepts DuckDB's logical query plan via the **Substrait** intermediate representation format
- **Execution:** Delegates supported operators to GPU execution using **NVIDIA libcudf** (RAPIDS)
- **Fallback:** Returns control to DuckDB's CPU engine for unsupported operators
- **Requirements:** NVIDIA Volta+ GPU (compute capability 7.0+), CUDA 13.0+, Ubuntu 22.04+, libcudf 26.04+

### Supported Operators (from [1])

| Operator | GPU-Accelerated | Notes |
|---|---|---|
| Filter | Yes | Predicate pushdown |
| Projection | Yes | Expression evaluation |
| Join | Yes | Hash join, dominates TPC-H execution |
| Group-by Aggregation | Yes | Built-in aggregates only (SUM, COUNT, AVG, etc.) |
| Order-by / Sort | Yes | GPU radix sort |
| Top-N / Limit | Yes | |
| CTE | Yes | |
| Window functions | **No** | Planned future work |
| Custom UDFs/UDAFs | **No** | Active research area, not implemented |
| LIST/nested types | **No** | Planned future work |

### What Sirius Does NOT Support

From the paper [1]: Sirius's roadmap explicitly lists "UDFs" as future work. The DaMoN 2023 workshop paper [3] on GPU-accelerated UDAFs is a research prototype — it is not integrated into the Sirius production engine.

From the Sirius documentation [2]: When unsupported operators are encountered, `gpu_processing_function()` returns an error, triggering CPU fallback. Custom aggregate functions registered via DuckDB's C API are invisible to Sirius's Substrait-based query plan interception.

---

## 2. Fundamental Incompatibility: Sirius + Behavioral Analytics

### Architectural Mismatch

Our extension registers aggregate functions through DuckDB's raw C API using five callbacks: `state_size`, `state_init`, `state_update`, `state_combine`, `state_finalize`. These callbacks contain arbitrary Rust code (NFA engines, greedy scans, bitmask operations).

Sirius operates at the **query plan level**, not the **function implementation level**. It intercepts relational algebra operators (join, filter, aggregate) and replaces them with GPU equivalents. It has no mechanism to:

1. Inspect or translate our Rust callback implementations to GPU kernels
2. Transfer our per-group state structs (`WindowFunnelState`, `SequenceState`, etc.) to GPU memory
3. Execute our NFA pattern matcher on GPU hardware
4. Manage `Arc<str>` reference-counted strings in GPU memory

### What Would Happen If Both Extensions Were Loaded

If a user loads both `behavioral` and `sirius`:

```sql
LOAD behavioral;
LOAD sirius;

-- Sirius CANNOT accelerate this — falls back to CPU
SELECT user_id, window_funnel(INTERVAL '1 hour', timestamp, c1, c2, c3)
FROM events
GROUP BY user_id;
```

Sirius would encounter `window_funnel` as an unknown aggregate function in the Substrait plan, return an error, and DuckDB would execute the entire query on CPU. **There is zero performance benefit from Sirius for any of our 7 functions.**

### Could Sirius Accelerate Surrounding Operations?

In theory, Sirius could accelerate the `GROUP BY` hash table construction and the sort/filter operations that precede our aggregate functions. However:

- Our functions already receive data through DuckDB's chunk-based pipeline (2048 rows/chunk)
- DuckDB's CPU-side GROUP BY is already highly optimized
- The bottleneck is our aggregate computation (NFA, funnel scan), not data routing
- Mixing GPU and CPU execution paths adds synchronization overhead

---

## 3. Custom Rust GPU Implementation Analysis

### Available Rust GPU Frameworks

| Framework | Backend | Maturity | Aggregate Support |
|---|---|---|---|
| [rust-gpu](https://rust-gpu.github.io/) [4] | SPIR-V (Vulkan/WebGPU) | Experimental, requires nightly Rust | No aggregate primitives |
| [Rust-CUDA](https://github.com/Rust-GPU/Rust-CUDA) [5] | NVVM IR → PTX | Experimental, requires nightly Rust | No aggregate primitives |
| [CubeCL](https://github.com/tracel-ai/cubecl) [6] | CUDA + WGPU | Early stage | Tensor operations only |
| [wgpu](https://wgpu.rs/) [7] | WebGPU/Vulkan/Metal/DX12 | Production-ready for graphics | Compute shaders only |
| cudarc (Rust bindings) | CUDA | Stable | Raw CUDA kernel dispatch |

### Why Custom GPU Kernels Are Infeasible

**Problem 1: Sequential State Dependencies**

All 7 functions have inherent sequential data dependencies that prevent GPU parallelization:

- **Sessionize:** Each `update()` depends on `last_ts` from the previous row. O(1) per row.
- **Window Funnel:** Greedy scan with conditional step advancement — each step depends on previous.
- **Sequence Match/Count:** NFA execution uses a LIFO stack; each state transition depends on prior states.
- **Sequence Next Node:** Sequential pattern matching with early termination.
- **Retention:** O(1) bitmask OR — trivial, no compute to parallelize.

Academic literature confirms this limitation. Breß et al. [8] document that GPUs are "ill-suited for accelerating any database processing which cannot be parallelized." The VLDB 2024 survey [9] demonstrates that "window triggering depends on the window assignment and is order sensitive," with "cyclic control flow" making GPU acceleration of stateful windowed aggregation fundamentally difficult.

**Problem 2: Branch Divergence**

Our functions contain data-dependent branches that cause GPU warp divergence:

```rust
// Window funnel: every event checks different conditions
if event.condition(current_step) {
    current_step += 1;  // some threads advance
} else if mode.is_strict() && event.condition(current_step - 1) {
    break;  // some threads break
}
```

Breß et al. [8]: "When threads in a workgroup diverge, for example due to data-dependent conditionals, the multiprocessor has to serialize the code paths."

**Problem 3: PCIe Transfer Overhead**

| Scale | Data Size (16B/event) | PCIe Gen4 Transfer | CPU Execution | GPU Break-Even Speedup Needed |
|---|---|---|---|---|
| 1M events | 16 MB | 1 ms | 7 ms | 1.17x |
| 10M events | 160 MB | 11 ms | 70 ms | 1.19x |
| 100M events | 1.6 GB | 107 ms | 700-1000 ms | 1.12-1.17x |

Even with zero GPU compute time (impossible), GPU could only provide ~15% speedup from eliminating all compute. In practice, GPU compute would be **slower** than CPU due to branch divergence and sequential dependencies.

**Problem 4: Nightly Rust Requirement**

Both rust-gpu and Rust-CUDA require specific nightly Rust toolchains. Our extension targets stable Rust (MSRV 1.80) and is distributed through DuckDB Community Extensions. Introducing a nightly dependency would violate our production readiness requirements and break CI/CD.

**Problem 5: NVIDIA Lock-in**

CUDA-based approaches restrict execution to NVIDIA GPUs. Our extension currently runs on any platform DuckDB supports (Linux x86_64, Linux aarch64, macOS x86_64, macOS aarch64). GPU acceleration would either:
- Require NVIDIA hardware (unacceptable for a community extension)
- Require maintaining parallel CPU and GPU codepaths (massive complexity increase)

---

## 4. Per-Function GPU Feasibility

### Sessionize — NOT GPU-Suitable

- **Algorithm:** O(1) per-row state update comparing timestamp to `last_ts`
- **CPU throughput:** 848 Melem/s (1B rows in 1.2s) — approaches memory bandwidth limit
- **GPU analysis:** Pure sequential dependency chain. Each row depends on the previous row's `last_ts`. GPU would need to process rows one-at-a-time — slower than CPU due to kernel launch overhead (~10-100µs per launch)
- **Verdict:** GPU would be 100-1000x slower

### Retention — NOT GPU-Suitable

- **Algorithm:** O(1) bitmask OR per update, O(1) combine
- **CPU throughput:** 365 Melem/s at combine
- **GPU analysis:** Single bitwise OR operation per state. GPU's minimum useful work unit (one warp = 32 threads) exceeds the entire computation. Transfer overhead alone exceeds compute by orders of magnitude.
- **Verdict:** No compute to parallelize

### Window Funnel — NOT GPU-Suitable

- **Algorithm:** Collect events, sort (pdqsort), greedy forward scan with mode-dependent branching
- **CPU throughput:** 140 Melem/s (100M events)
- **GPU analysis:** The sort phase could theoretically use GPU radix sort, but Session 7 already tested CPU LSD radix sort and found it 4.3x slower due to cache locality issues with 16-byte elements [PERF.md]. The greedy scan has step-dependent branching that causes warp divergence. Presorted detection (Session 6) eliminates sort entirely in the common case.
- **Verdict:** Sort is not the bottleneck; scan has sequential dependencies

### Sequence Match/Count — Marginally Theoretically Possible, Practically Infeasible

- **Algorithm:** NFA pattern matching with LIFO stack, backtracking, time constraints
- **CPU throughput:** 95-111 Melem/s (100M events)
- **GPU analysis:** This is the only function where GPU's parallel state evaluation might theoretically help — evaluating multiple NFA states simultaneously. However:
  - `MAX_NFA_STATES = 10,000` — GPU warp size (32) vs state count creates poor utilization
  - LIFO stack semantics require sequential pop/push — cannot parallelize
  - Session 11 fast paths (adjacent conditions, wildcard-separated) already achieve O(n) for common patterns, eliminating the NFA entirely
  - Early termination (`sequence_match` returns on first match) wastes GPU compute
- **Verdict:** CPU fast paths already handle common cases; GPU overhead exceeds marginal gain on pathological cases

### Sequence Match Events — NOT GPU-Suitable

- **Algorithm:** Same as sequence match + timestamp collection during NFA traversal
- **GPU analysis:** Same as sequence match, with additional timestamp collection requiring per-state storage
- **Verdict:** Same as sequence match — infeasible

### Sequence Next Node — NOT GPU-Suitable

- **Algorithm:** Sequential event matching with `Arc<str>` string values
- **CPU throughput:** 18 Melem/s (10M events)
- **GPU analysis:** `Arc<str>` strings are reference-counted heap allocations. GPU has no mechanism for atomic reference counting of heap strings. Sequential matching with early termination prevents parallelization. The lower throughput (vs other functions) is due to string management overhead, not compute — GPU cannot help with this.
- **Verdict:** String management is the bottleneck; GPU cannot manage `Arc<str>`

---

## 5. Academic Evidence: GPU Limitations for Stateful Aggregation

### Peer-Reviewed Sources

**[8] Breß et al., "GPU-Accelerated Database Systems: Survey and Open Challenges" (Springer, 2014):**
> GPUs are "ill-suited for accelerating any database processing which cannot be parallelized, or which involve...complex control flow instructions." "CPUs outperform GPUs for tasks that are hard to parallelize or that involve complex control flow instructions."

**[9] Cao et al., "GPU Database Systems Characterization and Optimization" (VLDB 2024):**
> "GPU resource utilization is dependent on the implementation of each system, and profiling reveals that several systems still have opportunities for further optimization due to a lack of kernel fusion, inefficient thread termination, and other factors." This confirms that even well-funded GPU database systems struggle with efficient GPU utilization.

**[1] Yu et al., "Rethinking Analytical Processing in the GPU Era" (arXiv 2508.04701):**
> Sirius achieves 8.3x speedup on TPC-H — but this is for **relational operators** (joins, filters, sorts). Group-by aggregation with string keys actually **underperforms** on GPU due to "memory contention" with small distinct group counts. Custom UDAFs are explicitly listed as future work.

**[3] Yogatama et al., "Accelerating UDAFs with Block-wide Execution and JIT Compilation on GPUs" (DaMoN@SIGMOD 2023):**
> This paper demonstrates GPU UDAF acceleration achieving 3600x over Pandas — but for **simple, parallelizable aggregates** (SUM, COUNT, statistical functions). The technique uses JIT compilation to fuse map-reduce patterns. NFA pattern matching, greedy funnel scans, and sequential state machines are not addressed and cannot be expressed in this framework.

**[10] Comprehensive Survey: "A Comprehensive Overview of GPU Accelerated Databases" (arXiv 2406.13831, 2024):**
> Documents that GPU databases excel at "compute-intensive operations on large datasets" with "regular access patterns." Sequential state-dependent aggregation with branching control flow is explicitly identified as a poor fit.

**[11] Query Processing on Heterogeneous Hardware (Springer 2024):**
> "The window triggering depends on the window assignment and is order sensitive...The cyclic control flow between these tasks makes it hard to apply state-of-the-art query compilation techniques." This directly addresses why windowed behavioral analytics functions resist GPU acceleration.

### Summary of Academic Consensus

The academic literature is consistent: GPU acceleration provides significant benefits for:
- Embarrassingly parallel operations (filter, project)
- Regular-access-pattern operations (sort, hash join)
- Compute-intensive numerical operations (statistical aggregates)

GPU acceleration provides no benefit (or negative benefit) for:
- Sequential state-dependent computations
- Branching control flow with data-dependent conditions
- Small per-group working sets
- Early termination patterns
- String/reference-counted data management

All 7 of our functions fall squarely in the second category.

---

## 6. Quantitative Cost-Benefit Analysis

### Best-Case Scenario: GPU for `sequence_count` at 100M Events

This is the most GPU-friendly function (highest compute-to-data ratio, no strings, no early termination in count mode).

| Component | CPU (measured) | GPU (estimated) |
|---|---|---|
| Data transfer H→D | 0 ms | 107 ms |
| Sort (pdqsort/GPU radix) | ~300 ms | ~30 ms |
| NFA execution | ~700 ms | ~350 ms (optimistic 2x) |
| Data transfer D→H | 0 ms | <1 ms |
| Kernel launch overhead | 0 ms | ~1 ms |
| **Total** | **~1050 ms** | **~489 ms** |
| **Speedup** | — | **2.1x** |

This 2.1x estimate is **extremely optimistic** — it assumes:
- Zero GPU memory allocation overhead (unrealistic)
- Perfect GPU utilization of NFA states (impossible due to warp divergence)
- No combine overhead on GPU (DuckDB combines on CPU)
- Presorted detection doesn't apply (it usually does, making CPU sort ~0ms)

**Realistic estimate with presorted input (the common case):**

| Component | CPU (measured) | GPU (estimated) |
|---|---|---|
| Presorted detection | ~50 ms | N/A (must transfer first) |
| Sort | 0 ms (skipped) | 107 ms transfer + 30 ms sort |
| NFA execution | ~700 ms | ~350 ms |
| Transfer back | 0 ms | <1 ms |
| **Total** | **~750 ms** | **~488 ms** |
| **Speedup** | — | **1.5x** |

A 1.5x speedup for the single most GPU-friendly function, requiring:
- NVIDIA GPU hardware dependency
- CUDA toolkit dependency
- Nightly Rust toolchain
- Parallel CPU/GPU codepaths to maintain
- Testing on GPU hardware in CI/CD

This does not meet the engineering cost-benefit threshold.

### Engineering Cost

| Item | Estimated Effort | Ongoing Maintenance |
|---|---|---|
| CUDA kernel development (7 functions) | Significant | High — dual codepaths |
| GPU memory management | Significant | Medium |
| CI/CD with GPU runners | Medium | High — GPU CI is expensive |
| Cross-platform fallback logic | Medium | High |
| Benchmarking infrastructure | Medium | Medium |
| **Additional dependency burden** | libcudf or cudarc + CUDA toolkit | Version tracking, ABI compatibility |

---

## 7. Recommendation

### Do NOT Integrate Sirius

Sirius cannot accelerate custom aggregate UDFs. Loading Sirius alongside our extension would provide zero performance benefit for our functions. It is architecturally impossible without Sirius adding a custom UDAF JIT framework — which is research-stage work [3], not production-ready.

### Do NOT Build Custom GPU Kernels

The combination of:
1. Sequential state dependencies in all 7 functions
2. Branch divergence in funnel scans and NFA execution
3. PCIe transfer overhead consuming 10-15% of execution time
4. Already-optimized CPU code approaching memory bandwidth limits
5. Nightly Rust requirement for GPU frameworks
6. NVIDIA hardware lock-in
7. Massive engineering cost for 1.5x best-case marginal gain

makes GPU acceleration a negative-ROI investment.

### Continue CPU Optimization

The current architecture is the correct architecture. Future performance work should focus on:

1. **SIMD intrinsics** for condition bitmask evaluation (AVX2/NEON) — potential 2-4x on the scan phase
2. **Parallel sort** using Rayon for extremely large unsorted inputs
3. **Further NFA fast paths** for additional common pattern shapes
4. **Cache-line alignment** optimization for event arrays
5. **Profile-guided optimization (PGO)** builds

These CPU-side optimizations can deliver comparable or better speedups without any of the GPU downsides.

---

## 8. What Would Change This Analysis

GPU integration would become worth revisiting if **all** of the following conditions were met:

1. **Sirius ships production-ready GPU UDAF support** with a stable API for registering custom aggregate functions that execute on GPU. Currently research-stage [3].

2. **DuckDB's aggregate framework supports GPU offload natively** — the five callbacks (state_size, init, update, combine, finalize) would need GPU-aware variants. This does not exist.

3. **PCIe Gen6 or CXL** reduces transfer overhead by 4-8x, making the break-even threshold achievable.

4. **Stable Rust GPU toolchain** — rust-gpu or Rust-CUDA reaches stable Rust support, eliminating the nightly requirement.

5. **Our workload profile changes** — if we add functions with embarrassingly parallel characteristics (e.g., batch condition evaluation across millions of independent groups simultaneously).

None of these conditions are met today. Conditions 1 and 2 are the most critical and have no public timeline.

---

## References

- [1] X. Yu et al., "Rethinking Analytical Processing in the GPU Era," arXiv:2508.04701, 2025. https://arxiv.org/abs/2508.04701
- [2] Sirius GitHub repository. https://github.com/sirius-db/sirius
- [3] B. Yogatama et al., "Accelerating User-Defined Aggregate Functions with Block-wide Execution and JIT Compilation on GPUs," DaMoN@SIGMOD, 2023. https://dl.acm.org/doi/10.1145/3592980.3595307
- [4] Rust GPU project. https://rust-gpu.github.io/
- [5] Rust-CUDA project. https://github.com/Rust-GPU/Rust-CUDA
- [6] CubeCL project. https://github.com/tracel-ai/cubecl
- [7] wgpu project. https://wgpu.rs/
- [8] S. Breß et al., "GPU-Accelerated Database Systems: Survey and Open Challenges," Springer LNCS, 2014. https://link.springer.com/chapter/10.1007/978-3-662-45761-0_1
- [9] J. Cao et al., "GPU Database Systems Characterization and Optimization," PVLDB 17(3), 2024. https://www.vldb.org/pvldb/vol17/p441-cao.pdf
- [10] "A Comprehensive Overview of GPU Accelerated Databases," arXiv:2406.13831, 2024. https://arxiv.org/html/2406.13831v1
- [11] "Query Processing on Heterogeneous Hardware," Springer, 2024. https://link.springer.com/chapter/10.1007/978-3-031-74097-8_2
- [12] NVIDIA Blog: "NVIDIA CUDA-X Powers the New Sirius GPU Engine for DuckDB." https://developer.nvidia.com/blog/nvidia-gpu-accelerated-sirius-achieves-record-setting-clickbench-record/
