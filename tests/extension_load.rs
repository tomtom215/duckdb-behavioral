// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! In-process integration tests that load the **real** loadable extension into a
//! live `DuckDB` instance and exercise every function through actual SQL.
//!
//! # Why this exists
//!
//! Unit tests exercise the pure-Rust cores and the FFI callbacks in isolation
//! (via `MockVectorReader`/`MockVectorWriter` and `AggregateTestHarness`), but
//! they cannot exercise the *registration* path — the code that runs when
//! `DuckDB` actually `LOAD`s the compiled `.duckdb_extension`. That path has
//! historically hidden the most dangerous bugs: a SEGFAULT on load, functions
//! silently failing to register, and a function returning wrong results — all
//! while the unit suite was green (see `CLAUDE.md`, "E2E testing is
//! non-negotiable").
//!
//! `quack_rs::testing::InMemoryDb::open_unsigned()` (new in quack-rs 0.14.0)
//! closes that gap *inside `cargo test`*: it opens an in-memory `DuckDB` with
//! `allow_unsigned_extensions` enabled, so we can `LOAD` the locally-built
//! (and therefore unsigned) artifact and assert on real query output — no
//! external `duckdb` CLI required. This is the same end-to-end path a user hits
//! with `LOAD behavioral`, run in-process.
//!
//! # How it works
//!
//! 1. Build the release `cdylib` (`cargo build --release --lib`), once.
//! 2. Append the `DuckDB` extension metadata footer to the raw shared library,
//!    producing a `.duckdb_extension` (mirrors `append_extension_metadata.py`).
//! 3. Open `InMemoryDb::open_unsigned()` and `LOAD` the artifact with the
//!    metadata checks left on.
//! 4. Run SQL covering all eight aggregate functions and assert on the results — the same
//!    expectations encoded in `test/sql/*.test`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use quack_rs::testing::InMemoryDb;

/// Minimum `DuckDB` C API version the extension declares (the `-dv` metadata
/// field). For the stable `C_STRUCT` ABI this is the C API version quack-rs
/// requests at load (`quack_rs::DUCKDB_API_VERSION`), not a `DuckDB` release:
/// any `DuckDB` whose C API is at least this version loads the binary. Kept in
/// sync with `TARGET_DUCKDB_VERSION` in the `Makefile`.
const DUCKDB_VERSION: &str = quack_rs::DUCKDB_API_VERSION;
/// Extension version metadata field (`-ev`); matches `Cargo.toml`'s `version`.
const EXTENSION_VERSION: &str = concat!("v", env!("CARGO_PKG_VERSION"));
/// ABI type: the stable C API struct. The extension calls only functions in
/// the stable prefix of `duckdb_ext_api_v1`, so it is not pinned to one
/// `DuckDB` release (`C_STRUCT_UNSTABLE` would be).
const ABI_TYPE: &str = "C_STRUCT";

/// The `DuckDB` platform string for the host target. `DuckDB` refuses to load
/// an extension whose platform differs from its own, and the test does not
/// relax that check.
const fn duckdb_platform() -> &'static str {
    match (cfg!(target_os = "macos"), cfg!(target_arch = "aarch64")) {
        (true, true) => "osx_arm64",
        (true, false) => "osx_amd64",
        (false, true) => "linux_arm64",
        (false, false) => "linux_amd64",
    }
}

/// Builds the release `cdylib` (no-op if already current) and returns its path.
fn build_release_cdylib() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    // `CARGO` is set by cargo when running test binaries; fall back to `cargo`.
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());

    let status = Command::new(&cargo)
        .args(["build", "--release", "--lib"])
        .current_dir(manifest_dir)
        .status()
        .expect("failed to spawn `cargo build --release --lib`");
    assert!(status.success(), "`cargo build --release --lib` failed");

    let target_dir =
        std::env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| format!("{manifest_dir}/target"));
    // `[lib] name = "behavioral"` → `libbehavioral.{so,dylib}`.
    let lib_file = if cfg!(target_os = "macos") {
        "libbehavioral.dylib"
    } else {
        "libbehavioral.so"
    };
    let path = PathBuf::from(target_dir).join("release").join(lib_file);
    assert!(
        path.exists(),
        "built cdylib not found at {} — check `[lib] name`",
        path.display()
    );
    path
}

/// Encodes a metadata field as a 32-byte, NUL-padded ASCII block.
fn padded_field(value: &str) -> [u8; 32] {
    assert!(
        value.len() <= 32,
        "metadata field {value:?} exceeds 32 bytes"
    );
    let mut field = [0u8; 32];
    field[..value.len()].copy_from_slice(value.as_bytes());
    field
}

/// Appends the `DuckDB` extension metadata footer to `raw_lib`, writing the
/// resulting `.duckdb_extension` to `out`.
///
/// This is a byte-for-byte port of `extension-ci-tools`'
/// `append_extension_metadata.py`: a WebAssembly custom-section header
/// (`duckdb_signature`), eight 32-byte fields written FIELD8→FIELD1, and a
/// 256-byte zero-filled signature slot.
fn append_metadata(raw_lib: &Path, out: &Path) {
    let mut bytes = std::fs::read(raw_lib).expect("read raw cdylib");

    // ── start signature (Wasm custom section so Wasm binaries stay valid) ──
    bytes.push(0x00); // custom section id
    bytes.push(147); // LEB128 low byte of 531 (1 + 16 + 2 + 8*32 + 256)
    bytes.push(4); // LEB128 high byte of 531
    bytes.push(16); // length of the section name
    bytes.extend_from_slice(b"duckdb_signature"); // 16-byte section name
    bytes.push(128); // LEB128 low byte of 512
    bytes.push(4); // LEB128 high byte of 512

    // ── eight 32-byte fields, written FIELD8 (last) → FIELD1 (first) ──
    bytes.extend_from_slice(&padded_field("")); // FIELD8 (unused)
    bytes.extend_from_slice(&padded_field("")); // FIELD7 (unused)
    bytes.extend_from_slice(&padded_field("")); // FIELD6 (unused)
    bytes.extend_from_slice(&padded_field(ABI_TYPE)); // FIELD5 abi_type
    bytes.extend_from_slice(&padded_field(EXTENSION_VERSION)); // FIELD4 ext version
    bytes.extend_from_slice(&padded_field(DUCKDB_VERSION)); // FIELD3 duckdb version
    bytes.extend_from_slice(&padded_field(duckdb_platform())); // FIELD2 platform
    bytes.extend_from_slice(&padded_field("4")); // FIELD1 header signature

    // ── 256-byte signature slot (unsigned → all zeroes) ──
    bytes.extend_from_slice(&[0u8; 256]);

    std::fs::write(out, &bytes).expect("write .duckdb_extension");
}

/// Builds + signs the extension once per test process and returns its path.
///
/// `cargo nextest` runs each test in its own process, so several may sign
/// concurrently. Each writes a private temporary file and renames it over one
/// shared name: the content is identical, a rename is atomic, and a process
/// that already loaded the previous file keeps its own inode. (A per-pid file
/// name instead left one 600 kB file behind per test process, per run.)
fn extension_path() -> &'static Path {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let raw = build_release_cdylib();
        let tmp = raw.with_file_name(format!(
            "behavioral.test.{}.duckdb_extension.tmp",
            std::process::id()
        ));
        append_metadata(&raw, &tmp);
        // DuckDB derives the init symbol from the file name up to the first
        // dot, so the shared name must start with the extension name.
        let out = raw.with_file_name("behavioral.test.duckdb_extension");
        std::fs::rename(&tmp, &out).expect("publish signed test extension");
        out
    })
    .as_path()
}

/// Opens a fresh in-memory `DuckDB` with the behavioral extension loaded.
fn load_extension() -> InMemoryDb {
    let db = InMemoryDb::open_unsigned().expect("open in-memory DuckDB (unsigned)");
    // No `allow_extensions_metadata_mismatch`: the footer must be one the
    // in-process DuckDB accepts as written, exactly as a user's `LOAD` would.
    let load = format!("LOAD '{}'", extension_path().display());
    db.execute_batch(&load)
        .unwrap_or_else(|e| panic!("LOAD failed for {}: {e}", extension_path().display()));
    db
}

