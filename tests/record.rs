// Integration tests for ctxl record (AC-1370-*)
#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used)] // integration tests

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Create a fully initialised v3 session DB in a temp directory.
/// Returns (TempDir, Connection) — keep TempDir alive for the duration.
fn setup_calls_db(session_id: &str) -> (tempfile::TempDir, rusqlite::Connection) {
    let tmp = tempfile::TempDir::new().unwrap();
    ctxl::db::init_session_at(tmp.path(), session_id).unwrap();
    let db_path = ctxl::db::session_dir_at(tmp.path(), session_id).join("store.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    (tmp, conn)
}

/// Insert a minimal row into the calls table for seeding tests.
fn insert_call(conn: &rusqlite::Connection, tool: &str, created_at: i64) {
    conn.execute(
        "INSERT INTO calls (tool, intercepted, created_at) VALUES (?1, 0, ?2)",
        rusqlite::params![tool, created_at],
    )
    .unwrap();
}

// ---------------------------------------------------------------------------
// AC-1370-01
// ---------------------------------------------------------------------------

// @ac AC-1370-01
#[test]
fn schema_migration_adds_calls_table() {
    // Verify: ctxl init on a v2 DB creates calls table with full plan DDL
    // Verify: id INTEGER PRIMARY KEY AUTOINCREMENT, tool TEXT NOT NULL, tool_use_id TEXT, params TEXT, cwd TEXT
    // Verify: intercepted BOOLEAN NOT NULL, handle_id TEXT, line_count INTEGER, token_est INTEGER, duration_ms INTEGER, exit_code INTEGER
    // Verify: created_at INTEGER NOT NULL (unix epoch seconds, matching handles.created_at format)
    // Verify: indexes idx_calls_tool and idx_calls_handle present
    // Verify: PRAGMA user_version advances to 3
    // Verify: migration is idempotent — v3 DB is a no-op

    // ---- Build a v2 database (handles table, user_version=2, no calls table) ----
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE handles (
            id TEXT PRIMARY KEY NOT NULL,
            tool TEXT NOT NULL,
            output_mode TEXT NOT NULL,
            params TEXT,
            cwd TEXT,
            content TEXT,
            line_count INTEGER,
            token_est INTEGER,
            created_at INTEGER NOT NULL
        );
        "#,
    )
    .unwrap();
    conn.pragma_update(None, "user_version", 2i32).unwrap();

    // ---- Apply schema (includes v3 migration) ----
    ctxl::db::apply_schema(&conn).unwrap();

    // ---- version advances to 3 ----
    let ver: i32 = conn.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
    assert_eq!(ver, 7, "user_version should be 7 after migration");

    // ---- calls table exists ----
    let table_count: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='calls'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(table_count, 1, "calls table should exist");

    // ---- Verify full column set by inserting a row with all columns ----
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
        as i64;
    conn.execute(
        "INSERT INTO calls \
         (tool, tool_use_id, params, cwd, intercepted, handle_id, line_count, token_est, \
          duration_ms, exit_code, created_at) \
         VALUES ('Read', 'tu_001', '{}', '/tmp', 0, NULL, 10, 3, 42, 0, ?1)",
        rusqlite::params![now],
    )
    .unwrap();

    let (tool, intercepted, created_at): (String, bool, i64) = conn
        .query_row("SELECT tool, intercepted, created_at FROM calls WHERE id=1", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap();
    assert_eq!(tool, "Read");
    assert!(!intercepted, "intercepted should be false (0)");
    assert_eq!(created_at, now);

    // ---- Both indexes present ----
    let idx_count: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='index' \
             AND name IN ('idx_calls_tool', 'idx_calls_handle')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(idx_count, 2, "both indexes (idx_calls_tool, idx_calls_handle) should exist");

    // ---- Idempotent: calling again on a v3 DB is a no-op ----
    ctxl::db::apply_schema(&conn).unwrap();
    let ver2: i32 = conn.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
    assert_eq!(ver2, 7, "idempotent: version should still be 7");
}

// ---------------------------------------------------------------------------
// AC-1370-02
// ---------------------------------------------------------------------------

// @ac AC-1370-02
#[test]
fn record_inserts_call_metadata() {
    // Verify: valid PostToolUse JSON on stdin with --session-id inserts a row into calls
    // Verify: opens session DB via open_session_db() respecting CTXL_CACHE_ROOT
    // Verify: sets PRAGMA busy_timeout=5000
    // Verify: intercepted=false, handle_id=NULL, correct field mapping
    // Verify: works for all recording-tier tools (Read, Edit, Write, Glob)
    // Verify: for tools without tool_response.content (Edit, Write), line_count and token_est are NULL/0
    // Verify: exit 0

    let (_tmp, mut conn) = setup_calls_db("record-meta-test");

    // ---- Read with string content ----
    let read_input = r#"{
        "tool_name": "Read",
        "tool_use_id": "toolu_abc123",
        "tool_input": {"file_path": "/src/main.rs"},
        "tool_response": {"content": "fn main() {\n    println!(\"hello\");\n}\n"}
    }"#;
    ctxl::record::record(&mut conn, read_input).unwrap();

    let (tool, tool_use_id, intercepted, handle_id, line_count, token_est): (
        String,
        Option<String>,
        bool,
        Option<String>,
        Option<i64>,
        Option<i64>,
    ) = conn
        .query_row(
            "SELECT tool, tool_use_id, intercepted, handle_id, line_count, token_est \
             FROM calls ORDER BY id DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .unwrap();

    assert_eq!(tool, "Read");
    assert_eq!(tool_use_id.as_deref(), Some("toolu_abc123"));
    assert!(!intercepted, "intercepted must be false");
    assert!(handle_id.is_none(), "handle_id must be NULL");
    assert_eq!(line_count, Some(3), "3 lines in content");
    assert!(token_est.is_some(), "token_est should be populated for Read");

    // ---- Glob with content ----
    let glob_input = r#"{
        "tool_name": "Glob",
        "tool_use_id": "toolu_glob01",
        "tool_input": {"pattern": "**/*.rs"},
        "tool_response": {"content": "src/main.rs\nsrc/lib.rs\n"}
    }"#;
    ctxl::record::record(&mut conn, glob_input).unwrap();

    let (tool2, lc2): (String, Option<i64>) = conn
        .query_row("SELECT tool, line_count FROM calls ORDER BY id DESC LIMIT 1", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(tool2, "Glob");
    assert_eq!(lc2, Some(2), "2 lines in Glob content");

    // ---- Edit (no content → line_count and token_est are NULL) ----
    let edit_input = r#"{
        "tool_name": "Edit",
        "tool_use_id": "toolu_edit01",
        "tool_input": {"file_path": "/src/lib.rs", "old_string": "foo", "new_string": "bar"},
        "tool_response": {}
    }"#;
    ctxl::record::record(&mut conn, edit_input).unwrap();

    let (tool3, lc3, te3): (String, Option<i64>, Option<i64>) = conn
        .query_row(
            "SELECT tool, line_count, token_est FROM calls ORDER BY id DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(tool3, "Edit");
    assert!(lc3.is_none(), "Edit has no content → line_count must be NULL");
    assert!(te3.is_none(), "Edit has no content → token_est must be NULL");

    // ---- Write (no content) ----
    let write_input = r#"{
        "tool_name": "Write",
        "tool_use_id": "toolu_write01",
        "tool_input": {"file_path": "/out.txt", "content": "data"},
        "tool_response": {}
    }"#;
    ctxl::record::record(&mut conn, write_input).unwrap();

    let tool4: String = conn
        .query_row("SELECT tool FROM calls ORDER BY id DESC LIMIT 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(tool4, "Write");

    // ---- Verify total row count ----
    let total: i64 = conn.query_row("SELECT count(*) FROM calls", [], |r| r.get(0)).unwrap();
    assert_eq!(total, 4, "should have 4 recorded calls");
}

// ---------------------------------------------------------------------------
// AC-1370-03
// ---------------------------------------------------------------------------

// @ac AC-1370-03
#[test]
fn calls_returns_bounded_history() {
    // Verify: given 30 recorded calls and --session-id, ctxl calls --last 10 returns exactly 10 rows
    // Verify: rows returned in reverse-chronological order (ordered by id DESC)
    // Verify: ctxl calls --tool Grep returns only Grep calls
    // Verify: default (no flags) returns last 20

    let (_tmp, conn) = setup_calls_db("calls-history-test");

    // Insert 20 Read calls and 10 Grep calls (30 total), staggered by created_at
    for i in 0i64..20 {
        insert_call(&conn, "Read", 1_700_000_000 + i);
    }
    for i in 0i64..10 {
        insert_call(&conn, "Grep", 1_700_000_100 + i);
    }

    // ---- --last 10 returns exactly 10 rows ----
    let opts = ctxl::retrieve::CallsOpts { last: 10, tool: None, intercepted: None };
    let rows = ctxl::retrieve::calls(&conn, opts).unwrap();
    assert_eq!(rows.len(), 10, "--last 10 should return exactly 10 rows");

    // ---- rows are in reverse-chronological order (id DESC) ----
    let ids: Vec<i64> = rows
        .iter()
        .map(|s| {
            serde_json::from_str::<serde_json::Value>(s)
                .unwrap()
                .get("id")
                .and_then(|v| v.as_i64())
                .unwrap()
        })
        .collect();
    let mut sorted_desc = ids.clone();
    sorted_desc.sort_unstable_by(|a, b| b.cmp(a));
    assert_eq!(ids, sorted_desc, "rows must be ordered by id DESC");

    // ---- --tool Grep returns only Grep rows ----
    let grep_opts =
        ctxl::retrieve::CallsOpts { last: 100, tool: Some("Grep".to_string()), intercepted: None };
    let grep_rows = ctxl::retrieve::calls(&conn, grep_opts).unwrap();
    assert_eq!(grep_rows.len(), 10, "exactly 10 Grep calls should be returned");
    for row in &grep_rows {
        let val: serde_json::Value = serde_json::from_str(row).unwrap();
        assert_eq!(val["tool"].as_str(), Some("Grep"), "only Grep rows expected");
    }

    // ---- default (no flags → last 20) ----
    let default_opts = ctxl::retrieve::CallsOpts::default();
    assert_eq!(default_opts.last, 20);
    let default_rows = ctxl::retrieve::calls(&conn, default_opts).unwrap();
    assert_eq!(default_rows.len(), 20, "default should return last 20");
}

// ---------------------------------------------------------------------------
// AC-1370-04
// ---------------------------------------------------------------------------

// @ac AC-1370-04
#[test]
fn record_uses_begin_immediate() {
    // Verify: two concurrent ctxl record invocations on the same session DB both succeed
    // Verify: no SQLITE_BUSY errors (WAL + BEGIN IMMEDIATE serialization)

    let tmp = tempfile::TempDir::new().unwrap();
    let session_id = "concurrent-test";
    ctxl::db::init_session_at(tmp.path(), session_id).unwrap();

    let db_path =
        std::sync::Arc::new(ctxl::db::session_dir_at(tmp.path(), session_id).join("store.db"));

    let p1 = db_path.clone();
    let t1 = std::thread::spawn(move || {
        let mut conn = rusqlite::Connection::open(&*p1).unwrap();
        let input =
            r#"{"tool_name":"Read","tool_use_id":"t1","tool_response":{"content":"a\nb\nc"}}"#;
        ctxl::record::record(&mut conn, input)
    });

    let p2 = db_path.clone();
    let t2 = std::thread::spawn(move || {
        let mut conn = rusqlite::Connection::open(&*p2).unwrap();
        let input = r#"{"tool_name":"Write","tool_use_id":"t2","tool_response":{}}"#;
        ctxl::record::record(&mut conn, input)
    });

    let r1 = t1.join().expect("thread 1 panicked");
    let r2 = t2.join().expect("thread 2 panicked");
    assert!(r1.is_ok(), "concurrent write 1 failed: {:?}", r1);
    assert!(r2.is_ok(), "concurrent write 2 failed: {:?}", r2);

    // Both rows must be persisted
    let conn = rusqlite::Connection::open(&*db_path).unwrap();
    let count: i64 = conn.query_row("SELECT count(*) FROM calls", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 2, "both concurrent writes should be persisted");
}

// ---------------------------------------------------------------------------
// AC-1370-05
// ---------------------------------------------------------------------------

// @ac AC-1370-05
#[test]
fn recording_failure_exits_silently() {
    // Verify: malformed PostToolUse JSON on stdin (missing required fields) causes exit 0
    // Verify: no stdout output on error
    // Verify: error logged to stderr only

    let tmp = tempfile::TempDir::new().unwrap();
    let session_id = "silent-fail-test";
    ctxl::db::init_session_at(tmp.path(), session_id).unwrap();

    // Missing required `tool_name` field
    let malformed = r#"{"tool_use_id":"abc","tool_response":{}}"#;

    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_ctxl"))
        .args(["intercept", "--session-id", session_id])
        .env("CTXL_CACHE_ROOT", tmp.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn ctxl binary");

    {
        use std::io::Write as _;
        if let Some(stdin) = child.stdin.as_mut() {
            stdin.write_all(malformed.as_bytes()).unwrap();
        }
    }
    let output = child.wait_with_output().expect("failed to wait for ctxl");

    assert_eq!(
        output.status.code(),
        Some(0),
        "malformed JSON should cause exit 0, got {:?}",
        output.status
    );
    assert!(
        output.stdout.is_empty(),
        "no stdout on malformed JSON, got: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
}

// ---------------------------------------------------------------------------
// AC-1370-06
// ---------------------------------------------------------------------------

// @ac AC-1370-06
#[test]
fn db_locked_during_record_exits_silently() {
    // Verify: session DB locked by another process causes exit 0 after busy_timeout expires
    // Verify: no stdout output

    let tmp = tempfile::TempDir::new().unwrap();
    let session_id = "locked-db-test";
    ctxl::db::init_session_at(tmp.path(), session_id).unwrap();
    let db_path = ctxl::db::session_dir_at(tmp.path(), session_id).join("store.db");

    // Acquire an exclusive write lock to simulate a process holding the DB busy.
    // BEGIN IMMEDIATE in WAL mode prevents any other writer from proceeding.
    let lock_conn = rusqlite::Connection::open(&db_path).unwrap();
    lock_conn.execute_batch("BEGIN IMMEDIATE;").unwrap();

    let input = r#"{"tool_name":"Read","tool_response":{}}"#;

    // Use a short busy_timeout so the test completes quickly (50 ms << 5 s).
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_ctxl"))
        .args(["intercept", "--session-id", session_id])
        .env("CTXL_CACHE_ROOT", tmp.path())
        .env("CTXL_BUSY_TIMEOUT_MS", "50")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn ctxl binary");

    {
        use std::io::Write as _;
        if let Some(stdin) = child.stdin.as_mut() {
            stdin.write_all(input.as_bytes()).unwrap();
        }
    }
    let output = child.wait_with_output().expect("failed to wait for ctxl");

    // Release lock after binary exits
    lock_conn.execute_batch("ROLLBACK;").unwrap();

    assert_eq!(output.status.code(), Some(0), "locked DB should exit 0, got {:?}", output.status);
    assert!(
        output.stdout.is_empty(),
        "no stdout when DB is locked, got: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
}
