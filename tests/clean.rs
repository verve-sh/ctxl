#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used, clippy::panic)] // integration tests

use ctxl::clean;
use std::fs::{File, FileTimes};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static ENV_LOCK: Mutex<()> = Mutex::new(());

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Create `cache_root/ctxl/<name>/` with a `.last_used` marker file whose
/// mtime is set to `days_ago` days in the past.
fn create_session(cache_root: &std::path::Path, name: &str, days_ago: u64) {
    let dir = cache_root.join("ctxl").join(name);
    std::fs::create_dir_all(&dir).expect("create session dir");

    let last_used = dir.join(".last_used");

    // Create the file, then set its mtime via std::fs::FileTimes
    // (stable since Rust 1.75; this crate uses 1.95).
    let f = File::create(&last_used).expect("create .last_used");

    let target_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .saturating_sub(days_ago * 86_400);
    let mtime = UNIX_EPOCH + Duration::from_secs(target_secs);

    let times = FileTimes::new().set_modified(mtime);
    f.set_times(times).expect("set_times on .last_used");
}

// ---------------------------------------------------------------------------
// AC-1112-05
// ---------------------------------------------------------------------------

// @ac AC-1112-05
#[test]
fn ctxl_clean_removes_stale_session_dirs_by_last_used_mtime() {
    // Verify: Given two session dirs, one with `.last_used` mtime 8 days old
    //         and one with mtime today, running clean with TTL=7d removes the
    //         stale dir and leaves the fresh dir intact.

    let tmp = tempfile::tempdir().expect("tempdir");
    let cache_root = tmp.path();

    create_session(cache_root, "stale-session", 8); // 8 days > 7d TTL → remove
    create_session(cache_root, "fresh-session", 0); // 0 days ≤ 7d TTL → keep

    let stale_dir = cache_root.join("ctxl").join("stale-session");
    let fresh_dir = cache_root.join("ctxl").join("fresh-session");
    assert!(stale_dir.exists(), "stale dir should exist before sweep");
    assert!(fresh_dir.exists(), "fresh dir should exist before sweep");

    clean::sweep(cache_root, Duration::from_secs(7 * 86_400)).expect("sweep");

    assert!(
        !stale_dir.exists(),
        "stale-session (8 days old) should be removed after sweep with TTL=7d"
    );
    assert!(fresh_dir.exists(), "fresh-session (today) should remain after sweep with TTL=7d");
}

// ---------------------------------------------------------------------------
// parse_ttl tests (AC-1112-05 support)
// ---------------------------------------------------------------------------

#[test]
fn parse_ttl_parses_days_and_hours() {
    assert_eq!(clean::parse_ttl("7d").unwrap(), Duration::from_secs(7 * 86_400));
    assert_eq!(clean::parse_ttl("1d").unwrap(), Duration::from_secs(86_400));
    assert_eq!(clean::parse_ttl("24h").unwrap(), Duration::from_secs(24 * 3_600));
    assert!(clean::parse_ttl("invalid").is_err());
    assert!(clean::parse_ttl("xd").is_err());
}

#[test]
fn ctxl_clean_skips_dirs_without_last_used_marker() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cache_root = tmp.path();

    // A dir with no .last_used file should be left intact
    let no_marker = cache_root.join("ctxl").join("no-marker");
    std::fs::create_dir_all(&no_marker).expect("create dir");

    clean::sweep(cache_root, Duration::from_secs(86_400)).expect("sweep");

    assert!(no_marker.exists(), "dir without .last_used should not be removed");
}

#[test]
fn sweep_continues_past_unreadable_entry() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let ctxl_dir = tmp.path().join("ctxl");
    std::fs::create_dir_all(&ctxl_dir).unwrap();

    // Create a valid expired session that should be cleaned up.
    let old_session = ctxl_dir.join("old-session");
    std::fs::create_dir(&old_session).unwrap();
    let marker = old_session.join(".last_used");
    let f = File::create(&marker).unwrap();
    let ten_days_ago = UNIX_EPOCH
        + Duration::from_secs(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                .saturating_sub(10 * 86_400),
        );
    f.set_times(FileTimes::new().set_modified(ten_days_ago)).unwrap();

    // Create a session dir with a .last_used that is a directory (unreadable as file metadata).
    let bad_session = ctxl_dir.join("bad-session");
    std::fs::create_dir(&bad_session).unwrap();
    let bad_marker = bad_session.join(".last_used");
    std::fs::create_dir(&bad_marker).unwrap(); // directory, not file

    // Sweep should not error — bad-session is skipped, old-session is removed.
    clean::sweep(tmp.path(), Duration::from_secs(86_400)).expect("sweep");
    assert!(!old_session.exists(), "expired session should be removed");
    assert!(bad_session.exists(), "unreadable session should be skipped, not removed");
}

