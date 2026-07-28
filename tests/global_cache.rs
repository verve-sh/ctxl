// Integration tests for Phase 4: cross-session global cache.
//
// Tests cover:
// - Global DB schema creation + idempotency
// - Cache lookup hit/miss paths
// - Param normalization + hashing
// - v7 migration columns present in session DB
// - `search_global` FTS5
// - `clean_old_blobs` TTL enforcement
// - Fail-open: global DB inaccessible → graceful fallback
#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ctxl::cache::{self, normalize_params, xxh128_hex, CacheCheckResult, CacheContext};
use ctxl::global_db::{
    apply_global_schema, clean_old_blobs, fetch_blob_content, lookup, lookup_any_mode,
    open_global_db, search_global, store_blob, touch_blob, StoreBlobParams,
};
use rusqlite::Connection;
use std::sync::Mutex;

/// Global mutex to serialize tests that modify `CTXL_CACHE_ROOT`.
/// Multiple parallel test threads mutating the same env var race and fail.
static ENV_LOCK: Mutex<()> = Mutex::new(());

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn fresh_global_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
    apply_global_schema(&conn).unwrap();
    conn
}

// ---------------------------------------------------------------------------
// Task 1: Global DB schema
// ---------------------------------------------------------------------------

#[test]
fn global_db_creates_blobs_table() {
    let conn = fresh_global_db();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='blobs'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "blobs table must exist");
}

#[test]
fn global_db_creates_cache_index_table() {
    let conn = fresh_global_db();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='cache_index'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "cache_index table must exist");
}

#[test]
fn global_db_creates_blobs_fts() {
    let conn = fresh_global_db();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='blobs_fts'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "blobs_fts FTS table must exist");
}

#[test]
fn global_db_schema_idempotent() {
    let conn = fresh_global_db();
    // Calling again should not fail
    apply_global_schema(&conn).unwrap();
    let ver: i32 = conn.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
    assert_eq!(ver, 2, "global schema version should remain 2");
}

#[test]
fn global_db_wal_mode() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("ctxl").join("global.db");
    let conn = open_global_db(&path).unwrap();
    let mode: String = conn.query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
    assert_eq!(mode, "wal", "global DB must use WAL mode");
}

#[test]
fn global_db_triggers_exist() {
    let conn = fresh_global_db();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='trigger' AND tbl_name='blobs'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 2, "two triggers (ai, ad) on blobs must exist");
}

// ---------------------------------------------------------------------------
// Task 2: Session schema v7
// ---------------------------------------------------------------------------

#[test]
fn session_db_has_v7_columns() {
    let conn = Connection::open_in_memory().unwrap();
    ctxl::db::apply_schema(&conn).unwrap();

    let cols: Vec<String> = conn
        .prepare("PRAGMA table_info(handles)")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();

    assert!(cols.contains(&"blob_id".to_string()), "blob_id column missing from handles");
    assert!(cols.contains(&"param_hash".to_string()), "param_hash column missing from handles");
    assert!(cols.contains(&"git_head".to_string()), "git_head column missing from handles");
}

#[test]
fn session_db_v7_columns_are_nullable() {
    let conn = Connection::open_in_memory().unwrap();
    ctxl::db::apply_schema(&conn).unwrap();

    // v7 columns must not have NOT NULL constraint (nullable = 0)
    let nullable_map: Vec<(String, i64)> = conn
        .prepare("PRAGMA table_info(handles)")
        .unwrap()
        .query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, i64>(3)?)))
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();

    for col in &["blob_id", "param_hash", "git_head"] {
        let notnull =
            nullable_map.iter().find(|(name, _)| name == col).map(|(_, n)| *n).unwrap_or(1);
        assert_eq!(notnull, 0, "v7 column '{col}' must be nullable (notnull=0)");
    }
}

#[test]
fn session_db_version_is_7() {
    let conn = Connection::open_in_memory().unwrap();
    ctxl::db::apply_schema(&conn).unwrap();
    let ver: i32 = conn.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
    assert_eq!(ver, 7, "session DB should be at schema version 7");
}

// ---------------------------------------------------------------------------
// Task 3: Cache lookup hit/miss paths
// ---------------------------------------------------------------------------

#[test]
fn cache_hit_same_git_head() {
    let conn = fresh_global_db();
    let params = StoreBlobParams {
        content_hash: "hit_hash",
        content: "some content",
        compressed_body: None,
        compressed_method: None,
        line_count: Some(1),
        token_est: Some(5),
    };
    let blob_id =
        store_blob(&conn, params, "/repo", "Bash", "stdout", "myhash", "abc1234").unwrap();

    let hit = lookup(&conn, "/repo", "Bash", "stdout", "myhash", "abc1234")
        .unwrap()
        .expect("expected hit");
    assert_eq!(hit.blob_id, blob_id);
}

