#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// @ac AC-1111-01

#[test]
fn init_session_provisions_per_session_db() {
    use std::time::SystemTime;

    let tmp = tempfile::tempdir().expect("tempdir");
    let session_id = "00000000-0000-0000-0000-000000000001";
    ctxl::db::init_session_at(tmp.path(), session_id).expect("init_session_at should succeed");

    let dir = ctxl::db::session_dir_at(tmp.path(), session_id);

    // store.db must exist
    let db_path = dir.join("store.db");
    assert!(db_path.exists(), "store.db should exist at {}", db_path.display());

    // WAL mode must be active
    let conn = rusqlite::Connection::open(&db_path).expect("open db");
    let mode: String =
        conn.query_row("PRAGMA journal_mode", [], |row| row.get(0)).expect("PRAGMA journal_mode");
    assert_eq!(mode, "wal", "journal_mode should be wal, got {mode}");

    // .last_used marker must exist and be recent (within 5 s)
    let last_used = dir.join(".last_used");
    assert!(last_used.exists(), ".last_used should exist");
    let mtime = std::fs::metadata(&last_used).expect("metadata").modified().expect("mtime");
    let elapsed = SystemTime::now().duration_since(mtime).expect("duration_since");
    assert!(
        elapsed.as_secs() < 5,
        ".last_used mtime should be within 5s, got {}s",
        elapsed.as_secs()
    );
}

// @ac AC-1111-02
#[test]
fn db_schema_matches_plan_phase_1() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("test.db");
    let conn = rusqlite::Connection::open(&db_path).expect("open db");

    ctxl::db::apply_schema(&conn).expect("apply_schema");

    // --- handles table columns — includes v7 cache columns (blob_id, param_hash, git_head) ---
    let expected_cols = [
        "id",
        "tool",
        "output_mode",
        "params",
        "cwd",
        "summary",
        "content",
        "compressed_body",
        "compressed_method",
        "line_count",
        "total_matches",
        "truncated",
        "token_est",
        "created_at",
        "tool_input",
        "retrieval_count",
        "last_retrieved_at",
        // v7 migration: global cache columns
        "blob_id",
        "param_hash",
        "git_head",
    ];

    let mut stmt = conn.prepare("PRAGMA table_info(handles)").expect("prepare table_info");
    let actual_cols: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .expect("query_map")
        .collect::<Result<_, _>>()
        .expect("collect cols");

    for col in &expected_cols {
        assert!(
            actual_cols.iter().any(|c| c == col),
            "missing column '{col}' in handles; actual: {actual_cols:?}"
        );
    }
    assert_eq!(
        actual_cols.len(),
        expected_cols.len(),
        "unexpected extra columns; actual: {actual_cols:?}"
    );

    // --- content column must be nullable (no NOT NULL constraint, no dflt) ---
    let mut stmt = conn.prepare("PRAGMA table_info(handles)").expect("prepare");
    let nullable_content: bool = stmt
        .query_map([], |row| {
            let name: String = row.get(1)?;
            let notnull: i64 = row.get(3)?;
            Ok((name, notnull))
        })
        .expect("query_map")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect")
        .iter()
        .find(|(name, _)| name == "content")
        .map(|(_, notnull)| *notnull == 0)
        .expect("content column not found");
    assert!(nullable_content, "content column should be nullable");

    // --- content_fts virtual table must exist ---
    let fts_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='content_fts'",
            [],
            |row| row.get(0),
        )
        .expect("query content_fts");
    assert_eq!(fts_count, 1, "content_fts virtual table should exist");

    // --- triggers must exist with WHEN content IS NOT NULL guards ---
    let mut stmt = conn
        .prepare("SELECT sql FROM sqlite_master WHERE type='trigger' AND tbl_name='handles'")
        .expect("prepare triggers");
    let trigger_sqls: Vec<String> = stmt
        .query_map([], |row| row.get(0))
        .expect("query_map")
        .collect::<Result<_, _>>()
        .expect("collect triggers");

    assert!(!trigger_sqls.is_empty(), "expected triggers on handles table");
    let insert_trigger =
        trigger_sqls.iter().any(|sql| sql.contains("WHEN NEW.content IS NOT NULL"));
    let delete_trigger =
        trigger_sqls.iter().any(|sql| sql.contains("WHEN OLD.content IS NOT NULL"));
    assert!(insert_trigger, "insert trigger guarded by WHEN NEW.content IS NOT NULL not found");
    assert!(delete_trigger, "delete trigger guarded by WHEN OLD.content IS NOT NULL not found");

    // --- idx_handles_created index must exist ---
    let idx_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_handles_created'",
            [],
            |row| row.get(0),
        )
        .expect("query index");
    assert_eq!(idx_count, 1, "idx_handles_created index should exist");

    // --- calls table exists (v3 migration applied by apply_schema) ---
    let calls_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='calls'",
            [],
            |row| row.get(0),
        )
        .expect("query calls");
    assert_eq!(calls_count, 1, "calls table must exist after apply_schema (v3 migration)");

    // --- v7 cache columns must be present and nullable ---
    for col in &["blob_id", "param_hash", "git_head"] {
        assert!(actual_cols.iter().any(|c| c == col), "v7 column '{col}' missing from handles");
    }
}

// @ac AC-1111-06
#[test]
fn ctxl_init_without_session_id_rejects_with_clear_error() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ctxl"))
        .arg("init")
        .env_remove("CTXL_SESSION_ID")
        .output()
        .expect("failed to spawn ctxl");

    assert!(!output.status.success(), "ctxl init should fail without --session-id");
    assert_eq!(
        output.status.code(),
        Some(1),
        "exit code should be 1, got {:?}",
        output.status.code()
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("session_id required"),
        "stderr should contain 'session_id required', got: {stderr}"
    );
}