/// The extension loads without error and registers every overload of the
/// eight aggregate functions plus the `behavioral_version()` diagnostic
/// scalar. A missing registration here is exactly the class of bug that passed
/// the unit suite while shipping a broken extension.
///
/// The counts are exact: each function registers one overload per condition
/// count and per timestamp type (`TIMESTAMP` and `TIMESTAMPTZ`), so a changed
/// `overloads(range, ..)` bound or a dropped timestamp type moves a number.
#[test]
fn loads_and_registers_all_functions() {
    let db = load_extension();
    let expected: [(&str, i64); 9] = [
        ("sessionize", 2),             // 1 signature x 2 timestamp types
        ("retention", 31),             // 2..=32 conditions
        ("window_funnel", 128),        // 1..=32 conditions x {no mode, mode} x 2 types
        ("window_funnel_events", 128), // as window_funnel
        ("sequence_match", 62),        // 2..=32 conditions x 2 types
        ("sequence_count", 62),
        ("sequence_match_events", 62),
        ("sequence_next_node", 64), // 1..=32 event conditions x 2 types
        ("behavioral_version", 1),
    ];
    let mut total = 0;
    for (func, overloads) in expected {
        let count: i64 = db
            .query_one(&format!(
                "SELECT count(*) FROM duckdb_functions() WHERE function_name = '{func}'"
            ))
            .unwrap_or_else(|e| panic!("catalog query for {func} failed: {e}"));
        assert_eq!(count, overloads, "overloads registered for `{func}`");
        total += count;
    }
    assert_eq!(
        total, 540,
        "total overloads (CLAUDE.md, scripts/compat-check.sh)"
    );
}

#[test]
fn behavioral_version_scalar() {
    let db = load_extension();
    let v: String = db.query_one("SELECT behavioral_version()").unwrap();
    assert_eq!(v, env!("CARGO_PKG_VERSION"));
}