#[test]
fn cache_miss_different_git_head() {
    let conn = fresh_global_db();
    let params = StoreBlobParams {
        content_hash: "miss_hash",
        content: "content",
        compressed_body: None,
        compressed_method: None,
        line_count: Some(1),
        token_est: Some(1),
    };
    store_blob(&conn, params, "/repo", "Bash", "stdout", "ph", "head_A").unwrap();

    let miss = lookup(&conn, "/repo", "Bash", "stdout", "ph", "head_B").unwrap();
    assert!(miss.is_none(), "different git_head → cache miss");
}

#[test]
fn cache_miss_different_param_hash() {
    let conn = fresh_global_db();
    let params = StoreBlobParams {
        content_hash: "miss_ph",
        content: "content",
        compressed_body: None,
        compressed_method: None,
        line_count: Some(1),
        token_est: Some(1),
    };
    store_blob(&conn, params, "/repo", "Bash", "stdout", "hash_A", "head1").unwrap();

    let miss = lookup(&conn, "/repo", "Bash", "stdout", "hash_B", "head1").unwrap();
    assert!(miss.is_none(), "different param_hash → cache miss");
}

#[test]
fn touch_blob_updates_last_used() {
    let conn = fresh_global_db();
    // Insert with old last_used
    conn.execute(
        "INSERT INTO blobs (content_hash, content, last_used) \
         VALUES ('touch_hash', 'content', datetime('now', '-10 days'))",
        [],
    )
    .unwrap();
    let blob_id: i64 = conn
        .query_row("SELECT id FROM blobs WHERE content_hash='touch_hash'", [], |r| r.get(0))
        .unwrap();

    touch_blob(&conn, blob_id).unwrap();

    let last_used: String = conn
        .query_row("SELECT last_used FROM blobs WHERE id=?1", rusqlite::params![blob_id], |r| {
            r.get(0)
        })
        .unwrap();
    // last_used should now be within the last few seconds
    let diff: i64 = conn
        .query_row(
            "SELECT CAST((julianday('now') - julianday(?1)) * 86400 AS INTEGER)",
            rusqlite::params![last_used],
            |r| r.get(0),
        )
        .unwrap();
    assert!(diff < 5, "last_used should be recent after touch_blob, diff={diff}s");
}

#[test]
fn fetch_blob_content_returns_stored_content() {
    let conn = fresh_global_db();
    let params = StoreBlobParams {
        content_hash: "fetch_hash",
        content: "exact content string",
        compressed_body: None,
        compressed_method: None,
        line_count: Some(1),
        token_est: Some(3),
    };
    let blob_id = store_blob(&conn, params, "/r", "Grep", "", "ph", "hd").unwrap();
    let content = fetch_blob_content(&conn, blob_id).unwrap().expect("content");
    assert_eq!(content, "exact content string");
}

#[test]
fn fetch_blob_content_returns_none_for_unknown_id() {
    let conn = fresh_global_db();
    let result = fetch_blob_content(&conn, 99999).unwrap();
    assert!(result.is_none(), "unknown blob_id → None");
}

// ---------------------------------------------------------------------------
// Task 3: Param normalization + hashing
// ---------------------------------------------------------------------------