#[test]
fn ctxl_clean_is_noop_when_ctxl_dir_missing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // No ctxl/ sub-directory created — sweep should be a no-op
    clean::sweep(tmp.path(), Duration::from_secs(86_400)).expect("sweep on missing ctxl dir");
}

// ---------------------------------------------------------------------------
// Stats aggregation during sweep
// ---------------------------------------------------------------------------

#[test]
fn sweep_aggregates_before_deletion() {
    let _lock = ENV_LOCK.lock().unwrap();
    let tmp = tempfile::tempdir().expect("tempdir");
    let cache_root = tmp.path();
    std::env::set_var("CTXL_CACHE_ROOT", cache_root);

    // Create a stale session with a real store.db
    let session_dir = cache_root.join("ctxl").join("test-agg-session");
    std::fs::create_dir_all(&session_dir).unwrap();

    // Create .last_used with old mtime
    let last_used = session_dir.join(".last_used");
    let f = File::create(&last_used).unwrap();
    let ten_days_ago = UNIX_EPOCH
        + Duration::from_secs(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                .saturating_sub(10 * 86_400),
        );
    f.set_times(FileTimes::new().set_modified(ten_days_ago)).unwrap();

    // Create a session DB with some handles
    let store_db = session_dir.join("store.db");
    let conn = rusqlite::Connection::open(&store_db).unwrap();
    ctxl::db::apply_schema(&conn).unwrap();
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
    conn.execute(
        "INSERT INTO handles (id, tool, output_mode, content, token_est, truncated, created_at) \
         VALUES ('b_test01', 'Bash', 'stdout', 'hello world test content', 50, 0, ?1)",
        rusqlite::params![now],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO handles (id, tool, output_mode, content, token_est, truncated, created_at) \
         VALUES ('b_test02', 'Bash', 'stdout', 'second handle', 30, 0, ?1)",
        rusqlite::params![now],
    )
    .unwrap();
    drop(conn);

    // Set up global DB
    let global_path = cache_root.join("ctxl").join("global.db");
    let gconn = ctxl::global_db::open_global_db(&global_path).unwrap();
    drop(gconn);

    // Sweep with 7d TTL — session is 10 days old
    clean::sweep(cache_root, Duration::from_secs(7 * 86_400)).unwrap();

    // Session dir should be removed
    assert!(!session_dir.exists(), "stale session should be removed after sweep");

    // Global DB should have session_summaries row
    let gconn = ctxl::global_db::open_global_db(&global_path).unwrap();
    let stats = ctxl::global_db::query_cumulative_stats(&gconn).unwrap();
    assert_eq!(stats.sessions_count, 1, "one session should be aggregated");
    assert_eq!(stats.handles_count, 2, "two handles should be counted");
    assert_eq!(stats.tokens_intercepted, 80, "token_est sum should be 50+30=80");

    std::env::remove_var("CTXL_CACHE_ROOT");
}

#[test]
fn sweep_deletes_even_if_aggregation_fails() {
    let _lock = ENV_LOCK.lock().unwrap();
    let tmp = tempfile::tempdir().expect("tempdir");
    let cache_root = tmp.path();
    // Point CTXL_CACHE_ROOT to a non-writable path so global DB can't be opened
    std::env::set_var("CTXL_CACHE_ROOT", "/nonexistent/path/for/global/db");

    // Create a stale session
    let session_dir = cache_root.join("ctxl").join("fail-agg-session");
    std::fs::create_dir_all(&session_dir).unwrap();
    let last_used = session_dir.join(".last_used");
    let f = File::create(&last_used).unwrap();
    let ten_days_ago = UNIX_EPOCH
        + Duration::from_secs(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                .saturating_sub(10 * 86_400),
        );
    f.set_times(FileTimes::new().set_modified(ten_days_ago)).unwrap();

    // Create a store.db
    let store_db = session_dir.join("store.db");
    let conn = rusqlite::Connection::open(&store_db).unwrap();
    ctxl::db::apply_schema(&conn).unwrap();
    drop(conn);

    // Sweep should still delete the session even though global DB is inaccessible
    clean::sweep(cache_root, Duration::from_secs(7 * 86_400)).unwrap();

    assert!(
        !session_dir.exists(),
        "session should be deleted even when aggregation fails (fail-open)"
    );

    std::env::remove_var("CTXL_CACHE_ROOT");
}
