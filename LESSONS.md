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

**4. Arc\<str\> beats String for shared immutable aggregate state (superseded by lesson 23).**
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

**11. String pool with Copy indices was slower than Arc\<str\> (as built then; lesson 23 revisits it).**
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

## Distribution & Robustness

**16. Decide the ABI from the binary, not from a comment.**
The Makefile set `USE_UNSTABLE_C_API=1` under a comment saying the Rust bindings needed the unstable C API. They did not: disassembling an unstripped release build and listing the `__DUCKDB_*` function-pointer statics that are *loaded* showed 76 of 548 slots used, the highest at 306, all inside the 357-slot stable prefix (`scripts/check-stable-api.py` now repeats that measurement in CI). The unstable stamp pinned each binary to one DuckDB release, so the community channel dropped the extension the day DuckDB v1.5.6 shipped. Stamped `C_STRUCT` for C API v1.2.0, one binary loads into DuckDB 1.3.2 through 2.0.0, and CI loads it into five of those releases and asserts the answers.

**17. `panic = "abort"` silently disables every panic guard.**
`catch_unwind` catches nothing under `abort`; the runtime aborts before unwinding begins. Combined with raw `extern "C"` callbacks, any panic in an aggregate killed the user's DuckDB process. Use the SDK's guarded callback macros and `panic = "unwind"`, and test the profile, because nothing else will notice the setting coming back.

**18. A DuckDB C API defect cannot always be fixed from the extension.**
`agg(x ORDER BY y)`, `OVER ()` and `UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING` frames hand every C API aggregate a one-element state array with `count > 1` (duckdb/duckdb#26109). The callback cannot detect it without performing the out-of-bounds read, and the C API has no aggregate bind hook to refuse the query. Map the exact shapes with valgrind, document them where users look, and give the workaround. Then map them per release: `scripts/crash-shapes.sh` showed that `OVER (PARTITION BY g)` without `ORDER BY` also crashes DuckDB 1.3.2 and 1.4.4 though not 1.5.0 or later, and that none of the shapes crashes 2.0.0. A matrix measured on one release had been written up as the whole truth.

**19. A toolchain file overrides the toolchain your CI step installs.**
`rust-toolchain.toml` pinned `channel = "stable"`, so the MSRV job's `dtolnay/rust-toolchain@1.87` followed by a bare `cargo check` ran stable and never tested 1.87 (the MSRV is now 1.88). The file now pins an exact release, and the shared `setup-rust` action exports `RUSTUP_TOOLCHAIN`, which outranks the file, so the MSRV job runs what its name says.

**20. A parity claim is only as good as a differential test against the real system.**
The docs said "complete ClickHouse parity", backed by reading ClickHouse's source and docs. Running both engines on the same random event groups (`clickhouse local` makes this cheap) showed `window_funnel` disagreeing in 5–30% of groups in almost every mode, and found three wrong-result or hang bugs in the `sequence_*` fast paths that 486 unit tests missed. The same comparison also exposed defects in ClickHouse itself. An exhaustive reference (ClickHouse's algorithm, keeping every chain) let each remaining difference be attributed instead of argued about. Re-run the comparison whenever matching code changes.

**21. A worst case nobody benchmarked is still the product.**
Lesson 6 fixed the backtracking search's exploration order, and the benchmarks only measured patterns that match quickly. Any pattern that could not complete through `.*` or a skipping time gate stayed quadratic per group (8–9 s at 32,000 events, 32–35 s for `sequence_match_events`). Replacing the search did not mean re-deriving its semantics from prose: the old code was kept test-only as an oracle, and differential proptests decide equivalence (they catch planted bugs in the new matcher's edge handling, such as a trailing `.*(?t<5)`, which matches only at the end of the events). Time the not-found case, and keep the old implementation as the spec when replacing one.

**22. Make the unsafe boundary a lint, not a review habit.**
`unsafe_code = "deny"` crate-wide, `#[allow]` only on `mod ffi`, and `clippy::undocumented_unsafe_blocks` plus `multiple_unsafe_ops_per_block` inside it turn "every unsafe block is one operation with a SAFETY note" into a compile error. The callbacks then became safe Rust over a small `view.rs` whose constructors state what DuckDB guarantees and whose accessors bounds-check every row, so a new accessor belongs there, not in a callback. Measured on DuckDB 2.0.0 the views cost nothing detectable (CPU time, interleaved runs), once the panic path was moved out of line and `#[inline]` hints were added; a first version without them looked 4-8% slower.

**23. When the host leaks your state, shrink it and make every growth fallible.**
DuckDB never destroys the aggregate states of groups a `GROUP BY .. LIMIT` or an interrupt did not reach, and an infallible `Arc<str>` per event aborted the process under memory pressure. One byte buffer per state, with 24-byte records holding ranges into it and `try_reserve` on every growth path, turned the abort into a SQL error and cut the CPU time of `sequence_next_node` roughly in half. The Arc and string-pool experiments in lessons 4 and 11 compared two per-event layouts; the layout that wins is the one that has no per-event allocation.

**24. For an unpublished crate, the SQL surface is the API; assert it exactly.**
`cargo-semver-checks` guards a Rust API nobody depends on. What users depend on is the set of registered overloads, so the integration test and `scripts/compat-check.sh` assert the exact count per function (540 in all). A dropped or doubled overload now fails a test instead of shipping.

**25. Explain a difference with a model that predicts every case, not with a sample that explains most.**
To explain the `sequence_match_events` differences from ClickHouse, the events before the extension's chain were deleted and ClickHouse asked again. That reproduced 975 of 1,274 sampled cases, and the other 299 stayed "unexplained" for a while: the abandoned attempt shared its first step with the successful match, so the pruning could not remove it. The better question was what rule produces ClickHouse's answer. "Report the first strictly longest chain of the search" is one line added to the extension's own reference matcher, and a ~100-line model with that rule plus two others found by running ClickHouse (a time constraint after `.*` is measured from the event where `.*` stopped; `.*` cannot absorb the last event) predicted all 14.4 million ClickHouse results of the `sequence_*` functions with no exception, and another 14.4 million on a second seed the rules were not fitted on. It also corrected two statements in the docs: 160 `sequence_match` differences had been attributed to ClickHouse's undefined behaviour but were a `.*` rule the extension had already fixed, and that undefined behaviour showed up on ClickHouse 26.9.8.3 (answers changed between runs of identical input) and not on 26.10.2.103. Keep the model in the repository so the claim can be rerun (`scripts/clickhouse-sequence-diff.py`).
