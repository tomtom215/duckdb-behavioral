# Lessons Learned — DuckDB Behavioral Analytics Extension

Accumulated from past development sessions. Consult before beginning any work.

## Table of Contents

- [Type Safety & Overflow](#type-safety--overflow) (1)
- [Build & Dependency Management](#build--dependency-management) (2-3, 53-54, 62)
- [Testing Strategy](#testing-strategy) (8, 13-14, 22, 26, 48-49, 56-57, 64-65)
- [Unsafe Code & FFI](#unsafe-code--ffi) (4-5, 7, 50-52, 55, 63)
- [Benchmarking & Measurement](#benchmarking--measurement) (6, 15-16, 18-21, 38, 42)
- [Combine Operations](#combine-operations) (9-11, 51)
- [Sorting & Data Structures](#sorting--data-structures) (12, 28-29, 31)
- [NFA Pattern Engine](#nfa-pattern-engine) (17, 27, 58-61)
- [ClickHouse Semantics](#clickhouse-semantics) (24-25, 30, 34-36)
- [Performance Optimization Principles](#performance-optimization-principles) (19, 32-33)
- [String & Memory Optimization](#string--memory-optimization) (37, 43-46)
- [Community Extension](#community-extension) (39-41, 47, 62)

---

## Type Safety & Overflow

1. **Retention finalize overflow**: `1u32 << i` panics when `i >= 32`. Always test
   at type boundaries (`u8::MAX`, `u32::MAX`, `i32::MIN`, etc.).

## Build & Dependency Management

2. **cargo-deny config format**: Pin tool versions. Use `deny.toml` v2 format with
   `[graph]` section.

3. **Transitive dependency licenses**: Run `cargo deny check licenses` locally after
   any dependency change. `CC0-1.0`, `CDLA-Permissive-2.0` from transitive deps
   must be in the allow list.

53. **Extension metadata version is C API version, not DuckDB version**: DuckDB
    v1.4.4 uses C API version `v1.2.0`. The `append_extension_metadata.py -dv`
    flag must use the C API version. Using the DuckDB release version (`-dv v1.4.4`)
    causes: "file was built for DuckDB C API version 'v1.4.4' but we can only load
    extensions built for DuckDB C API 'v1.2.0'". The C API version can be found in
    `duckdb-loadable-macros` source (`DEFAULT_DUCKDB_VERSION = "v1.2.0"`) or by
    checking what version string DuckDB reports in the error message.

54. **`libduckdb-sys` `loadable-extension` feature is sufficient for runtime**:
    This feature provides runtime function pointer stubs via global atomic statics,
    initialized by `duckdb_rs_extension_api_init`. Neither `duckdb` nor
    `duckdb-loadable-macros` are needed as runtime dependencies. `duckdb` remains
    as a dev-dependency with `bundled` feature for unit tests that use
    `Connection::open_in_memory`. This reduces the dependency tree by ~6 crates
    (darling, darling_core, darling_macro, ident_case, strsim, duckdb-loadable-macros).

62. **Library `[lib] name` must match the extension name for community builds**:
    The community extension Makefile expects `lib{extension_name}.so`. If the
    crate name is `duckdb-behavioral` (producing `libduckdb_behavioral.so`) but
    `description.yml` says `name: behavioral`, the build fails with
    `FileNotFoundError: libbehavioral.so`. Fix: add `name = "behavioral"` to
    `[lib]` in `Cargo.toml`. This cascades: all benchmark `use` imports and
    doc-test imports must also change from `duckdb_behavioral::` to `behavioral::`.

## Testing Strategy

8. **Test organization**: Group by category — state tests, edge cases, combine
   correctness, overflow/boundary. Makes gaps visible.

13. **Mutation testing reveals test gaps**: cargo-mutants found 17 surviving mutants
    including combine_in_place (untested), OR vs XOR in retention combine (both pass
    when conditions don't overlap), and first_ts/last_ts tracking comparisons.
    Run mutation testing after adding new public methods.

14. **Property-based testing catches algebraic violations**: proptest verified
    combine associativity, identity, and commutativity across randomized inputs.
    Essential for aggregate functions used in DuckDB's segment trees where these
    properties are required for correctness.

22. **Systematic mutation analysis reveals NFA executor gaps**: Automated search for
    mutation opportunities (operator swaps, branch removal, return value changes)
    found that NFA executor time constraint propagation through `.` (OneEvent) and
    `.*` (AnyEvents) steps lacked coverage. Tests for individual features existed
    but not for their composition. Always test feature interactions, not just
    individual features.

26. **Event filtering interacts with test design**: The `update()` method filters
    events where `has_any_condition()` is false to avoid storing irrelevant data.
    Test "gap" events (intended to test non-matching intermediate events) must have
    at least one true condition to pass the filter, or they will be silently dropped.
    Design tests with awareness of upstream filters.

48. **SQL integration tests use SQLLogicTest format**: DuckDB's test runner
    expects `.test` files in `test/sql/` using SQLLogicTest format: `statement ok`,
    `query <type_codes>`, `----` result separator, `NULL` for null values.
    Column type codes: `I` = integer, `T` = text, `R` = real. The `require
    behavioral` directive loads the extension before test execution.

49. **E2E testing against real DuckDB is non-negotiable**: Unit tests alone missed
    3 critical bugs: SEGFAULT on load, 6 functions failing to register, and
    `window_funnel` returning wrong results. All 403 unit tests passed while the
    extension was completely broken in production. Unit tests validate business
    logic in isolation; E2E tests validate the FFI boundary, DuckDB's data chunk
    format, state lifecycle management, and the extension loading mechanism. Both
    test levels are mandatory — never ship without E2E validation.

56. **Unit tests and E2E tests serve fundamentally different purposes**: 375 unit
    tests validated business logic: combine associativity, mode parsing, NFA pattern
    matching, edge cases. But they operate on Rust structs in isolation — they never
    exercise the FFI boundary, DuckDB's data chunk format, DuckDB's aggregate state
    lifecycle (init→update→combine→finalize→destroy), or extension loading. All 3
    bugs found in Session 10 were in the FFI/integration layer. The test pyramid
    must include both levels.

57. **Debug with `eprintln` in FFI callbacks, then remove**: When debugging E2E
    failures, adding `eprintln!("behavioral: DEBUG ...")` in FFI callbacks (update,
    combine, finalize) is the fastest way to understand what DuckDB is actually
    passing to the extension. DuckDB's stderr output is visible in the CLI. Always
    remove debug output before committing — it causes measurable performance
    degradation at scale and pollutes user-visible output.

64. **SQL test expected values must be validated against actual DuckDB output**:
    Session 12 found 4 categories of expected-value errors in SQL tests:
    (a) session IDs 0-indexed vs actual 1-indexed, (b) LIST(TIMESTAMP) format
    without single quotes vs actual `['ts', ...]` format, (c) window_funnel
    step counts wrong due to cross-PR event aggregation, (d) sequence_next_node
    test semantics wrong. Always run `make test_release` to validate test
    expectations against real DuckDB before committing SQL tests.

65. **`sequence_next_node` requires base_condition AND event1 at start**: The
    function requires the starting event to satisfy BOTH the base_condition AND
    the first event condition simultaneously. A pattern with `base=is_home,
    event1=is_product` will NOT match a "home" event because `is_product` is
    false at that position. The start position must satisfy: `base_condition(i)
    && conditions[0](i)`. Design SQL tests with awareness of this conjunction.

## Unsafe Code & FFI

4. **Unsafe code confinement**: All unsafe lives in `src/ffi/`. Business logic is
   100% safe Rust. Every unsafe block has `// SAFETY:` documentation.

5. **Raw connection extraction was fatally fragile (REMOVED in Session 10)**:
   `extract_raw_connection` depended on `duckdb` internal struct layout and
   caused SEGFAULTs. Replaced with custom C entry point using `duckdb_connect`
   directly. See lesson 52.

7. **Function set overloads**: 31 overloads (2-32 boolean parameters) per function
   because `duckdb_aggregate_function_set_varargs` does not exist.

50. **`duckdb_aggregate_function_set_name` must be called on EACH function in a
    set**: DuckDB's `duckdb_register_aggregate_function_set` silently fails if
    individual functions in the set don't have their name set via
    `duckdb_aggregate_function_set_name`. The name must be set on both the set
    (via `duckdb_create_aggregate_function_set`) AND each member function. This is
    not documented in the C API — discovered by reading DuckDB's own test code
    (`test/api/capi/test_capi_aggregate_functions.cpp`). When debugging function
    registration, test with a single function first (via
    `duckdb_register_aggregate_function`), then progressively add function set
    complexity.

51. **DuckDB combine creates fresh zero-initialized target states**: DuckDB's
    segment tree calls `state_init` to create new target states, then combines
    source states into them. This means `combine_in_place` must propagate ALL
    configuration fields — not just events/data. Fields that default to zero or
    empty will remain zero at finalize time unless explicitly copied. Affected
    fields: `window_size_us`, `mode` (window_funnel), `pattern_str` (sequence),
    `direction`, `base`, `num_steps` (sequence_next_node). The `sessionize`
    function avoids this because its `combine()` returns a new `Self` and
    handles the empty case via `(None, _) => other.clone()`.

52. **Custom C entry point eliminates struct layout fragility**: The hand-written
    `behavioral_init_c_api` uses `duckdb_connect` directly from
    `duckdb_extension_access.get_database`, bypassing `duckdb::Connection`
    entirely. This eliminates `extract_raw_connection` which depended on internal
    `Rc<RefCell<InnerConnection>>` layout — a version-pinned, architecture-specific
    hack that caused SEGFAULTs. The custom entry point requires only `libduckdb-sys`
    (not `duckdb` or `duckdb-loadable-macros`) at runtime, reducing the dependency
    surface and eliminating the DuckDB version coupling risk documented in lesson 41.

55. **Function set registration failure is silent**: `duckdb_register_aggregate_function_set`
    returns `DuckDBError` but produces no diagnostic output explaining WHY it failed.
    The extension's own `eprintln` was the only indication. Debug strategy: (1) test
    with `duckdb_register_aggregate_function` for a single function first, (2) test
    with a function set containing one overload, (3) check DuckDB's test code for
    correct API usage patterns. The `duckdb_functions()` system table is invaluable
    for verifying which functions actually registered.

63. **`duckdb_vector_ensure_validity_writable` required for NULL output**: When
    writing NULL values in FFI finalize callbacks, calling
    `duckdb_vector_get_validity` on an uninitialized vector returns an
    uninitialized pointer. Writing to it produces garbage instead of NULL.
    Always call `duckdb_vector_ensure_validity_writable(result)` before
    `duckdb_vector_get_validity(result)` to ensure a valid bitmap exists.

## Benchmarking & Measurement

6. **Benchmark stability**: Run Criterion benchmarks 3+ times before comparing.
   Single-run comparisons are invalid. Report confidence intervals.

15. **Session startup protocol**: Read PERF.md baseline first. Run benchmarks 3x
    to establish session baseline BEFORE any changes. Compare against documented
    CIs. This catches environment-specific differences early (different machines
    produce different absolute numbers but same relative improvements).

16. **Optimization priority by measured data**: The combine operations were orders
    of magnitude more expensive than any other operation (75ms vs 200µs). Always
    target the largest measured cost first. Per-element analysis (cost/N) reveals
    algorithmic complexity in practice.

18. **10M benchmarks expose DRAM bandwidth limits**: At 10M events (160MB working
    set), throughput drops from ~800 Melem/s to ~42-96 Melem/s as the working set
    exceeds L3 cache. Per-element cost analysis at this scale reveals the true
    constant factors hidden by cache effects at smaller sizes.

19. **Negative results are results**: Document optimizations with overlapping CIs
    honestly. The compiled pattern preservation showed ~2.5% improvement but CIs
    overlapped — committed as code quality but NOT counted as performance gain.
    This prevents accumulation of unjustified performance claims.

20. **Large-scale benchmarks find hidden bugs**: The 1M benchmark expansion directly
    exposed the NFA O(n²) bug that was invisible at 10K events (30µs vs expected
    ~17µs — easy to dismiss as noise). At 1M events, the 4.4 second runtime was
    unmistakable. Always benchmark at scales that exceed cache hierarchies.

21. **O(1)-state functions scale to 1B without memory constraints**: Functions like
    sessionize (O(1) state) and retention (O(1) combine, 4 bytes/state) can be
    benchmarked at 1B elements because they don't store event data. Event-collecting
    functions (window_funnel, sequence) are limited by available RAM since they store
    16 bytes per event. On a 21GB system, 100M events (1.6GB) is the practical maximum
    for Criterion benchmarks that clone the data per iteration.

38. **Non-monotonic throughput at large scales**: `sequence_next_node` shows
    6.58 Melem/s at 1M but 8.12 Melem/s at 10M — a counterintuitive improvement.
    This is measurement noise: at 10M with only 10 Criterion samples, the CIs are
    wide ([1.15s, 1.30s]). The underlying trend is decreasing throughput with
    scale as expected from cache hierarchy effects. Wide CIs at the largest scale
    should be flagged but not over-interpreted.

42. **Benchmark data generation cost matters**: The `sequence_next_node` benchmark
    uses `format!("page_{i}")` to generate per-event String values. At 10M events,
    this generates 10M unique strings of ~12-18 characters each. In production,
    event values have much lower cardinality (few hundred distinct page names).
    The benchmark measures worst-case allocation behavior, not typical usage.
    Session 9 added a "realistic cardinality" benchmark variant that reuses a
    pool of ~100 distinct `Rc<str>` values, showing 35% improvement at 1M scale.

## Combine Operations

9. **Combine is the dominant cost**: In DuckDB's segment tree windowing, combine
   is called O(n log n) times. The combine implementation IS the bottleneck for
   event-collecting functions. Replacing O(n+m) merge-alloc with O(m) in-place
   extend yielded 2,436x improvement at scale. Always profile combine first.

10. **Deferred sorting**: If finalize re-sorts events anyway, maintaining sorted
    order in combine is wasted work. Append-only combine + sort-in-finalize is
    strictly better than merge-in-combine + sort-in-finalize.

11. **In-place vs returning-new**: `combine(&self, other: &Self) -> Self` creates
    a new Vec every time. `combine_in_place(&mut self, other: &Self)` reuses the
    existing Vec with amortized O(1) append. For left-fold chains, the difference
    is O(N²) vs O(N) total copies. Always provide an in-place variant.

## Sorting & Data Structures

12. **Unstable sort for Copy types**: When element ordering among equal keys has no
    defined semantics, prefer `sort_unstable_by_key` (pdqsort). Eliminates O(n)
    auxiliary memory for Copy types and provides better constant factors.

28. **Presorted detection saves O(n log n) for ordered input**: DuckDB often
    provides events in timestamp order via ORDER BY. An explicit `is_sorted`
    check with `Iterator::windows(2)` and early termination on first violation
    is cheaper than pdqsort's adaptive path for the fully-sorted case. The O(n)
    overhead for unsorted input is negligible before O(n log n) sort.

29. **u8 → u32 bitmask is free due to alignment**: Expanding the Event conditions
    bitmask from `u8` to `u32` does not change the struct size: `i64` (8 bytes)
    requires 8-byte alignment, so the struct is padded to 16 bytes regardless of
    whether the second field is 1 byte or 4 bytes. This means 32-condition support
    is a zero-cost upgrade from 8-condition support.

31. **LSD radix sort is catastrophically slow for embedded-key structs**: For
    sorting 16-byte Event structs by their embedded i64 key, LSD radix sort
    (8-bit radix, 8 passes) was 4.3x slower than pdqsort at 100M elements.
    The scatter pattern (random writes to 256 buckets) has terrible cache/TLB
    locality for large elements. Radix sort wins only when sorting small keys
    (4-8 bytes) separate from large payloads. For embedded keys, comparison-based
    sorts with good spatial locality (pdqsort, introsort) are superior.

## NFA Pattern Engine

17. **NFA exploration order matters catastrophically**: In a LIFO-based NFA
    backtracking engine, the push order for `AnyEvents` (.*) determines lazy vs
    greedy semantics. Greedy (consume event first) causes O(n²) at scale because
    the NFA consumes all events before trying to advance the pattern. Lazy (advance
    step first) is O(n * pattern_len). This produced a 1,961x speedup at 1M events.
    Always verify NFA semantics match the specification (ClickHouse uses lazy).

27. **NFA timestamp collection requires separate execution path**: Extracting
    matched condition timestamps during NFA execution requires tracking per-state
    `collected: Vec<i64>` alongside the standard `event_idx`/`step_idx`. This
    cannot be bolted onto the existing `execute_pattern`/`count_pattern` without
    breaking their performance characteristics. A separate `execute_pattern_events`
    function with `NfaStateWithTimestamps` keeps the hot paths clean.

58. **Reuse heap allocations across loop iterations**: When a function is called
    repeatedly in a loop (e.g., `try_match_from` called from every starting
    position in `execute_pattern`), pre-allocate any internal Vecs outside the
    loop and pass them by `&mut` reference. Use `clear()` to reset without
    freeing. This converts O(N) alloc/free pairs to O(1) total allocations.
    For `sequence_count`, this single change delivered 33-47% improvement.

59. **Pattern-directed specialization eliminates NFA overhead for common cases**:
    Most behavioral analytics patterns are simple chains of conditions with
    optional `.*` wildcards (e.g., `(?1).*(?2).*(?3)`). These can be executed
    with a single-pass O(n) linear scan instead of the full NFA backtracking
    engine. Classifying patterns at execution time and dispatching to specialized
    code paths delivered 39-40% additional improvement for `sequence_count`.
    The key insight: the NFA is a general solution, but the common case doesn't
    need generality.

60. **First-condition pre-checks add overhead in tight loops**: Adding an
    `if let Some(idx) = first_cond` check before each NFA invocation to skip
    non-matching starting positions caused 0-8% regression. The NFA already
    handles non-matching starts efficiently (push + pop + condition check =
    ~5ns). The extra branch in the hot loop path costs more than it saves.
    When the operation being skipped is already very cheap, pre-checking is
    a pessimization.

61. **Adjacent-condition fast paths must not skip-ahead on intermediate
    failures**: For pattern `(?1)(?2)`, when checking events[i..i+k], if
    events[i+j] fails (j > 0), advancing to i+j+1 is WRONG because positions
    i+1 through i+j could be valid starting positions. The NFA handles this
    correctly by always advancing by 1 on failure. The fast path must do
    the same. This bug was caught by adding a targeted regression test.

## ClickHouse Semantics

24. **Bitflags beat enums for combinable modes**: ClickHouse's `window_funnel` modes
    are independently composable (e.g., `strict | strict_increase`). An enum forces
    mutually exclusive variants, requiring N! combinations as separate variants.
    A bitflag struct `FunnelMode(u8)` with `has()`, `with()` methods enables O(1)
    mode checks via bitwise AND while supporting arbitrary combinations. The
    `parse_mode_str()` method handles comma-separated string input from SQL.

25. **Multi-step advancement per event**: ClickHouse's `window_funnel` allows a
    single event to advance multiple funnel steps when it satisfies consecutive
    conditions. Using `if event.condition(step)` limits to one step per event;
    `while event.condition(step)` enables multi-step. The `strict_once` mode then
    naturally constrains this by breaking after one advancement. Always verify
    ClickHouse semantics with the source code, not just the documentation.

30. **Comma-separated mode parsing needs careful whitespace handling**: When
    parsing mode strings like `"strict_increase, strict_once"`, each token must
    be trimmed independently. Empty tokens (from trailing commas or double commas)
    should be silently skipped rather than causing errors. The `parse_modes()`
    method handles all edge cases: empty string, whitespace-only, trailing comma,
    duplicate modes (idempotent via OR).

34. **`sequenceNextNode` return type is NOT polymorphic**: ClickHouse's
    `sequenceNextNode` always returns `Nullable(String)`, contrary to the original
    assumption that it would require polymorphic return types. The `event_column`
    parameter only accepts `String` or `Nullable(String)`. This simplifies FFI
    registration to a single VARCHAR return type. Always verify ClickHouse
    semantics from the source/docs, not from assumptions.

35. **Separate event struct for String-carrying functions**: `sequence_next_node`
    requires storing a `String` value per event, which cannot be added to the
    existing `Copy` `Event` struct without breaking all other functions'
    performance characteristics. A dedicated `NextNodeEvent` struct keeps the
    hot paths clean for the six existing functions while supporting the per-event
    String storage needed by `sequence_next_node`. Always use separate types
    when new requirements break existing performance invariants.

36. **Store all events for next-node functions**: Unlike `sequence_match` which
    filters events with `has_any_condition()`, `sequence_next_node` must store
    ALL events regardless of conditions because any event could be the "next
    node" whose value is returned. Different filtering requirements demand
    different `update()` implementations.

## Performance Optimization Principles

23. **Throughput stabilizes at DRAM bandwidth for O(1) operations**: Sessionize
    maintains ~824 Melem/s from 100K to 1B elements (1.21 ns/element). This is
    consistent with the operation being compute-bound on a tight compare + branch
    + increment loop that fits entirely in registers, with no data-dependent memory
    access pattern. The ~3 cycles per element matches the expected cost of a
    conditional branch + integer add on modern x86.

32. **Branchless is not always better**: Replacing well-predicted branches
    (90/10 or better split) with branchless operations (`i64::from(bool)`, `max`,
    `min`) can ADD overhead. The CMOV instruction has fixed latency and always
    executes both paths, while a correctly predicted branch has zero-cost
    continuation. Only attempt branchless optimization when branch misprediction
    rate exceeds ~5% (random data, 50/50 splits). For periodic or monotonic
    patterns, the branch predictor achieves near-perfect accuracy.

33. **Negative results are essential for future sessions**: Session 7 tested 3
    performance hypotheses and all were negative. This is valuable because it
    establishes that the current implementation is already well-optimized: pdqsort
    is the right choice for Event sorting, branches are correct for Sessionize,
    and the retention bottleneck is allocation not computation. Future sessions
    should not re-attempt these optimizations.

## String & Memory Optimization

37. **String allocation dominates sequence_next_node cost**: At 10K events,
    `sequence_next_node` is 31x slower per element than `sequence_match` (53
    ns/element vs 1.7 ns/element). The dominant costs are: per-event heap
    allocation for `Option<String>`, larger struct size (~80+ bytes vs 16 bytes)
    reducing cache utilization, and deep String cloning in combine operations.
    Future optimization should target string interning, arena allocation, or
    SmallString optimization for short event values (<24 bytes).

43. **Rc\<str\> beats String for shared immutable event values**: Replacing
    `Option<String>` with `Option<Rc<str>>` in `NextNodeEvent` delivered 2.1-5.8x
    improvement with zero functionality change. The key insight: event values in
    behavioral analytics are read-only after creation, making reference counting
    ideal. `Rc::clone` is a single atomic increment (~1ns) vs `String::clone`
    which copies len bytes (~20-80ns). The improvement compounds in combine
    operations where every element is cloned. Always prefer `Rc<str>` over
    `String` for immutable shared data in aggregate state.

44. **String pool with separate index vectors is a pessimization**: Separating
    string storage into `Vec<String>` pool + `u32` indices in a `Copy` struct
    sounds optimal (24-byte Copy events!) but measured 10-55% slower at most
    scales. The dual-vector overhead (two separate allocations, two resize
    paths, pool cloning in combine) outweighs the per-element size reduction.
    Only the 1M scale showed improvement (22%) where cache line utilization
    dominated. Lesson: measure before committing to "obviously better" data
    structures. Simple reference counting (Rc) beat the clever pool design.

45. **Struct size reduction has diminishing returns below cache line**: Reducing
    `NextNodeEvent` from 40 bytes (String) to 32 bytes (Rc\<str\>) improved
    throughput by 2.1-5.8x, but most of the gain came from O(1) clone, not
    size reduction. The further reduction to 24 bytes (PooledEvent with u32
    index) was a net negative. Below one cache line (64 bytes), the access
    pattern and clone cost matter more than the absolute struct size.

46. **Realistic cardinality benchmarks reveal Rc sharing benefits**: With 100
    distinct `Rc<str>` values (typical for page names), `Rc::clone` from the
    pool avoids even the initial `Rc::from()` allocation. At 1M events this
    provides 35% additional improvement over unique-string benchmarks. Always
    include a realistic-cardinality benchmark alongside worst-case (unique
    values) to understand production performance characteristics.

## Community Extension

39. **Community extension path for Rust**: DuckDB community extensions support
    two paths for Rust: (a) CMake + Corrosion + C++ glue (`build: cmake`,
    `requires_toolchains: rust`) — well-tested, 5+ examples; (b) Pure Rust via
    `extension-template-rs` (`build: cargo`) — experimental, only `rusty_quack`
    example. Our extension's `duckdb_entrypoint_c_api` macro aligns with path (b).
    Key requirement: add a `Makefile` with configure/debug/release targets and a
    metadata appending script. Platform exclusions needed for wasm and some Windows
    targets.

40. **Community extension `description.yml` is the only submission artifact**:
    Extension source stays in our repo. The PR to `duckdb/community-extensions`
    adds only `extensions/behavioral/description.yml`. The CI builds for all
    platforms automatically. No code review of extension source — only build
    success and metadata correctness. The `ref` field pins to a specific commit
    hash for reproducibility.

41. **DuckDB version coupling risk eliminated by custom entry point**: The
    Session 10 custom C entry point (`behavioral_init_c_api`) removed the
    `duckdb = "=1.4.4"` pin dependency for connection extraction. The only
    remaining version coupling is `libduckdb-sys = "=1.4.4"` for C API struct
    definitions, which is stable across DuckDB releases. The community extension
    system supports `ref` (stable) and `ref_next` (development) fields to manage
    version-specific builds.

47. **Community extension submodule is the critical dependency**: The
    `extension-ci-tools` git submodule provides the Makefile includes, Python
    metadata appending script, and test runner. Without it, the Makefile
    targets (`configure`, `debug`, `release`, `test`) cannot function. The
    submodule must be initialized before any `make` invocation. Pin to a
    known-working commit for reproducibility.
