# Lessons Learned

Hard-won insights from building a DuckDB extension in Rust with a C FFI boundary.

---

## Architecture

**1. Confine all unsafe to the FFI layer.**
Every `unsafe` block lives in `src/ffi/`. Business logic is 100% safe Rust with zero FFI dependencies. This means the core algorithms are testable, refactorable, and auditable without reasoning about pointer validity. Each unsafe block carries a `// SAFETY:` comment.

**2. Bitflags beat enums for combinable modes.**
ClickHouse's `window_funnel` modes are independently composable (`strict | strict_increase`). An enum forces mutually exclusive variants, requiring combinatorial explosion. A `FunnelMode(u8)` bitflag struct with `has()` / `with()` methods enables O(1) mode checks via bitwise AND while supporting arbitrary combinations from comma-separated SQL strings.

**3. u8 to u32 bitmask expansion is zero-cost due to alignment padding.**
The `Event` struct pairs an `i64` timestamp with a conditions bitmask. Because `i64` requires 8-byte alignment, the struct is padded to 16 bytes regardless of whether the bitmask is 1 byte or 4 bytes. Expanding from 8-condition to 32-condition support cost nothing at runtime.

**4. Arc\<str\> beats String for shared immutable aggregate state.**
Replacing `Option<String>` with `Option<Arc<str>>` in `NextNodeEvent` delivered 2.1-5.8x improvement. Event values in behavioral analytics are read-only after creation. `Arc::clone` is a single atomic increment (~1ns) vs `String::clone` copying len bytes (~20-80ns). The improvement compounds in combine operations where every element is cloned.

## Performance

**5. Combine is the dominant cost in DuckDB aggregates.**
DuckDB's segment tree calls combine O(n log n) times. Replacing O(n+m) merge-allocate with O(m) in-place extend yielded a 2,436x improvement at 10k states. The key insight: `combine(&self, other: &Self) -> Self` creates a new Vec every call; `combine_in_place(&mut self, other: &Self)` reuses the existing allocation. For left-fold chains this is O(N) vs O(N^2) total copies.

**6. NFA exploration order: lazy vs greedy is a 1,961x difference.**
In the LIFO-based NFA for `.*` (any events), push order determines whether the engine tries advancing the pattern first (lazy) or consuming events first (greedy). Greedy causes O(n^2) behavior because the NFA consumes all events before backtracking. Swapping two lines of push order made `sequence_match` go from 4.43s to 2.31ms at 1M events.

**7. Pattern specialization: dispatch common shapes to O(n) linear scans.**
Most behavioral patterns are simple chains like `(?1).*(?2).*(?3)`. Classifying patterns at execution time and dispatching to specialized single-pass scans (instead of the full NFA) delivered 39-40% additional improvement for `sequence_count`. The NFA is the correct general solution, but the common case does not need generality.

**8. Reuse heap allocations across loop iterations.**
Pre-allocating the NFA state Vec once and reusing it via `clear()` across all starting positions converted O(N) alloc/free pairs to O(1). This single change delivered 33-47% improvement for `sequence_count`. Applies anywhere a function is called repeatedly in a tight loop.

## Negative Results

**9. Radix sort was 4.3x slower than pdqsort for 16-byte structs.**
LSD radix sort (8-bit radix, 8 passes) has a scatter pattern that writes randomly to 256 buckets. For 16-byte elements at 100M scale, the cache/TLB misses dominate. Comparison-based sorts with good spatial locality win for embedded-key structs. Radix sort only wins when sorting small keys separate from large payloads.

**10. Branchless code was slower because the branch predictor is very good.**
Replacing branches with `i64::from(bool)` / `max` / `min` in sessionize caused ~5-10% regression. CMOV has fixed latency and always evaluates both paths. A correctly predicted branch (90/10 split from session boundaries) has near-zero cost. Only go branchless when misprediction rate exceeds ~5%.

**11. String pool with Copy indices was slower than Arc\<str\>.**
Separating strings into a `Vec<String>` pool with `u32` indices in a 24-byte `Copy` struct sounded ideal but measured 10-55% slower. The dual-vector overhead (two allocations, two resize paths, pool cloning in combine) outweighed the per-element size reduction. Simple reference counting beat the clever design.

**12. Negative results must be documented with the same rigor as wins.**
Three optimization hypotheses in one session all produced negative results. Documenting them honestly prevents future developers from re-attempting the same dead ends and establishes that the current implementation is already well-optimized in those areas.

## Testing & FFI

**13. 375 unit tests passed while the extension was completely broken.**
E2E testing against real DuckDB discovered three critical bugs that no unit test could catch: (a) SEGFAULT on extension load from incorrect pointer arithmetic, (b) 6 of 7 functions silently failing to register because `duckdb_aggregate_function_set_name` was not called per overload, (c) `window_funnel` returning wrong results because combine did not propagate config fields. Unit tests validate business logic; E2E tests validate the FFI boundary. Both are mandatory.

**14. DuckDB's segment tree creates zero-initialized target states before combine.**
`combine_in_place` must propagate ALL configuration fields, not just data. DuckDB calls `state_init` to create a fresh target, then combines source states into it. Fields like `window_size_us`, `mode`, and `pattern_str` that default to zero remain zero at finalize time unless explicitly copied from the source. This bug class is invisible in unit tests that construct states directly.

**15. DuckDB function set registration fails silently.**
`duckdb_register_aggregate_function_set` returns an error code but produces no diagnostic explaining why. The fix for our case: `duckdb_aggregate_function_set_name` must be called on each function in the set, not just the set itself. This is undocumented in the C API and was discovered by reading DuckDB's test code.