#[test]
fn sessionize_window_function() {
    let db = load_extension();
    db.execute_batch(
        "CREATE TABLE s(ts TIMESTAMP);
         INSERT INTO s VALUES
            ('2024-01-01 00:00:00'), ('2024-01-01 00:05:00'),
            ('2024-01-01 00:10:00'), ('2024-01-01 02:00:00'),
            ('2024-01-01 02:05:00');",
    )
    .unwrap();
    let ids: Vec<i64> = db
        .conn()
        .prepare(
            "SELECT sessionize(ts, INTERVAL '30 minutes') OVER (ORDER BY ts) FROM s ORDER BY ts",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(ids, vec![1, 1, 1, 2, 2]);
}

#[test]
fn retention_aggregate() {
    let db = load_extension();
    db.execute_batch(
        "CREATE TABLE a(user_id INTEGER, day DATE);
         INSERT INTO a VALUES
            (1,'2024-01-01'),(1,'2024-01-02'),(1,'2024-01-03'),
            (2,'2024-01-01'),(2,'2024-01-03'),
            (3,'2024-01-01');",
    )
    .unwrap();
    let q = |uid: i32| {
        format!(
            "SELECT CAST(retention(day='2024-01-01', day='2024-01-02', day='2024-01-03') AS VARCHAR) \
             FROM a WHERE user_id = {uid}"
        )
    };
    let r1: String = db.query_one(&q(1)).unwrap();
    let r2: String = db.query_one(&q(2)).unwrap();
    let r3: String = db.query_one(&q(3)).unwrap();
    assert_eq!(r1, "[true, true, true]");
    assert_eq!(r2, "[true, false, true]");
    assert_eq!(r3, "[true, false, false]");
}

#[test]
fn window_funnel_aggregate() {
    let db = load_extension();
    db.execute_batch(
        "CREATE TABLE f(user_id INTEGER, ts TIMESTAMP, event VARCHAR);
         INSERT INTO f VALUES
            (1,'2024-01-01 00:00:00','view'),(1,'2024-01-01 00:05:00','cart'),(1,'2024-01-01 00:10:00','purchase'),
            (2,'2024-01-01 00:00:00','view'),(2,'2024-01-01 00:05:00','cart'),
            (3,'2024-01-01 00:00:00','view'),(3,'2024-01-01 05:00:00','cart');",
    )
    .unwrap();
    let rows: Vec<(i32, i32)> = db
        .conn()
        .prepare(
            "SELECT user_id, window_funnel(INTERVAL '1 hour', ts, \
                event='view', event='cart', event='purchase') \
             FROM f GROUP BY user_id ORDER BY user_id",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(rows, vec![(1, 3), (2, 2), (3, 1)]);

    // strict_increase mode (named-mode overload exercises the VARCHAR arg path).
    let strict: i32 = db
        .query_one(
            "SELECT window_funnel(INTERVAL '1 hour', 'strict_increase', ts, \
                event='view', event='cart', event='purchase') \
             FROM f WHERE user_id = 1",
        )
        .unwrap();
    assert_eq!(strict, 3);
}

#[test]
fn window_funnel_events_aggregate() {
    let db = load_extension();
    db.execute_batch(
        "CREATE TABLE fe(user_id INTEGER, ts TIMESTAMP, event VARCHAR);
         INSERT INTO fe VALUES
            (1,'2024-01-01 00:00:00','view'),(1,'2024-01-01 00:05:00','cart'),(1,'2024-01-01 00:10:00','purchase'),
            (2,'2024-01-01 00:00:00','view'),(2,'2024-01-01 05:00:00','cart'),
            (3,'2024-01-01 00:00:00','cart');",
    )
    .unwrap();
    // Complete chain: one timestamp per matched step.
    let full: String = db
        .query_one(
            "SELECT CAST(window_funnel_events(INTERVAL '1 hour', ts, \
                event='view', event='cart', event='purchase') AS VARCHAR) \
             FROM fe WHERE user_id = 1",
        )
        .unwrap();
    assert_eq!(
        full,
        "['2024-01-01 00:00:00', '2024-01-01 00:05:00', '2024-01-01 00:10:00']"
    );
    // Out-of-window second step: only the entry is matched.
    let partial: String = db
        .query_one(
            "SELECT CAST(window_funnel_events(INTERVAL '1 hour', ts, \
                event='view', event='cart', event='purchase') AS VARCHAR) \
             FROM fe WHERE user_id = 2",
        )
        .unwrap();
    assert_eq!(partial, "['2024-01-01 00:00:00']");
    // No entry condition: empty list.
    let empty: String = db
        .query_one(
            "SELECT CAST(window_funnel_events(INTERVAL '1 hour', ts, \
                event='view', event='cart', event='purchase') AS VARCHAR) \
             FROM fe WHERE user_id = 3",
        )
        .unwrap();
    assert_eq!(empty, "[]");
    // Mode overload binds and behaves (strict_order).
    let with_mode: String = db
        .query_one(
            "SELECT CAST(window_funnel_events(INTERVAL '1 hour', 'strict_order', ts, \
                event='view', event='cart', event='purchase') AS VARCHAR) \
             FROM fe WHERE user_id = 1",
        )
        .unwrap();
    assert_eq!(
        with_mode,
        "['2024-01-01 00:00:00', '2024-01-01 00:05:00', '2024-01-01 00:10:00']"
    );
}

#[test]
fn sequence_match_and_count_aggregates() {
    let db = load_extension();
    db.execute_batch(
        "CREATE TABLE c(user_id INTEGER, ts TIMESTAMP, is_view BOOLEAN, is_cart BOOLEAN, is_purchase BOOLEAN);
         INSERT INTO c VALUES
            (1,'2024-01-01 00:00:00',true,false,false),(1,'2024-01-01 00:05:00',false,true,false),(1,'2024-01-01 00:10:00',false,false,true),
            (2,'2024-01-01 00:00:00',true,false,false),(2,'2024-01-01 00:05:00',true,false,false),
            (3,'2024-01-01 00:00:00',true,false,false),(3,'2024-01-01 00:05:00',false,false,true);",
    )
    .unwrap();

    let matches: Vec<(i32, bool)> = db
        .conn()
        .prepare(
            "SELECT user_id, sequence_match('(?1)(?2)(?3)', ts, is_view, is_cart, is_purchase) \
             FROM c GROUP BY user_id ORDER BY user_id",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(matches, vec![(1, true), (2, false), (3, false)]);

    let counts: Vec<(i32, i64)> = db
        .conn()
        .prepare(
            "SELECT user_id, sequence_count('(?1).*(?3)', ts, is_view, is_cart, is_purchase) \
             FROM c GROUP BY user_id ORDER BY user_id",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(counts, vec![(1, 1), (2, 0), (3, 1)]);
}

#[test]
fn sequence_match_events_aggregate() {
    let db = load_extension();
    db.execute_batch(
        "CREATE TABLE e(user_id INTEGER, ts TIMESTAMP, c1 BOOLEAN, c2 BOOLEAN, c3 BOOLEAN);
         INSERT INTO e VALUES
            (1,'2024-01-01 00:00:00',true,false,false),(1,'2024-01-01 00:05:00',false,true,false),(1,'2024-01-01 00:10:00',false,false,true),
            (2,'2024-01-01 00:00:00',true,false,false),(2,'2024-01-01 00:05:00',true,false,false);",
    )
    .unwrap();
    let matched: String = db
        .query_one(
            "SELECT CAST(sequence_match_events('(?1)(?2)(?3)', ts, c1, c2, c3) AS VARCHAR) \
             FROM e WHERE user_id = 1",
        )
        .unwrap();
    assert_eq!(
        matched,
        "['2024-01-01 00:00:00', '2024-01-01 00:05:00', '2024-01-01 00:10:00']"
    );
    // No complete match for user 2: ClickHouse's sequenceMatchEvents
    // returns the LONGEST PARTIAL chain — (?1) matched at the first event.
    let partial: String = db
        .query_one(
            "SELECT CAST(sequence_match_events('(?1)(?2)(?3)', ts, c1, c2, c3) AS VARCHAR) \
             FROM e WHERE user_id = 2",
        )
        .unwrap();
    assert_eq!(partial, "['2024-01-01 00:00:00']");
}

/// `ClickHouse` time-constraint semantics through live SQL: the constraint
/// gates the next step but non-matching events in between are skipped, and
/// `sequence_match_events` returns the longest partial chain on no-match.
#[test]
fn clickhouse_time_constraint_semantics() {
    let db = load_extension();
    db.execute_batch(
        "CREATE TABLE tc(ts TIMESTAMP, a BOOLEAN, b BOOLEAN, g BOOLEAN);
         INSERT INTO tc VALUES
            ('2024-01-01 00:00:00', true,  false, false),
            ('2024-01-01 00:00:02', false, false, true),  -- gap event
            ('2024-01-01 00:00:05', false, true,  false);",
    )
    .unwrap();

    // (?1)(?t<=10)(?2): the gap event between them is skipped (ClickHouse
    // semantics) — previously this required strict adjacency.
    let matched: bool = db
        .query_one("SELECT sequence_match('(?1)(?t<=10)(?2)', ts, a, b) FROM tc")
        .unwrap();
    assert!(matched, "gap events inside the time window must be skipped");

    // (?1)(?t>=4)(?2): 5s elapsed satisfies >=4 even with the gap event.
    let matched: bool = db
        .query_one("SELECT sequence_match('(?1)(?t>=4)(?2)', ts, a, b) FROM tc")
        .unwrap();
    assert!(matched);

    // (?1)(?t<=2)(?2): 5s elapsed violates <=2 -> no match.
    let matched: bool = db
        .query_one("SELECT sequence_match('(?1)(?t<=2)(?2)', ts, a, b) FROM tc")
        .unwrap();
    assert!(!matched);
}

#[test]
fn sequence_next_node_aggregate() {
    let db = load_extension();
    db.execute_batch(
        "CREATE TABLE p(user_id INTEGER, ts TIMESTAMP, page VARCHAR, is_home BOOLEAN, is_product BOOLEAN);
         INSERT INTO p VALUES
            (1,'2024-01-01 00:00:00','home',true,false),(1,'2024-01-01 00:01:00','product',false,true),(1,'2024-01-01 00:02:00','cart',false,false),
            (2,'2024-01-01 00:00:00','home',true,false),(2,'2024-01-01 00:01:00','search',false,false),(2,'2024-01-01 00:02:00','product',false,true);",
    )
    .unwrap();
    // forward / first_match: what comes after the first home?
    let rows: Vec<(i32, String)> = db
        .conn()
        .prepare(
            "SELECT user_id, sequence_next_node('forward','first_match', ts, page, is_home, is_home) \
             FROM p GROUP BY user_id ORDER BY user_id",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        rows,
        vec![(1, "product".to_string()), (2, "search".to_string())]
    );

    // backward / first_match: what comes before the first product?
    let back: Vec<(i32, String)> = db
        .conn()
        .prepare(
            "SELECT user_id, sequence_next_node('backward','first_match', ts, page, is_product, is_product) \
             FROM p GROUP BY user_id ORDER BY user_id",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        back,
        vec![(1, "home".to_string()), (2, "search".to_string())]
    );
}

/// Invalid configuration raises real SQL errors (via
/// `duckdb_aggregate_function_set_error`) instead of silently producing wrong
/// results or NULL: unknown funnel modes, month-based or negative intervals,
/// malformed sequence patterns, and unknown `sequence_next_node`
/// direction/base values.
#[test]
fn invalid_configuration_raises_sql_errors() {
    let db = load_extension();
    db.execute_batch(
        "CREATE TABLE inv(ts TIMESTAMP, event VARCHAR);
         INSERT INTO inv VALUES
            ('2024-01-01 00:00:00','view'), ('2024-01-01 00:05:00','cart');",
    )
    .unwrap();

    let cases: &[(&str, &str)] = &[
        (
            "SELECT window_funnel(INTERVAL '1 hour', 'strict_typo', ts, \
                event='view', event='cart') FROM inv",
            "unknown mode 'strict_typo'",
        ),
        (
            "SELECT window_funnel(INTERVAL '1 month', ts, \
                event='view', event='cart') FROM inv",
            "month-based intervals are ambiguous",
        ),
        (
            "SELECT window_funnel(INTERVAL '-1 hour', ts, \
                event='view', event='cart') FROM inv",
            "must be non-negative",
        ),
        (
            "SELECT sessionize(ts, INTERVAL '1 month') OVER (ORDER BY ts) FROM inv",
            "month-based intervals are ambiguous",
        ),
        (
            "SELECT sequence_match('(?1)(?', ts, event='view', event='cart') FROM inv",
            "invalid sequence pattern '(?1)(?'",
        ),
        (
            "SELECT sequence_count('(?1)(?', ts, event='view', event='cart') FROM inv",
            "invalid sequence pattern",
        ),
        (
            "SELECT sequence_match_events('(?1)x(?2)', ts, \
                event='view', event='cart') FROM inv",
            "invalid sequence pattern",
        ),
        (
            "SELECT sequence_next_node('sideways', 'head', ts, event, \
                event='view', event='cart') FROM inv",
            "unknown direction 'sideways'",
        ),
        (
            "SELECT sequence_next_node('forward', 'middle', ts, event, \
                event='view', event='cart') FROM inv",
            "unknown base 'middle'",
        ),
    ];

    for (sql, expected) in cases {
        let err = db
            .query_one::<i64>(sql)
            .expect_err(&format!("query must fail: {sql}"));
        let msg = err.to_string();
        assert!(
            msg.contains(expected),
            "error for `{sql}`\n  expected substring: {expected}\n  actual: {msg}"
        );
    }
}

/// `DuckDB`'s special `±infinity` timestamps are `i64::MAX` / `i64::MIN+1`
/// internally. Gap arithmetic must saturate rather than wrap: a gap that
/// spans from -infinity to a finite timestamp is infinite, not negative.
#[test]
fn infinity_timestamps_saturate_not_wrap() {
    let db = load_extension();
    db.execute_batch(
        "CREATE TABLE inf_s(ts TIMESTAMP);
         INSERT INTO inf_s VALUES
            (TIMESTAMP '-infinity'), (TIMESTAMP '2024-01-01 00:00:00'), (TIMESTAMP 'infinity');",
    )
    .unwrap();

    // Each gap is infinite, so each row opens a new session.
    let ids: Vec<i64> = db
        .conn()
        .prepare(
            "SELECT sessionize(ts, INTERVAL '30 minutes') OVER (ORDER BY ts) FROM inf_s ORDER BY ts",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(ids, vec![1, 2, 3], "infinite gaps must open new sessions");

    // The second event is infinitely far from the entry: funnel stops at step 1.
    db.execute_batch(
        "CREATE TABLE inf_f(ts TIMESTAMP, a BOOLEAN, b BOOLEAN);
         INSERT INTO inf_f VALUES
            (TIMESTAMP '-infinity', true, false),
            (TIMESTAMP '2024-01-01 00:00:00', false, true);",
    )
    .unwrap();
    let step: i32 = db
        .query_one("SELECT window_funnel(INTERVAL '1 hour', ts, a, b) FROM inf_f")
        .unwrap();
    assert_eq!(step, 1, "an infinitely distant event is outside any window");

    // The elapsed time from -infinity is enormous, so (?t>=1) must hold.
    let matched: bool = db
        .query_one("SELECT sequence_match('(?1)(?t>=1)(?2)', ts, a, b) FROM inf_f")
        .unwrap();
    assert!(matched, "elapsed time from -infinity satisfies (?t>=1)");
}

/// Time-constraint numbers larger than `i64::MAX` must raise a pattern error
/// instead of wrapping negative through an unchecked cast.
#[test]
fn time_constraint_over_i64_max_is_rejected() {
    let db = load_extension();
    db.execute_batch(
        "CREATE TABLE big_t(ts TIMESTAMP, a BOOLEAN, b BOOLEAN);
         INSERT INTO big_t VALUES (TIMESTAMP '2024-01-01 00:00:00', true, false),
                                  (TIMESTAMP '2024-01-01 00:00:05', false, true);",
    )
    .unwrap();
    let err = db
        .query_one::<bool>(
            "SELECT sequence_match('(?1)(?t>=18446744073709551615)(?2)', ts, a, b) FROM big_t",
        )
        .expect_err("oversized time constraint must fail");
    assert!(
        err.to_string().contains("invalid sequence pattern"),
        "actual error: {err}"
    );
}

/// Pins the ClickHouse-parity semantics of `sequence_next_node` across all
/// eight direction/base combinations through live SQL, mirroring
/// `test/sql/sequence_next_node.test`: head/tail anchor at the literal
/// first/last event, chains match consecutive events only, and a failed
/// chain is not retried at other anchors.
#[test]
fn sequence_next_node_direction_base_matrix() {
    let db = load_extension();
    db.execute_batch(
        "CREATE TABLE pm(user_id INTEGER, ts TIMESTAMP, page VARCHAR, is_home BOOLEAN, is_product BOOLEAN);
         INSERT INTO pm VALUES
            (1,'2024-01-01 00:00:00','home',true,false),
            (1,'2024-01-01 00:01:00','product',false,true),
            (1,'2024-01-01 00:02:00','cart',false,false),
            (1,'2024-01-01 00:03:00','checkout',false,false),
            (2,'2024-01-01 00:00:00','home',true,false),
            (2,'2024-01-01 00:01:00','search',false,false),
            (2,'2024-01-01 00:02:00','product',false,true);",
    )
    .unwrap();

    let run = |dir: &str, base: &str, cond: &str| -> Vec<Option<String>> {
        db.conn()
            .prepare(&format!(
                "SELECT sequence_next_node('{dir}', '{base}', ts, page, {cond}, {cond}) \
                 FROM pm GROUP BY user_id ORDER BY user_id"
            ))
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    let some = |v: &str| Some(v.to_string());

    // forward
    assert_eq!(
        run("forward", "head", "is_home"),
        vec![some("product"), some("search")]
    );
    // ("forward", "tail") is an error: see sequence_next_node_rejects_end_anchored_chains.
    assert_eq!(
        run("forward", "first_match", "is_home"),
        vec![some("product"), some("search")]
    );
    assert_eq!(
        run("forward", "last_match", "is_home"),
        vec![some("product"), some("search")]
    );
    // backward
    // ("backward", "head") is an error: see sequence_next_node_rejects_end_anchored_chains.
    assert_eq!(
        run("backward", "tail", "is_product"),
        vec![None, some("search")]
    );
    assert_eq!(
        run("backward", "first_match", "is_product"),
        vec![some("home"), some("search")]
    );
    assert_eq!(
        run("backward", "last_match", "is_product"),
        vec![some("home"), some("search")]
    );

    // Consecutive-chain contract: home -> product is adjacent for user 1
    // (cart follows), but user 2 has 'search' between home and product.
    let chains: Vec<Option<String>> = db
        .conn()
        .prepare(
            "SELECT sequence_next_node('forward', 'first_match', ts, page, \
                is_home, is_home, is_product) \
             FROM pm GROUP BY user_id ORDER BY user_id",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        chains,
        vec![some("cart"), None],
        "gap events break the chain"
    );
}

/// Parallel hash aggregation combines partial states in nondeterministic
/// order. With the (timestamp, conditions) sort key, same-timestamp ties have
/// one canonical order, so results must be identical across thread counts and
/// physical insertion orders.
#[test]
fn parallel_combine_determinism_under_ties() {
    let db = load_extension();
    // 200 users x 60 events; every user has bursts of same-timestamp events
    // carrying different conditions (the tie-heavy worst case).
    db.execute_batch(
        "CREATE TABLE det AS
         SELECT (i % 200) AS user_id,
                TIMESTAMP '2024-01-01 00:00:00' + INTERVAL (((i // 200) % 20) || ' minutes') AS ts,
                ((i * 7) % 3 = 0) AS c1,
                ((i * 11) % 3 = 1) AS c2,
                ((i * 13) % 3 = 2) AS c3
         FROM range(12000) t(i);
         CREATE TABLE det_rev AS SELECT * FROM det ORDER BY user_id DESC, ts DESC, c1, c2, c3;",
    )
    .unwrap();

    let funnel_hash = |table: &str, threads: i32| -> String {
        db.execute_batch(&format!("SET threads={threads}")).unwrap();
        db.query_one(&format!(
            "SELECT md5(string_agg(step::VARCHAR, ',' ORDER BY user_id)) FROM (
                SELECT user_id, window_funnel(INTERVAL '1 hour', ts, c1, c2, c3) AS step
                FROM {table} GROUP BY user_id)"
        ))
        .unwrap()
    };

    let base = funnel_hash("det", 1);
    assert_eq!(
        funnel_hash("det", 4),
        base,
        "thread count must not change results"
    );
    assert_eq!(
        funnel_hash("det_rev", 4),
        base,
        "physical row order must not change results"
    );

    let count_hash = |table: &str, threads: i32| -> String {
        db.execute_batch(&format!("SET threads={threads}")).unwrap();
        db.query_one(&format!(
            "SELECT md5(string_agg(c::VARCHAR, ',' ORDER BY user_id)) FROM (
                SELECT user_id, sequence_count('(?1).*(?2)', ts, c1, c2) AS c
                FROM {table} GROUP BY user_id)"
        ))
        .unwrap()
    };
    let base = count_hash("det", 1);
    assert_eq!(count_hash("det", 4), base);
    assert_eq!(count_hash("det_rev", 4), base);
}

/// `window_funnel` works as a windowed aggregate (`DuckDB` windows any
/// aggregate): a running funnel over an expanding frame is monotonically
/// non-decreasing and ends at the full GROUP BY result.
#[test]
fn window_funnel_as_windowed_aggregate() {
    let db = load_extension();
    db.execute_batch(
        "CREATE TABLE wf(ts TIMESTAMP, event VARCHAR);
         INSERT INTO wf VALUES
            ('2024-01-01 00:00:00','view'),
            ('2024-01-01 00:05:00','cart'),
            ('2024-01-01 00:10:00','purchase');",
    )
    .unwrap();
    let running: Vec<i32> = db
        .conn()
        .prepare(
            "SELECT window_funnel(INTERVAL '1 hour', ts, \
                event='view', event='cart', event='purchase') \
             OVER (ORDER BY ts ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) \
             FROM wf ORDER BY ts",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(running, vec![1, 2, 3]);
}

/// The query shapes that crashed every C API aggregate before `DuckDB` 2.0.0
/// (duckdb/duckdb#26109, fixed by duckdb/duckdb#26455): `ORDER BY` inside the
/// call, `OVER ()`, `UNBOUNDED PRECEDING .. UNBOUNDED FOLLOWING`, and
/// `OVER (PARTITION BY g)` with no `ORDER BY`. Runs all eight aggregates through
/// each shape in-process and requires the windowed result to equal the plain
/// `GROUP BY` result. `sessionize` is only required not to crash: its value
/// depends on the row's place in the order, so it has no `GROUP BY` equivalent.
///
/// The in-process `DuckDB` is the release `libduckdb-sys` links (2.0.0). The
/// shapes still crash 1.x releases, so the test runs only when the linked
/// library is 2.x or later; `scripts/crash-shapes.sh` maps the older releases.
#[test]
fn issue_26109_query_shapes_work_on_duckdb_2() {
    let db = load_extension();
    let version: String = db
        .query_one("SELECT library_version FROM pragma_version()")
        .unwrap();
    let major: u32 = version
        .trim_start_matches('v')
        .split('.')
        .next()
        .and_then(|m| m.parse().ok())
        .unwrap_or_else(|| panic!("unparsable DuckDB version {version:?}"));
    if major < 2 {
        eprintln!("skipped: DuckDB {version} crashes on these shapes (duckdb/duckdb#26109)");
        return;
    }
    db.execute_batch(
        "CREATE TABLE t AS SELECT i % 2 AS g, TIMESTAMP '2024-01-01' + to_seconds(i) AS ts,
            i % 2 = 0 AS a, i % 3 = 0 AS b, 'v' || i AS v FROM range(10) r(i);",
    )
    .unwrap();
    // (name, arguments). The ORDER BY shape appends `ORDER BY ts` to the arguments.
    let calls: [(&str, &str); 8] = [
        ("sessionize", "ts, INTERVAL 5 SECOND"),
        ("retention", "a, b"),
        ("window_funnel", "INTERVAL 1 HOUR, ts, a, b"),
        ("window_funnel_events", "INTERVAL 1 HOUR, ts, a, b"),
        ("sequence_match", "'(?1).*(?2)', ts, a, b"),
        ("sequence_count", "'(?1).*(?2)', ts, a, b"),
        ("sequence_match_events", "'(?1).*(?2)', ts, a, b"),
        ("sequence_next_node", "'forward', 'head', ts, v, a, a"),
    ];
    let windows = [
        "OVER ()",
        "OVER (ORDER BY ts ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING)",
        "OVER (PARTITION BY g)",
    ];
    for (name, args) in calls {
        let plain = format!("{name}({args})");
        // `ORDER BY` inside the call: one row per group, equal to the plain result.
        let ordered: i64 = db
            .query_one(&format!(
                "SELECT count(*) FROM (SELECT g, {name}({args} ORDER BY ts) AS r FROM t GROUP BY g) o
                 JOIN (SELECT g, {plain} AS r FROM t GROUP BY g) p USING (g)
                 WHERE o.r IS NOT DISTINCT FROM p.r"
            ))
            .unwrap_or_else(|e| panic!("{name}: ORDER BY inside the call failed: {e}"));
        assert_eq!(ordered, 2, "{name}: ORDER BY inside the call");
        for window in windows {
            let sql = format!("SELECT count(r) FROM (SELECT {plain} {window} AS r FROM t)");
            let rows: i64 = db
                .query_one(&sql)
                .unwrap_or_else(|e| panic!("{name} {window} failed: {e}"));
            // `count` skips NULL; a sessionize id is never NULL here.
            if name == "sessionize" {
                assert_eq!(rows, 10, "{name} {window}");
                continue;
            }
            // Rows whose windowed result differs from the GROUP BY result of
            // their partition (the whole table for the unpartitioned shapes).
            let partitioned = window.contains("PARTITION BY g");
            let check = if partitioned {
                format!(
                    "SELECT count(*) FROM (SELECT g, {plain} {window} AS r FROM t) w
                     JOIN (SELECT g, {plain} AS r FROM t GROUP BY g) a USING (g)
                     WHERE w.r IS DISTINCT FROM a.r"
                )
            } else {
                format!(
                    "SELECT count(*) FROM (SELECT {plain} {window} AS r FROM t) w
                     WHERE w.r IS DISTINCT FROM (SELECT {plain} FROM t)"
                )
            };
            let mismatches: i64 = db
                .query_one(&check)
                .unwrap_or_else(|e| panic!("{name} {window} comparison failed: {e}"));
            assert_eq!(
                mismatches, 0,
                "{name} {window}: rows differing from GROUP BY"
            );
        }
    }
}

/// `sequence_next_node` returns the next value byte for byte: an embedded NUL
/// neither ends the string nor shortens it, an empty string is not `NULL`,
/// multi-byte text survives, and a 100 kB value passes through the value
/// buffer and into the result vector unchanged.
#[test]
fn sequence_next_node_returns_awkward_values_whole() {
    let db = load_extension();
    for value in [
        "'a' || chr(0) || 'b'",
        "''",
        "'h\u{e9}llo \u{2713}'",
        "repeat('x', 100000)",
    ] {
        let (same, bytes): (bool, i64) = db
            .conn()
            .query_row(
                &format!(
                    "SELECT r = ({value}), strlen(r) FROM (
                         SELECT sequence_next_node('forward', 'head', ts, v, c, c) AS r
                         FROM (VALUES (TIMESTAMP '2024-01-01 00:00:00', 'start', true),
                                      (TIMESTAMP '2024-01-01 00:00:01', {value}, false))
                              t(ts, v, c))"
                ),
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap_or_else(|e| panic!("value {value}: {e}"));
        assert!(same, "value {value} was altered");
        let expected: i64 = db.query_one(&format!("SELECT strlen({value})")).unwrap();
        assert_eq!(bytes, expected, "value {value}");
    }
}

/// A window ordered by descending timestamp, or by another column, used to
/// give silently wrong session IDs (every step back in time read as a huge
/// gap, so each row opened a session). It is now an error; ties and ascending
/// order are not.
#[test]
fn sessionize_rejects_rows_out_of_timestamp_order() {
    let db = load_extension();
    db.execute_batch(
        "CREATE TABLE ord AS SELECT i, TIMESTAMP '2024-01-01' + to_seconds(i % 3 * 100 + i) AS ts
         FROM range(6) t(i);",
    )
    .unwrap();
    for order in ["ts DESC", "i DESC"] {
        let err = db
            .query_one::<i64>(&format!(
                "SELECT sum(s) FROM (SELECT sessionize(ts, INTERVAL 10 SECOND)
                     OVER (ORDER BY {order}) AS s FROM ord)"
            ))
            .unwrap_err()
            .to_string();
        assert!(err.contains("ascending timestamp order"), "{order}: {err}");
    }
    // Ascending order, with ties, is fine: three rows share each timestamp.
    let sessions: i64 = db
        .query_one(
            "SELECT max(s) FROM (SELECT sessionize(ts, INTERVAL 10 SECOND)
                 OVER (ORDER BY ts) AS s
             FROM (VALUES (TIMESTAMP '2024-01-01'), (TIMESTAMP '2024-01-01'),
                          (TIMESTAMP '2024-01-01 00:01:00')) t(ts))",
        )
        .unwrap();
    assert_eq!(sessions, 2);
}

/// Without `OVER`, `sessionize` is a plain aggregate whose thread-local
/// states `DuckDB` merges in any order, so partial states can meet out of
/// timestamp order. That used to give a different session count per run (8 or
/// 9 sessions for one that is exactly 1, at four threads); combine now rejects
/// the misordered merge. Scheduling decides whether a given run merges in
/// order, so the invariant is "correct or an error, never a silently different
/// answer".
#[test]
fn sessionize_plain_aggregate_is_correct_or_an_error_never_wrong() {
    let db = load_extension();
    db.execute_batch(
        "SET threads = 4;
         CREATE TABLE sorted AS SELECT TIMESTAMP '2024-01-01' + to_seconds(i * 60) AS ts
         FROM range(1000000) t(i);",
    )
    .unwrap();
    for run in 0..8 {
        match db.query_one::<i64>("SELECT sessionize(ts, INTERVAL 2 MINUTE) FROM sorted") {
            Ok(sessions) => assert_eq!(sessions, 1, "run {run}: a silently wrong count"),
            Err(e) => assert!(
                e.to_string().contains("ascending timestamp order"),
                "run {run}: {e}"
            ),
        }
    }
}

/// `sessionize` over every kind of window frame `DuckDB` offers, against a
/// reference computed from the frame's own rows: the number of gaps above the
/// threshold in the list of timestamps the frame holds, plus one. Running,
/// sliding, forward-looking, `EXCLUDE`, `RANGE` and `GROUPS` frames take
/// different paths through `DuckDB`'s window machinery (a running state, a
/// segment tree whose nodes are combined in a different order); the
/// partitions are large enough (3,000 rows) to need several tree levels.
/// Timestamps are unique within a partition, so row order is not ambiguous.
#[test]
fn sessionize_matches_a_list_reference_in_every_window_frame() {
    let db = load_extension();
    db.execute_batch(
        "SET threads = 4;
         CREATE TABLE ev AS SELECT i % 2 AS g,
             TIMESTAMP '2024-01-01' + to_seconds((i * 7919) % 100003) AS ts
         FROM range(6000) t(i);",
    )
    .unwrap();
    let frames = [
        "",
        "ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW",
        "ROWS BETWEEN 5 PRECEDING AND CURRENT ROW",
        "ROWS BETWEEN 10 PRECEDING AND 3 FOLLOWING",
        "ROWS BETWEEN CURRENT ROW AND UNBOUNDED FOLLOWING",
        "ROWS BETWEEN 2000 PRECEDING AND 1500 FOLLOWING",
        "ROWS BETWEEN 3 PRECEDING AND 3 FOLLOWING EXCLUDE CURRENT ROW",
        "RANGE BETWEEN INTERVAL 3000 SECOND PRECEDING AND CURRENT ROW",
        "GROUPS BETWEEN 2 PRECEDING AND 1 FOLLOWING",
    ];
    for frame in frames {
        let (mismatches, distinct): (i64, i64) = db
            .conn()
            .query_row(
                &format!(
                    "WITH got AS (
                         SELECT g, ts, sessionize(ts, INTERVAL 30 SECOND)
                             OVER (PARTITION BY g ORDER BY ts {frame}) AS v
                         FROM ev),
                     frames AS (
                         SELECT g, ts, list(ts ORDER BY ts)
                             OVER (PARTITION BY g ORDER BY ts {frame}) AS l
                         FROM ev),
                     expected AS (
                         SELECT g, ts, CASE WHEN len(l) = 0 THEN NULL ELSE 1 + len(list_filter(
                             range(2, len(l) + 1),
                             lambda i: l[i] - l[i - 1] > INTERVAL 30 SECOND)) END AS e
                         FROM frames)
                     SELECT count(*) FILTER (WHERE got.v IS DISTINCT FROM expected.e),
                            count(DISTINCT got.v)
                     FROM got JOIN expected USING (g, ts)"
                ),
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap_or_else(|e| panic!("frame [{frame}] failed: {e}"));
        assert_eq!(mismatches, 0, "frame [{frame}]");
        assert!(
            distinct > 3,
            "frame [{frame}] produced one value: {distinct}"
        );
    }
}

/// The shipped release profile keeps panic containment working: every
/// aggregate callback runs under quack-rs's `catch_unwind` guard, which
/// `panic = "abort"` makes inert (a panic would then kill the user's `DuckDB`
/// process instead of failing the query). Checked with quack-rs's own
/// validator against this crate's `[profile.release]`.
#[test]
fn release_profile_keeps_panic_guards_effective() {
    let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .expect("read Cargo.toml");
    let section: Vec<(&str, &str)> = manifest
        .lines()
        .skip_while(|l| l.trim() != "[profile.release]")
        .skip(1)
        .take_while(|l| !l.trim_start().starts_with('['))
        .filter_map(|l| {
            let l = l.split('#').next()?.trim();
            let (k, v) = l.split_once('=')?;
            Some((k.trim(), v.trim().trim_matches('"')))
        })
        .collect();
    let get = |key: &str| {
        section
            .iter()
            .find(|(k, _)| *k == key)
            .map_or("", |(_, v)| *v)
    };
    let check = quack_rs::validate::validate_release_profile(
        get("panic"),
        get("lto"),
        get("opt-level"),
        get("codegen-units"),
    )
    .unwrap_or_else(|e| panic!("[profile.release] rejected: {e}"));
    assert!(check.is_fully_optimized(), "{check:?}");
}

/// A NULL window skips that row, like a NULL timestamp, for both funnel
/// functions. It used to leave the window at its zero default, so
/// `window_funnel(NULL, ..)` returned 1 (a 0-length funnel) instead of
/// ignoring the row.
#[test]
fn null_window_skips_the_row() {
    let db = load_extension();
    db.execute_batch(
        "CREATE TABLE nw(ts TIMESTAMP, event VARCHAR, w INTERVAL);
         INSERT INTO nw VALUES
            ('2024-01-01 00:00:00', 'view',     NULL),
            ('2024-01-01 00:05:00', 'cart',     NULL),
            ('2024-01-01 00:10:00', 'purchase', NULL);",
    )
    .unwrap();
    let funnel = |window: &str| -> i32 {
        db.query_one(&format!(
            "SELECT window_funnel({window}, ts, event='view', event='cart', \
                event='purchase') FROM nw"
        ))
        .unwrap()
    };
    let events = |window: &str| -> String {
        db.query_one(&format!(
            "SELECT window_funnel_events({window}, ts, event='view', event='cart', \
                event='purchase')::VARCHAR FROM nw"
        ))
        .unwrap()
    };
    // Every row has a NULL window: every row is skipped.
    assert_eq!(funnel("w"), 0);
    assert_eq!(funnel("NULL::INTERVAL"), 0);
    assert_eq!(events("w"), "[]");

    // Only the 'cart' row has a NULL window: it is skipped, so the funnel
    // stops after 'view' even though 'purchase' is within the window.
    db.execute_batch("UPDATE nw SET w = INTERVAL '1 hour' WHERE event <> 'cart';")
        .unwrap();
    assert_eq!(funnel("w"), 1);
    assert_eq!(events("w"), "['2024-01-01 00:00:00']");
}

/// Sequence pattern defects found by differential testing against `ClickHouse`
/// 26.9.8.3, checked through the loaded extension.
#[test]
fn sequence_pattern_clickhouse_parity_fixes() {
    let db = load_extension();
    db.execute_batch(
        "CREATE TABLE sp(ts TIMESTAMP, c1 BOOLEAN, c2 BOOLEAN, c3 BOOLEAN);
         INSERT INTO sp VALUES
            ('2020-01-01 00:00:01', true,  false, false),
            ('2020-01-01 00:00:02', false, false, true),
            ('2020-01-01 00:00:03', false, true,  false),
            ('2020-01-01 00:00:04', false, false, true);",
    )
    .unwrap();
    // Adjacent conditions next to `.*` keep their adjacency (ClickHouse: 0, 0).
    let matched: bool = db
        .query_one("SELECT sequence_match('(?1)(?2).*(?3)', ts, c1, c2, c3) FROM sp")
        .unwrap();
    let count: i64 = db
        .query_one("SELECT sequence_count('(?1)(?2).*(?3)', ts, c1, c2, c3) FROM sp")
        .unwrap();
    assert!(!matched);
    assert_eq!(count, 0);
    // A pattern matching zero events terminates (ClickHouse counts 4 here).
    let count: i64 = db
        .query_one("SELECT sequence_count('.*', ts, c1, c2, c3) FROM sp")
        .unwrap();
    assert_eq!(count, 4);
    // Out-of-range condition numbers and anchorless time constraints are errors.
    for (sql, needle) in [
        (
            "SELECT sequence_match('(?1)(?4)', ts, c1, c2, c3) FROM sp",
            "condition (?4) is out of range",
        ),
        (
            "SELECT sequence_match_events('(?1)(?4)', ts, c1, c2, c3) FROM sp",
            "condition (?4) is out of range",
        ),
        (
            "SELECT sequence_count('(?t>0)(?1)', ts, c1, c2, c3) FROM sp",
            "time constraint must follow an event condition",
        ),
    ] {
        let err = db
            .query_one::<i64>(sql)
            .map(|_| ())
            .unwrap_err()
            .to_string();
        assert!(err.contains(needle), "{sql}: {err}");
    }
}

/// A gap that varies per row: each row starts a new session when its gap to
/// the previous row exceeds that row's own threshold, however `DuckDB`'s
/// segment tree splits the frame. Combine used to judge the boundary between
/// two segments by the left segment's threshold, which changed results from
/// 20 rows on. Checked against a lag-based reference.
#[test]
fn sessionize_varying_gap_matches_reference() {
    let db = load_extension();
    db.execute_batch(
        "CREATE TABLE vg AS SELECT i, TIMESTAMP '2024-01-01' + to_seconds(2 * i) AS ts,
             CASE WHEN i % 2 = 1 THEN INTERVAL 100 SECOND ELSE INTERVAL 0 SECOND END AS gap
         FROM range(200) r(i);",
    )
    .unwrap();
    let mismatches: i64 = db
        .query_one(
            "WITH ext AS (
                 SELECT i, sessionize(ts, gap) OVER (ORDER BY ts ROWS UNBOUNDED PRECEDING) AS s
                 FROM vg),
             starts AS (
                 SELECT i, ts, CASE WHEN ts - lag(ts) OVER (ORDER BY ts) > gap
                                    THEN 1 ELSE 0 END AS new_session
                 FROM vg),
             ref AS (
                 SELECT i, 1 + sum(new_session)
                     OVER (ORDER BY ts ROWS UNBOUNDED PRECEDING) AS s
                 FROM starts)
             SELECT count(*) FROM ext JOIN ref USING (i) WHERE ext.s <> ref.s",
        )
        .unwrap();
    assert_eq!(mismatches, 0);
}

/// `forward` + `tail` and `backward` + `head` start the chain at an end of
/// the sequence, so no adjacent event exists. `ClickHouse` rejects both; the
/// extension used to return NULL silently.
/// A pattern made only of `.*` matches the empty sequence, so it matches a
/// group whose rows all have every condition false (`ClickHouse`'s
/// `sequenceMatch('.*')` returns 1 there). Nothing is counted, and a group
/// with no rows at all is still NULL (a documented difference).
#[test]
fn star_only_pattern_matches_a_group_without_usable_events() {
    let db = load_extension();
    let rows = "(VALUES (TIMESTAMP '2024-01-01', false, false)) t(ts, c1, c2)";
    let matched: bool = db
        .query_one(&format!(
            "SELECT sequence_match('.*', ts, c1, c2) FROM {rows}"
        ))
        .unwrap();
    assert!(matched);
    let count: i64 = db
        .query_one(&format!(
            "SELECT sequence_count('.*', ts, c1, c2) FROM {rows}"
        ))
        .unwrap();
    assert_eq!(count, 0);
    let stepped: bool = db
        .query_one(&format!(
            "SELECT sequence_match('(?1)', ts, c1, c2) FROM {rows}"
        ))
        .unwrap();
    assert!(!stepped, "a pattern with a (?N) step still needs an event");
    let no_rows: Option<bool> = db
        .query_one(&format!(
            "SELECT sequence_match('.*', ts, c1, c2) FROM {rows} WHERE false"
        ))
        .unwrap();
    assert_eq!(no_rows, None);
}

#[test]
fn sequence_next_node_rejects_end_anchored_chains() {
    let db = load_extension();
    db.execute_batch(
        "CREATE TABLE nn AS SELECT * FROM (VALUES
            (TIMESTAMP '2024-01-01 00:00:00', 'a'),
            (TIMESTAMP '2024-01-01 00:00:01', 'b')) t(ts, v);",
    )
    .unwrap();
    for (direction, base) in [("forward", "tail"), ("backward", "head")] {
        let sql = format!(
            "SELECT sequence_next_node('{direction}', '{base}', ts, v, true, v = 'a') FROM nn"
        );
        let err = db
            .query_one::<String>(&sql)
            .map(|_| ())
            .unwrap_err()
            .to_string();
        assert!(err.contains("cannot be combined"), "{sql}: {err}");
    }
}

/// Configuration arguments must be constant within a group. Before 0.10.0
/// the first value a partial state saw won, so the result (and whether an
/// invalid later value raised an error) depended on row order. Each case is
/// run in both row orders and must fail both times.
#[test]
fn differing_configuration_in_one_group_is_an_error() {
    let db = load_extension();
    db.execute_batch(
        "CREATE TABLE cfg AS SELECT * FROM (VALUES
            (1, TIMESTAMP '2024-01-01 00:00:00', 'a', INTERVAL 1 MINUTE, 'strict_order',
             '(?1)(?2)', 'forward', 'first_match'),
            (2, TIMESTAMP '2024-01-01 00:30:00', 'b', INTERVAL 1 HOUR, 'strict_once',
             '(?2)', 'backward', 'last_match'))
          t(i, ts, ev, win, mode, pat, dir, base);",
    )
    .unwrap();
    let cases = [
        ("window", "window_funnel(win, ts, ev = 'a', ev = 'b')"),
        (
            "window",
            "len(window_funnel_events(win, ts, ev = 'a', ev = 'b'))",
        ),
        (
            "mode",
            "window_funnel(INTERVAL 1 HOUR, mode, ts, ev = 'a', ev = 'b')",
        ),
        (
            "pattern",
            "sequence_match(pat, ts, ev = 'a', ev = 'b')::INT",
        ),
        (
            "pattern",
            "sequence_count(pat, ts, ev = 'a', ev = 'b')::INT",
        ),
        (
            "pattern",
            "len(sequence_match_events(pat, ts, ev = 'a', ev = 'b'))",
        ),
        (
            "direction",
            "len(sequence_next_node(dir, 'first_match', ts, ev, true, true))",
        ),
        (
            "base",
            "len(sequence_next_node('forward', base, ts, ev, true, true))",
        ),
    ];
    for (argument, call) in cases {
        for order in ["i", "i DESC"] {
            let sql = format!("SELECT {call} FROM (SELECT * FROM cfg ORDER BY {order})");
            let err = db
                .query_one::<i64>(&sql)
                .map(|v| format!("returned {v}"))
                .unwrap_err()
                .to_string();
            assert!(
                err.contains(&format!("the {argument} argument must be the same")),
                "{sql}: {err}"
            );
        }
    }

    // An invalid value on a later row is an error in either order (it used
    // to be ignored when a valid value came first).
    for order in ["i", "i DESC"] {
        let sql = format!(
            "SELECT window_funnel(INTERVAL 1 HOUR, m, ts, ev = 'a', ev = 'b') FROM
             (SELECT *, CASE i WHEN 1 THEN 'strict_order' ELSE 'bogus_mode' END AS m
              FROM cfg ORDER BY {order})"
        );
        assert!(db.query_one::<i64>(&sql).is_err(), "{sql}");
    }

    // Repeated equal values, equivalent spellings and NULLs are not conflicts.
    let same: i64 = db
        .query_one(
            "SELECT window_funnel(w, m, ts, ev = 'a', ev = 'b') FROM
             (SELECT *, CASE i WHEN 1 THEN INTERVAL 1 HOUR ELSE INTERVAL 60 MINUTE END AS w,
                        CASE i WHEN 1 THEN 'strict_order' ELSE ' STRICT_ORDER ' END AS m
              FROM cfg)",
        )
        .unwrap();
    assert_eq!(same, 2);
    let with_null: bool = db
        .query_one(
            "SELECT sequence_match(CASE i WHEN 1 THEN '(?1)(?2)' END, ts, ev = 'a', ev = 'b')
             FROM cfg",
        )
        .unwrap();
    assert!(with_null);
}

/// The same conflict split across `DuckDB`'s parallel partial aggregates is
/// caught in combine. The pattern alternates per row group (122,880 rows,
/// `DuckDB`'s scan unit), so threads start from different patterns and their
/// partial states disagree when combined. (With the update-time check
/// disabled, the combine check alone raised the error in 3 of 3 runs.)
#[test]
fn differing_pattern_across_threads_is_an_error() {
    let db = load_extension();
    db.execute_batch(
        "SET threads = 4;
         CREATE TABLE big AS SELECT TIMESTAMP '2024-01-01' + to_seconds(i) AS ts,
                CASE WHEN (i // 122880) % 2 = 0 THEN '(?1)(?2)' ELSE '(?2)(?1)' END AS pat,
                i % 2 = 0 AS a, i % 2 = 1 AS b
         FROM range(1000000) r(i);",
    )
    .unwrap();
    let err = db
        .query_one::<bool>("SELECT sequence_match(pat, ts, a, b) FROM big")
        .map(|v| format!("returned {v}"))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("the pattern argument must be the same"),
        "{err}"
    );
}

/// A NULL direction means 'forward' (documented), so with base 'tail' it is
/// the forbidden forward+tail combination. It used to return NULL.
#[test]
fn null_direction_with_tail_base_is_an_error() {
    let db = load_extension();
    let err = db
        .query_one::<String>(
            "SELECT sequence_next_node(NULL, 'tail', ts, v, true, true)
             FROM (VALUES (TIMESTAMP '2024-01-01', 'a')) t(ts, v)",
        )
        .map(|v| format!("returned {v}"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("cannot be combined"), "{err}");
}

/// Mode names are case-insensitive, like `sequence_next_node`'s direction
/// and base.
#[test]
fn funnel_mode_names_are_case_insensitive() {
    let db = load_extension();
    let lower: i64 = db
        .query_one(
            "SELECT window_funnel(INTERVAL 1 HOUR, 'strict_order', ts, c1, c2)
             FROM (VALUES (TIMESTAMP '2024-01-01 00:00', true, false),
                          (TIMESTAMP '2024-01-01 00:01', false, false),
                          (TIMESTAMP '2024-01-01 00:02', false, true)) t(ts, c1, c2)",
        )
        .unwrap();
    let upper: i64 = db
        .query_one(
            "SELECT window_funnel(INTERVAL 1 HOUR, 'Strict_Order', ts, c1, c2)
             FROM (VALUES (TIMESTAMP '2024-01-01 00:00', true, false),
                          (TIMESTAMP '2024-01-01 00:01', false, false),
                          (TIMESTAMP '2024-01-01 00:02', false, true)) t(ts, c1, c2)",
        )
        .unwrap();
    assert_eq!((lower, upper), (1, 1));
}

/// A group's mode applies to all of its rows, including rows read before
/// the first non-NULL mode: under `strict_order` a condition-less row breaks
/// the chain even when it arrived with a NULL mode (earlier builds dropped
/// such rows and returned 2).
#[test]
fn strict_order_sees_condition_less_rows_read_before_the_mode() {
    let db = load_extension();
    db.execute_batch("SET threads = 1").unwrap();
    let steps: i64 = db
        .query_one(
            "SELECT window_funnel(INTERVAL 1 HOUR, m, ts, c1, c2)
             FROM (VALUES (NULL, TIMESTAMP '2024-01-01 00:00', true, false),
                          (NULL, TIMESTAMP '2024-01-01 00:01', false, false),
                          ('strict_order', TIMESTAMP '2024-01-01 00:02', false, true))
                  t(m, ts, c1, c2)",
        )
        .unwrap();
    assert_eq!(steps, 1);
}

/// A group whose timestamps are all NULL has no usable rows, so the
/// sequence functions return what they return for an empty group (NULL),
/// not `false` / `0`.
#[test]
fn all_null_timestamp_group_matches_empty_group() {
    let db = load_extension();
    let both_null: bool = db
        .query_one(
            "SELECT sequence_match('(?1)', ts, c, c) IS NULL
                    AND sequence_count('(?1)', ts, c, c) IS NULL
             FROM (VALUES (NULL::TIMESTAMP, true), (NULL::TIMESTAMP, false)) t(ts, c)",
        )
        .unwrap();
    assert!(both_null);
}

/// Every function accepts `TIMESTAMPTZ` and treats it as the instant it is:
/// under UTC the results equal the `TIMESTAMP` versions, the `*_events`
/// functions return `TIMESTAMPTZ[]`, and events keep their real order across
/// a daylight-saving change (the `::TIMESTAMP` cast users needed before
/// converts to local time and reorders them).
#[test]
fn timestamptz_is_accepted_everywhere() {
    let db = load_extension();
    db.execute_batch(
        "SET TimeZone = 'UTC';
         CREATE TABLE tz AS SELECT TIMESTAMP '2024-01-01' + to_seconds(i * 7) AS ts,
                ts::TIMESTAMPTZ AS tstz, i % 3 = 0 AS a, i % 3 = 1 AS b, (i % 5)::VARCHAR AS v
         FROM range(200) r(i);",
    )
    .unwrap();
    let same: bool = db
        .query_one(
            "SELECT
               window_funnel(INTERVAL 1 MINUTE, ts, a, b)
                 = window_funnel(INTERVAL 1 MINUTE, tstz, a, b)
               AND window_funnel(INTERVAL 1 MINUTE, 'strict_order', ts, a, b)
                 = window_funnel(INTERVAL 1 MINUTE, 'strict_order', tstz, a, b)
               AND window_funnel_events(INTERVAL 1 MINUTE, ts, a, b)
                 = window_funnel_events(INTERVAL 1 MINUTE, tstz, a, b)::TIMESTAMP[]
               AND sequence_match('(?1)(?t<10)(?2)', ts, a, b)
                 = sequence_match('(?1)(?t<10)(?2)', tstz, a, b)
               AND sequence_count('(?1)(?t<10)(?2)', ts, a, b)
                 = sequence_count('(?1)(?t<10)(?2)', tstz, a, b)
               AND sequence_match_events('(?1).*(?2)', ts, a, b)
                 = sequence_match_events('(?1).*(?2)', tstz, a, b)::TIMESTAMP[]
               AND sequence_next_node('forward', 'first_match', ts, v, a, a, b)
                 IS NOT DISTINCT FROM
                   sequence_next_node('forward', 'first_match', tstz, v, a, a, b)
               AND retention(a, b) = retention(a, b)
             FROM tz",
        )
        .unwrap();
    assert!(same);
    let sessions_differ: i64 = db
        .query_one(
            "SELECT count(*) FROM (
               SELECT sessionize(ts, INTERVAL 20 SECOND) OVER (ORDER BY ts) AS s1,
                      sessionize(tstz, INTERVAL 20 SECOND) OVER (ORDER BY ts) AS s2
               FROM tz) WHERE s1 <> s2",
        )
        .unwrap();
    assert_eq!(sessions_differ, 0);
    let types: String = db
        .query_one(
            "SELECT typeof(window_funnel_events(INTERVAL 1 MINUTE, tstz, a, b)) || ',' ||
                    typeof(sequence_match_events('(?1).*(?2)', tstz, a, b))
             FROM tz",
        )
        .unwrap();
    assert_eq!(
        types,
        "TIMESTAMP WITH TIME ZONE[],TIMESTAMP WITH TIME ZONE[]"
    );

    // 05:30Z then 06:10Z on 2020-11-01 are 01:30 EDT then 01:10 EST: casting
    // to local TIMESTAMP reverses them, TIMESTAMPTZ keeps them in order.
    db.execute_batch(
        "SET TimeZone = 'America/New_York';
         CREATE TABLE dst AS SELECT * FROM (VALUES
           (TIMESTAMPTZ '2020-11-01 05:30:00+00', true, false),
           (TIMESTAMPTZ '2020-11-01 06:10:00+00', false, true)) t(ts, a, b);",
    )
    .unwrap();
    let (local, instant): (bool, bool) = (
        db.query_one("SELECT sequence_match('(?1)(?2)', ts::TIMESTAMP, a, b) FROM dst")
            .unwrap(),
        db.query_one("SELECT sequence_match('(?1)(?2)', ts, a, b) FROM dst")
            .unwrap(),
    );
    assert!(!local, "the local-time cast reorders the events");
    assert!(instant);
}
