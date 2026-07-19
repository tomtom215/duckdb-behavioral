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
//! 3. Open `InMemoryDb::open_unsigned()`, relax the metadata-mismatch check, and
//!    `LOAD` the artifact.
//! 4. Run SQL covering all seven functions and assert on the results — the same
//!    expectations encoded in `test/sql/*.test`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use quack_rs::testing::InMemoryDb;

/// `DuckDB` release version this extension targets (the `-dv` metadata field for
/// the `C_STRUCT_UNSTABLE` ABI). Kept in sync with the `Makefile` /
/// `.github/workflows/e2e.yml`.
const DUCKDB_VERSION: &str = "v1.5.4";
/// Extension version metadata field (`-ev`); matches `Cargo.toml`'s `version`.
const EXTENSION_VERSION: &str = concat!("v", env!("CARGO_PKG_VERSION"));
/// ABI type for a quack-rs / `libduckdb-sys` C-struct extension.
const ABI_TYPE: &str = "C_STRUCT_UNSTABLE";

/// The `DuckDB` platform triple for the host target. With
/// `allow_extensions_metadata_mismatch=true` the value need not match the host
/// exactly, but we still emit the correct one so the artifact is identical to
/// what CI ships.
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
/// The signed artifact name is namespaced by process id. `cargo nextest` runs
/// each test in its own process, so several may build/sign concurrently; a
/// per-pid filename keeps each writer on its own file (the underlying
/// `cargo build` is itself idempotent and serialized by cargo's own lock).
fn extension_path() -> &'static Path {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let raw = build_release_cdylib();
        let out = raw.with_file_name(format!(
            "behavioral.test.{}.duckdb_extension",
            std::process::id()
        ));
        append_metadata(&raw, &out);
        out
    })
    .as_path()
}

/// Opens a fresh in-memory `DuckDB` with the behavioral extension loaded.
fn load_extension() -> InMemoryDb {
    let db = InMemoryDb::open_unsigned().expect("open in-memory DuckDB (unsigned)");
    // Locally-built artifacts carry host platform / version metadata that need
    // not match the in-process DuckDB; relax the check (mirrors the documented
    // `InMemoryDb::open_unsigned` workflow).
    db.execute_batch("SET allow_extensions_metadata_mismatch=true")
        .expect("relax metadata mismatch");
    let load = format!("LOAD '{}'", extension_path().display());
    db.execute_batch(&load)
        .unwrap_or_else(|e| panic!("LOAD failed for {}: {e}", extension_path().display()));
    db
}

/// The extension loads without error and registers all eight aggregate
/// functions plus the `behavioral_version()` diagnostic scalar in the catalog. A missing registration here is exactly the class of bug that passed
/// the unit suite while shipping a broken extension.
#[test]
fn loads_and_registers_all_functions() {
    let db = load_extension();
    for func in [
        "sessionize",
        "retention",
        "window_funnel",
        "window_funnel_events",
        "sequence_match",
        "sequence_count",
        "sequence_match_events",
        "sequence_next_node",
        "behavioral_version",
    ] {
        let count: i64 = db
            .query_one(&format!(
                "SELECT count(*) FROM duckdb_functions() WHERE function_name = '{func}'"
            ))
            .unwrap_or_else(|e| panic!("catalog query for {func} failed: {e}"));
        assert!(count >= 1, "function `{func}` is not registered");
    }
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
    assert_eq!(
        run("forward", "tail", "is_home"),
        vec![None, None],
        "tail = literal last event"
    );
    assert_eq!(
        run("forward", "first_match", "is_home"),
        vec![some("product"), some("search")]
    );
    assert_eq!(
        run("forward", "last_match", "is_home"),
        vec![some("product"), some("search")]
    );
    // backward
    assert_eq!(
        run("backward", "head", "is_product"),
        vec![None, None],
        "head = literal first event"
    );
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