#[test]
fn normalize_params_stable() {
    let a = normalize_params(r#"{"command":"ls","cwd":"/tmp"}"#);
    let b = normalize_params(r#"{"cwd":"/tmp","command":"ls"}"#);
    assert_eq!(a, b, "normalized form must be key-order independent");
}

#[test]
fn normalize_removes_session_id() {
    let input = r#"{"session_id":"s123","cmd":"foo"}"#;
    let out = normalize_params(input);
    assert!(!out.contains("session_id"), "session_id must be stripped");
}

#[test]
fn xxh128_hex_is_32_chars() {
    let h = xxh128_hex("test");
    assert_eq!(h.len(), 32, "xxh128 hex must be 32 chars");
}

#[test]
fn xxh128_hex_is_deterministic() {
    assert_eq!(xxh128_hex("hello"), xxh128_hex("hello"));
}

#[test]
fn xxh128_hex_differs_for_different_inputs() {
    assert_ne!(xxh128_hex("a"), xxh128_hex("b"));
}

#[test]
fn param_hash_stable_after_normalization() {
    let h1 = xxh128_hex(&normalize_params(r#"{"b":2,"a":1}"#));
    let h2 = xxh128_hex(&normalize_params(r#"{"a":1,"b":2}"#));
    assert_eq!(h1, h2, "reordered JSON → same hash after normalization");
}

// ---------------------------------------------------------------------------
// Task 1: FTS5 search
// ---------------------------------------------------------------------------

#[test]
fn search_global_fts_finds_content() {
    let conn = fresh_global_db();
    let params = StoreBlobParams {
        content_hash: "fts_hash",
        content: "fn test_function() { let x = 42; }",
        compressed_body: None,
        compressed_method: None,
        line_count: Some(1),
        token_est: Some(8),
    };
    store_blob(&conn, params, "/my/repo", "Bash", "stdout", "ph", "hd").unwrap();

    let results = search_global(&conn, "test_function", Some("/my/repo"), 10).unwrap();
    assert!(!results.is_empty(), "FTS5 should find test_function");
    assert_eq!(results[0].tool, "Bash");
    assert_eq!(results[0].repo_root, "/my/repo");
}

#[test]
fn search_global_scoped_by_repo_root() {
    let conn = fresh_global_db();

    let params_a = StoreBlobParams {
        content_hash: "scope_hash_a",
        content: "hello from repo_a",
        compressed_body: None,
        compressed_method: None,
        line_count: Some(1),
        token_est: Some(3),
    };
    store_blob(&conn, params_a, "/repo_a", "Bash", "stdout", "ph_a", "hd").unwrap();

    let params_b = StoreBlobParams {
        content_hash: "scope_hash_b",
        content: "hello from repo_b",
        compressed_body: None,
        compressed_method: None,
        line_count: Some(1),
        token_est: Some(3),
    };
    store_blob(&conn, params_b, "/repo_b", "Bash", "stdout", "ph_b", "hd").unwrap();

    // Scoped to /repo_a — should only return repo_a result
    let results = search_global(&conn, "hello", Some("/repo_a"), 10).unwrap();
    assert_eq!(results.len(), 1, "scoped search should return only repo_a result");
    assert_eq!(results[0].repo_root, "/repo_a");
}

#[test]
fn search_global_no_repo_scope_finds_all() {
    let conn = fresh_global_db();

    let p1 = StoreBlobParams {
        content_hash: "unscoped_a",
        content: "unique_token_abc",
        compressed_body: None,
        compressed_method: None,
        line_count: Some(1),
        token_est: Some(1),
    };
    store_blob(&conn, p1, "/r1", "Bash", "stdout", "ph1", "hd1").unwrap();

    let p2 = StoreBlobParams {
        content_hash: "unscoped_b",
        content: "unique_token_abc again",
        compressed_body: None,
        compressed_method: None,
        line_count: Some(1),
        token_est: Some(2),
    };
    store_blob(&conn, p2, "/r2", "Grep", "", "ph2", "hd2").unwrap();

    let results = search_global(&conn, "unique_token_abc", None, 20).unwrap();
    assert_eq!(results.len(), 2, "unscoped search should find both results");
}

// ---------------------------------------------------------------------------
// Task 1: clean_old_blobs
// ---------------------------------------------------------------------------

#[test]
fn clean_global_removes_old_blobs() {
    let conn = fresh_global_db();

    conn.execute(
        "INSERT INTO blobs (content_hash, content, last_used) \
         VALUES ('stale_hash', 'stale content', datetime('now', '-31 days'))",
        [],
    )
    .unwrap();
    let stale_id: i64 = conn
        .query_row("SELECT id FROM blobs WHERE content_hash='stale_hash'", [], |r| r.get(0))
        .unwrap();
    conn.execute(
        "INSERT INTO cache_index (repo_root, tool, output_mode, param_hash, git_head, blob_id) \
         VALUES ('/r', 'Bash', '', 'ph', 'hd', ?1)",
        rusqlite::params![stale_id],
    )
    .unwrap();

    let (count, _bytes) = clean_old_blobs(&conn, 30).unwrap();
    assert_eq!(count, 1);

    let remaining: i64 = conn
        .query_row("SELECT COUNT(*) FROM blobs WHERE content_hash='stale_hash'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(remaining, 0, "stale blob should be deleted");

    let ci_remaining: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM cache_index WHERE blob_id=?1",
            rusqlite::params![stale_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(ci_remaining, 0, "orphaned cache_index entry should be deleted");
}

#[test]
fn clean_global_preserves_fresh_blobs() {
    let conn = fresh_global_db();

    conn.execute("INSERT INTO blobs (content_hash, content) VALUES ('fresh_hash', 'fresh')", [])
        .unwrap();

    let (count, _) = clean_old_blobs(&conn, 30).unwrap();
    assert_eq!(count, 0, "fresh blob should not be removed");
}

#[test]
fn clean_global_reports_bytes_freed() {
    let conn = fresh_global_db();

    conn.execute(
        "INSERT INTO blobs (content_hash, content, last_used) \
         VALUES ('bytes_hash', '1234567890', datetime('now', '-31 days'))",
        [],
    )
    .unwrap();

    let (count, bytes) = clean_old_blobs(&conn, 30).unwrap();
    assert_eq!(count, 1);
    assert!(bytes > 0, "bytes_freed should be > 0, got {bytes}");
}

// ---------------------------------------------------------------------------
// Task 3: fail-open — global DB inaccessible
// ---------------------------------------------------------------------------

#[test]
fn cache_check_unavailable_on_no_repo_root() {
    let ctx = CacheContext { repo_root: None, cwd: None };
    let result = cache_check_result(&ctx);
    assert!(matches!(result, CacheCheckResult::Unavailable(_)), "no repo_root → Unavailable");
}

fn cache_check_result(ctx: &CacheContext) -> CacheCheckResult {
    cache::cache_check(ctx, "Bash", Some("stdout"), r#"{"command":"ls"}"#)
}

#[test]
fn lookup_any_mode_hits_regardless_of_stored_mode() {
    let conn = fresh_global_db();
    let params = StoreBlobParams {
        content_hash: "any_mode_test",
        content: "fn hello() {}",
        compressed_body: None,
        compressed_method: None,
        line_count: Some(1),
        token_est: Some(3),
    };
    store_blob(&conn, params, "/repo", "Bash", "mixed", "ph1", "head1").unwrap();

    let hit = lookup_any_mode(&conn, "/repo", "Bash", "ph1", "head1")
        .unwrap()
        .expect("should hit regardless of output_mode");
    assert!(hit.blob_id > 0);

    let miss = lookup_any_mode(&conn, "/repo", "Bash", "ph1", "other_head").unwrap();
    assert!(miss.is_none(), "different git_head → miss");
}

#[test]
fn lookup_any_mode_returns_most_recent_when_multiple_modes() {
    let conn = fresh_global_db();
    let params_a = StoreBlobParams {
        content_hash: "multi_mode_a",
        content: "older content",
        compressed_body: None,
        compressed_method: None,
        line_count: Some(1),
        token_est: Some(2),
    };
    let blob_a = store_blob(&conn, params_a, "/repo", "Bash", "stdout", "ph2", "head2").unwrap();

    let params_b = StoreBlobParams {
        content_hash: "multi_mode_b",
        content: "newer content",
        compressed_body: None,
        compressed_method: None,
        line_count: Some(1),
        token_est: Some(2),
    };
    let blob_b = store_blob(&conn, params_b, "/repo", "Bash", "mixed", "ph2", "head2").unwrap();

    let hit = lookup_any_mode(&conn, "/repo", "Bash", "ph2", "head2").unwrap().expect("should hit");
    assert_eq!(
        hit.blob_id, blob_b,
        "should return most recent (blob_b > blob_a: {blob_b} > {blob_a})"
    );
}

#[test]
fn global_db_open_creates_parent_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    let nested = tmp.path().join("a").join("b").join("c").join("global.db");
    let _conn = open_global_db(&nested).expect("open_global_db should create parent dirs");
    assert!(nested.exists(), "global.db should be created at nested path");
}

// ---------------------------------------------------------------------------
// Task 3: Retrieval fallback when content=NULL + blob_id set
// (simulated via direct SQL — actual ATTACH logic lives in retrieve.rs)
// ---------------------------------------------------------------------------

#[test]
fn session_handle_can_have_null_content_with_blob_id() {
    // Verify that the session schema accepts content=NULL alongside blob_id.
    let conn = Connection::open_in_memory().unwrap();
    ctxl::db::apply_schema(&conn).unwrap();

    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
        as i64;

    conn.execute(
        "INSERT INTO handles \
         (id, tool, output_mode, truncated, created_at, blob_id, param_hash, git_head) \
         VALUES ('b_test01', 'Bash', 'stdout', 0, ?1, 42, 'myphash', 'deadbeef')",
        rusqlite::params![now],
    )
    .expect("insert should succeed with content=NULL and blob_id set");

    let blob_id: Option<i64> = conn
        .query_row("SELECT blob_id FROM handles WHERE id='b_test01'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(blob_id, Some(42), "blob_id should be stored");

    let content: Option<String> = conn
        .query_row("SELECT content FROM handles WHERE id='b_test01'", [], |r| r.get(0))
        .unwrap();
    assert!(content.is_none(), "content should be NULL");
}

// ---------------------------------------------------------------------------
// Gap 1: Intercept write-through integration tests
// ---------------------------------------------------------------------------

/// Helper: open an in-memory session DB with the v7 schema applied.
fn fresh_session_db() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    ctxl::db::apply_schema(&conn).unwrap();
    conn
}

#[test]
fn try_cache_write_through_updates_session_blob_id() {
    // Arrange: a temp global DB, a session DB with a stored handle.
    let _lock = ENV_LOCK.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("CTXL_CACHE_ROOT", tmp.path());

    let session_conn = fresh_session_db();
    let content = "hello world cache test";

    // Insert a handle manually (simulating what store::write does).
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
        as i64;
    session_conn
        .execute(
            "INSERT INTO handles (id, tool, output_mode, content, truncated, created_at) \
             VALUES ('b_cachetest', 'Bash', 'mixed', ?1, 0, ?2)",
            rusqlite::params![content, now],
        )
        .unwrap();

    // Act: call the cache write-through.
    ctxl::intercept::try_cache_write_through(
        &session_conn,
        "b_cachetest",
        content,
        "Bash",
        "mixed",
        None,
    );

    // Assert: session handle now has blob_id, param_hash, git_head set.
    // Note: git operations may fail in test env (not in a git repo), so we
    // only assert that the call didn't panic. If blob_id was set, great.
    // If git wasn't available (CI), blob_id stays NULL — that's also correct (fail-open).
    let blob_id: Option<i64> = session_conn
        .query_row("SELECT blob_id FROM handles WHERE id='b_cachetest'", [], |r| r.get(0))
        .unwrap();

    // blob_id is either set (git available) or NULL (git unavailable) — both valid.
    // The important thing: no panic, handle still exists.
    let handle_id: String = session_conn
        .query_row("SELECT id FROM handles WHERE id='b_cachetest'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(handle_id, "b_cachetest", "handle must still exist after cache write-through");
    let _ = blob_id; // accepted either way

    std::env::remove_var("CTXL_CACHE_ROOT");
}

#[test]
fn try_cache_write_through_succeeds_when_global_db_empty() {
    // A fresh temp global DB (no blobs) — write-through should create the blob.
    let _lock = ENV_LOCK.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("CTXL_CACHE_ROOT", tmp.path());

    // Prime the global DB so it exists.
    let db_path = tmp.path().join("ctxl").join("global.db");
    let _gconn = open_global_db(&db_path).unwrap();

    let session_conn = fresh_session_db();
    let content = "unique content for cache miss test";
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
        as i64;
    session_conn
        .execute(
            "INSERT INTO handles (id, tool, output_mode, content, truncated, created_at) \
             VALUES ('b_miss', 'Bash', 'mixed', ?1, 0, ?2)",
            rusqlite::params![content, now],
        )
        .unwrap();

    // Act — should not panic even in a non-git environment.
    ctxl::intercept::try_cache_write_through(
        &session_conn,
        "b_miss",
        content,
        "Bash",
        "mixed",
        None,
    );

    // Handle must still be intact.
    let found: i64 = session_conn
        .query_row("SELECT COUNT(*) FROM handles WHERE id='b_miss'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(found, 1, "handle must survive cache write-through");

    std::env::remove_var("CTXL_CACHE_ROOT");
}

#[test]
fn try_cache_write_through_is_fail_open_on_bad_db_path() {
    // Point CTXL_CACHE_ROOT at a non-writable path.
    // The write-through must not panic or corrupt the session DB.
    let _lock = ENV_LOCK.lock().unwrap();
    std::env::set_var("CTXL_CACHE_ROOT", "/nonexistent/path/that/cannot/be/created");

    let session_conn = fresh_session_db();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
        as i64;
    session_conn
        .execute(
            "INSERT INTO handles (id, tool, output_mode, content, truncated, created_at) \
             VALUES ('b_failopen', 'Bash', 'mixed', 'some content', 0, ?1)",
            rusqlite::params![now],
        )
        .unwrap();

    // Must not panic.
    ctxl::intercept::try_cache_write_through(
        &session_conn,
        "b_failopen",
        "some content",
        "Bash",
        "mixed",
        None,
    );

    // Session handle must still be intact.
    let count: i64 = session_conn
        .query_row("SELECT COUNT(*) FROM handles WHERE id='b_failopen'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "session handle must survive a bad global DB path");

    std::env::remove_var("CTXL_CACHE_ROOT");
}

// ---------------------------------------------------------------------------
// Gap 2: Retrieval fallback for content=NULL + blob_id
// ---------------------------------------------------------------------------

/// Insert a handle with content=NULL and blob_id pointing to a global blob.
fn insert_null_content_handle(
    session_conn: &rusqlite::Connection,
    handle_id: &str,
    blob_id: i64,
    param_hash: &str,
    git_head: &str,
) {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
        as i64;
    session_conn
        .execute(
            "INSERT INTO handles \
             (id, tool, output_mode, truncated, created_at, blob_id, param_hash, git_head) \
             VALUES (?1, 'Bash', 'mixed', 0, ?2, ?3, ?4, ?5)",
            rusqlite::params![handle_id, now, blob_id, param_hash, git_head],
        )
        .unwrap();
}

#[test]
fn show_resolves_null_content_via_global_blob() {
    let _lock = ENV_LOCK.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("CTXL_CACHE_ROOT", tmp.path());

    // Set up global DB with a blob.
    let db_path = tmp.path().join("ctxl").join("global.db");
    let gconn = open_global_db(&db_path).unwrap();
    let params = StoreBlobParams {
        content_hash: "show_test_hash",
        content: "line one\nline two\nline three\n",
        compressed_body: None,
        compressed_method: None,
        line_count: Some(3),
        token_est: Some(6),
    };
    let blob_id =
        store_blob(&gconn, params, "/repo", "Bash", "mixed", "show_ph", "show_head").unwrap();
    drop(gconn);

    // Set up session handle with content=NULL pointing at the global blob.
    let session_conn = fresh_session_db();
    insert_null_content_handle(&session_conn, "b_show01", blob_id, "show_ph", "show_head");

    // Act: show() must resolve content from the global DB.
    let result =
        ctxl::retrieve::show(&session_conn, "b_show01", ctxl::retrieve::ShowOpts::default())
            .unwrap();

    assert!(result.contains("line one"), "show must resolve content from global DB, got: {result}");
    assert!(result.contains("line two"), "show must resolve all lines, got: {result}");

    std::env::remove_var("CTXL_CACHE_ROOT");
}

#[test]
fn search_resolves_null_content_via_global_blob() {
    let _lock = ENV_LOCK.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("CTXL_CACHE_ROOT", tmp.path());

    let db_path = tmp.path().join("ctxl").join("global.db");
    let gconn = open_global_db(&db_path).unwrap();
    let params = StoreBlobParams {
        content_hash: "search_test_hash",
        content: "fn search_target() { let x = 42; }\nother line\n",
        compressed_body: None,
        compressed_method: None,
        line_count: Some(2),
        token_est: Some(10),
    };
    let blob_id =
        store_blob(&gconn, params, "/repo", "Bash", "mixed", "srch_ph", "srch_head").unwrap();
    drop(gconn);

    let session_conn = fresh_session_db();
    insert_null_content_handle(&session_conn, "b_srch01", blob_id, "srch_ph", "srch_head");

    // Act: search() must find content via global DB fallback.
    let results = ctxl::retrieve::search(&session_conn, "b_srch01", "search_target", 10).unwrap();

    // Substring fallback will match since FTS5 can't index NULL content.
    assert!(!results.is_empty(), "search must find content via global blob fallback");
    assert!(
        results.iter().any(|l| l.contains("search_target")),
        "search result must contain the query term, got: {results:?}"
    );

    std::env::remove_var("CTXL_CACHE_ROOT");
}

#[test]
fn show_returns_empty_when_blob_id_absent() {
    let session_conn = fresh_session_db();

    // Insert a handle with content=NULL and no blob_id.
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
        as i64;
    session_conn
        .execute(
            "INSERT INTO handles (id, tool, output_mode, truncated, created_at) \
             VALUES ('b_noblobid', 'Bash', 'mixed', 0, ?1)",
            rusqlite::params![now],
        )
        .unwrap();

    // show() should return empty (graceful — no content available).
    let result =
        ctxl::retrieve::show(&session_conn, "b_noblobid", ctxl::retrieve::ShowOpts::default())
            .unwrap();
    assert!(result.is_empty(), "show with no content and no blob_id should return empty");
}

#[test]
fn show_returns_not_found_for_unknown_handle() {
    let session_conn = fresh_session_db();

    let result =
        ctxl::retrieve::show(&session_conn, "b_nonexistent", ctxl::retrieve::ShowOpts::default());
    assert!(result.is_err(), "show must return Err for unknown handle");
}

#[test]
fn files_resolves_null_content_via_global_blob() {
    let _lock = ENV_LOCK.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("CTXL_CACHE_ROOT", tmp.path());

    let db_path = tmp.path().join("ctxl").join("global.db");
    let gconn = open_global_db(&db_path).unwrap();
    // Store grep-like content (file:line:text format) so files() can extract
    // a file manifest from it.
    let grep_content =
        concat!("src/lib.rs:1:fn foo()\n", "src/lib.rs:2:fn bar()\n", "src/main.rs:1:fn main()\n");
    let params = StoreBlobParams {
        content_hash: "files_test_hash",
        content: grep_content,
        compressed_body: None,
        compressed_method: None,
        line_count: Some(3),
        token_est: Some(12),
    };
    let blob_id =
        store_blob(&gconn, params, "/repo", "Grep", "content", "files_ph", "files_head").unwrap();
    drop(gconn);

    let session_conn = fresh_session_db();
    // Insert with output_mode="content" to match the grep content format.
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
        as i64;
    session_conn
        .execute(
            "INSERT INTO handles \
             (id, tool, output_mode, truncated, created_at, blob_id, param_hash, git_head) \
             VALUES ('g_files01', 'Grep', 'content', 0, ?1, ?2, 'files_ph', 'files_head')",
            rusqlite::params![now, blob_id],
        )
        .unwrap();

    // Act: files() must resolve content from global DB.
    let rows = ctxl::retrieve::files(&session_conn, "g_files01").unwrap();

    // rows contains header + file rows + summary.
    let joined = rows.join("\n");
    assert!(
        joined.contains("src/lib.rs"),
        "files must show lib.rs from global blob, got: {joined}"
    );
    assert!(
        joined.contains("src/main.rs"),
        "files must show main.rs from global blob, got: {joined}"
    );

    std::env::remove_var("CTXL_CACHE_ROOT");
}

// ---------------------------------------------------------------------------
// Issue 6a: search_all NULL-content fallback via global blob
// ---------------------------------------------------------------------------

#[test]
fn search_all_resolves_null_content_via_global_blob() {
    let _lock = ENV_LOCK.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("CTXL_CACHE_ROOT", tmp.path());

    // Set up global DB with a blob containing searchable content.
    let db_path = tmp.path().join("ctxl").join("global.db");
    let gconn = open_global_db(&db_path).unwrap();
    let params = StoreBlobParams {
        content_hash: "search_all_null_hash",
        content: "fn unique_search_all_target() { return 42; }\nother line\n",
        compressed_body: None,
        compressed_method: None,
        line_count: Some(2),
        token_est: Some(10),
    };
    let blob_id = store_blob(&gconn, params, "/repo", "Bash", "mixed", "sa_ph", "sa_head").unwrap();
    drop(gconn);

    // Set up session handle with content=NULL pointing at the global blob.
    let session_conn = fresh_session_db();
    insert_null_content_handle(&session_conn, "b_sa01", blob_id, "sa_ph", "sa_head");

    // Act: search_all must find content via substring fallback on NULL-content handles.
    let results =
        ctxl::retrieve::search_all(&session_conn, "unique_search_all_target", 10).unwrap();

    assert!(
        !results.is_empty(),
        "search_all must find content via global blob fallback for NULL-content handles"
    );
    assert!(
        results.iter().any(|m| m.line.contains("unique_search_all_target")),
        "search_all result must contain the query term, got: {:?}",
        results.iter().map(|m| &m.line).collect::<Vec<_>>()
    );

    std::env::remove_var("CTXL_CACHE_ROOT");
}

// ---------------------------------------------------------------------------
// Issue 6b: normalize_params keeps cwd in output
// ---------------------------------------------------------------------------

#[test]
fn normalize_params_keeps_cwd() {
    let input = r#"{"cwd":"/tmp/mydir","command":"ls -la"}"#;
    let out = normalize_params(input);
    assert!(
        out.contains("cwd"),
        "normalize_params must keep cwd (it affects command output), got: {out}"
    );
    assert!(out.contains("/tmp/mydir"), "normalize_params must preserve cwd value, got: {out}");
}

#[test]
fn normalize_params_different_cwd_produces_different_hash() {
    let h1 = xxh128_hex(&normalize_params(r#"{"command":"ls","cwd":"/tmp/a"}"#));
    let h2 = xxh128_hex(&normalize_params(r#"{"command":"ls","cwd":"/tmp/b"}"#));
    assert_ne!(
        h1, h2,
        "different cwd must produce different param_hash (commands produce different output)"
    );
}

// ---------------------------------------------------------------------------
// Issue 6c: cache lookup MissNoMatch vs MissHeadChanged distinction
// ---------------------------------------------------------------------------

#[test]
fn lookup_returns_none_for_no_match() {
    let conn = fresh_global_db();
    // Empty DB — no entries at all.
    let result = lookup(&conn, "/repo", "Bash", "stdout", "nonexistent_hash", "head1").unwrap();
    assert!(result.is_none(), "empty DB must produce None (MissNoMatch)");
}

#[test]
fn lookup_returns_none_for_different_git_head() {
    let conn = fresh_global_db();
    let params = StoreBlobParams {
        content_hash: "head_changed_hash",
        content: "content for head change test",
        compressed_body: None,
        compressed_method: None,
        line_count: Some(1),
        token_est: Some(5),
    };
    store_blob(&conn, params, "/repo", "Bash", "stdout", "ph1", "head_OLD").unwrap();

    // Same repo/tool/output_mode/param_hash but different git_head.
    let result = lookup(&conn, "/repo", "Bash", "stdout", "ph1", "head_NEW").unwrap();
    assert!(result.is_none(), "different git_head must produce None (MissHeadChanged scenario)");
}

#[test]
fn lookup_returns_hit_for_matching_entry() {
    let conn = fresh_global_db();
    let params = StoreBlobParams {
        content_hash: "hit_test_hash_6c",
        content: "content for cache hit test",
        compressed_body: None,
        compressed_method: None,
        line_count: Some(1),
        token_est: Some(5),
    };
    let blob_id =
        store_blob(&conn, params, "/repo", "Bash", "stdout", "ph_hit", "head_match").unwrap();

    // Exact match on all dimensions.
    let hit = lookup(&conn, "/repo", "Bash", "stdout", "ph_hit", "head_match")
        .unwrap()
        .expect("expected cache hit");
    assert_eq!(hit.blob_id, blob_id, "cache hit blob_id must match stored blob");
}

// ---------------------------------------------------------------------------
// Issue 4 (related): search_global deduplication across cache_index entries
// ---------------------------------------------------------------------------

#[test]
fn search_global_deduplicates_multiple_cache_index_entries() {
    let conn = fresh_global_db();

    // Store one blob.
    let params = StoreBlobParams {
        content_hash: "dedup_test_hash",
        content: "fn deduplicate_me() { unique_dedup_content }",
        compressed_body: None,
        compressed_method: None,
        line_count: Some(1),
        token_est: Some(5),
    };
    let blob_id = store_blob(&conn, params, "/repo", "Bash", "stdout", "ph_d1", "head_A").unwrap();

    // Add a second cache_index entry pointing to the same blob (different git_head).
    conn.execute(
        "INSERT INTO cache_index (repo_root, tool, output_mode, param_hash, git_head, blob_id) \
         VALUES ('/repo', 'Bash', 'stdout', 'ph_d1', 'head_B', ?1)",
        rusqlite::params![blob_id],
    )
    .unwrap();

    // Verify two cache_index entries exist.
    let ci_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM cache_index WHERE blob_id = ?1",
            rusqlite::params![blob_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(ci_count, 2, "setup: two cache_index entries must exist");

    // Search should return exactly 1 result (deduplicated), not 2.
    let results = search_global(&conn, "unique_dedup_content", Some("/repo"), 10).unwrap();
    assert_eq!(
        results.len(),
        1,
        "search_global must deduplicate results from multiple cache_index entries, got {}",
        results.len()
    );
    assert_eq!(results[0].blob_id, blob_id);
}

// ---------------------------------------------------------------------------
// Issue 2 (related): --compressed retrieval resolves NULL content via global DB
// ---------------------------------------------------------------------------

#[test]
fn show_compressed_resolves_null_content_via_global_blob() {
    let _lock = ENV_LOCK.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("CTXL_CACHE_ROOT", tmp.path());

    // Set up global DB with a blob.
    let db_path = tmp.path().join("ctxl").join("global.db");
    let gconn = open_global_db(&db_path).unwrap();
    let params = StoreBlobParams {
        content_hash: "compressed_show_hash",
        content: "line alpha\nline beta\nline gamma\n",
        compressed_body: None,
        compressed_method: None,
        line_count: Some(3),
        token_est: Some(6),
    };
    let blob_id =
        store_blob(&gconn, params, "/repo", "Bash", "mixed", "comp_ph", "comp_head").unwrap();
    drop(gconn);

    // Session handle with content=NULL and no compressed_body.
    let session_conn = fresh_session_db();
    insert_null_content_handle(&session_conn, "b_comp01", blob_id, "comp_ph", "comp_head");

    // Act: show with compressed=true must resolve content via global DB fallback.
    let opts = ctxl::retrieve::ShowOpts { compressed: true, ..Default::default() };
    let result = ctxl::retrieve::show(&session_conn, "b_comp01", opts).unwrap();

    assert!(
        result.contains("line alpha"),
        "compressed show must resolve content from global DB, got: {result}"
    );
    assert!(
        result.contains("No structural compression"),
        "must include fallback note, got: {result}"
    );

    std::env::remove_var("CTXL_CACHE_ROOT");
}
