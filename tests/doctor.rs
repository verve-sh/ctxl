#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use tempfile::TempDir;

fn init_db(cache_root: &std::path::Path, session_id: &str) -> rusqlite::Connection {
    ctxl::db::init_session_at(cache_root, session_id).unwrap();
    let db_path = ctxl::db::session_dir_at(cache_root, session_id).join("store.db");
    let conn = rusqlite::Connection::open(db_path).unwrap();
    ctxl::db::apply_schema(&conn).unwrap();
    conn
}

#[test]
fn doctor_fresh_session() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let sid = "doctor-fresh";
    init_db(root, sid);

    let report = ctxl::doctor::collect(Some(sid), root);

    assert_eq!(report.session_id.as_deref(), Some(sid));
    assert!(report.db_exists);
    assert!(report.schema_version.is_some());
    assert_eq!(report.handle_count, Some(0));
    assert_eq!(report.calls_count, Some(0));
    assert_eq!(report.intercepted_count, Some(0));
    assert_eq!(report.retrieved_count, Some(0));
    assert_eq!(report.retrieval_total, Some(0));
    assert!(report.hook_enabled);
    assert!(!report.norecord);
    assert!(report.version.contains('.'));
}

#[test]
fn doctor_with_handles() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let sid = "doctor-handles";
    let conn = init_db(root, sid);

    let payload = serde_json::json!({
        "tool": "Bash",
        "output_mode": "stdout",
        "stdout": "hello world\nline 2\n",
    });
    ctxl::store::write(&conn, payload).unwrap();

    ctxl::calls::insert_calls_row(
        &conn,
        &ctxl::calls::InterceptCallMeta {
            tool: "Bash",
            handle_id: "b_test01",
            line_count: 2,
            token_est: None,
            tool_input: None,
            cwd: None,
            tool_use_id: None,
            exit_code: None,
        },
    )
    .unwrap();
    ctxl::calls::insert_calls_row(
        &conn,
        &ctxl::calls::InterceptCallMeta {
            tool: "Bash",
            handle_id: "b_test02",
            line_count: 5,
            token_est: None,
            tool_input: None,
            cwd: None,
            tool_use_id: None,
            exit_code: None,
        },
    )
    .unwrap();

    drop(conn);

    let report = ctxl::doctor::collect(Some(sid), root);

    assert_eq!(report.handle_count, Some(1));
    assert_eq!(report.calls_count, Some(2));
    assert_eq!(report.intercepted_count, Some(2));
}

#[test]
fn doctor_no_session() {
    let tmp = TempDir::new().unwrap();
    let report = ctxl::doctor::collect(None, tmp.path());

    assert!(report.session_id.is_none());
    assert!(!report.db_exists);
    assert!(report.schema_version.is_none());
    assert!(report.handle_count.is_none());
    assert!(report.calls_count.is_none());
    assert!(report.intercepted_count.is_none());
    assert!(report.retrieved_count.is_none());
    assert!(report.retrieval_total.is_none());
}

#[test]
fn doctor_thresholds_from_env() {
    // Run doctor via subprocess to avoid `set_var` races under parallel tests.
    // Each subprocess gets its own isolated environment.
    let tmp = TempDir::new().unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ctxl"))
        .args(["doctor", "--json"])
        .env("CTXL_CACHE_ROOT", tmp.path())
        .env("CTXL_BASH_THRESHOLD", "4096")
        .env("CTXL_GREP_THRESHOLD", "100")
        .env("CTXL_BASH_LINE_THRESHOLD", "150")
        .output()
        .expect("spawn ctxl doctor");

    assert!(output.status.success(), "doctor should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("doctor JSON parse failed: {e}\nstdout: {stdout}"));

    assert_eq!(parsed["thresholds"]["bash_bytes"], 4096);
    assert_eq!(parsed["thresholds"]["grep_lines"], 100);
    assert_eq!(parsed["thresholds"]["bash_lines"], 150);
}

#[test]
fn doctor_json_output_is_valid() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let sid = "doctor-json";
    init_db(root, sid);

    let report = ctxl::doctor::collect(Some(sid), root);
    let json_str = ctxl::doctor::format_json(&report);
    let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();

    assert_eq!(parsed["session_id"], "doctor-json");
    assert_eq!(parsed["db_exists"], true);
    assert_eq!(parsed["handle_count"], 0);
    assert_eq!(parsed["retrieved_count"], 0);
    assert_eq!(parsed["retrieval_total"], 0);
}

#[test]
fn doctor_human_output_contains_key_fields() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let sid = "doctor-human";
    init_db(root, sid);

    let report = ctxl::doctor::collect(Some(sid), root);
    let human = ctxl::doctor::format_human(&report);

    assert!(human.contains("ctxl v"));
    assert!(human.contains("session_id:"));
    assert!(human.contains("doctor-human"));
    assert!(human.contains("db_exists:"));
    assert!(human.contains("Thresholds:"));
    assert!(human.contains("retrieved:"));
    assert!(human.contains("retrieval_total:"));
    assert!(human.contains("Liveness:"));
    assert!(human.contains("last_interception:"));
}

// ---------------------------------------------------------------------------
// Liveness
// ---------------------------------------------------------------------------

fn set_db_mtime(cache_root: &std::path::Path, sid: &str, secs_ago: u64) {
    let db = ctxl::db::session_dir_at(cache_root, sid).join("store.db");
    let t = std::time::SystemTime::now() - std::time::Duration::from_secs(secs_ago);
    let f = std::fs::File::options().write(true).open(db).unwrap();
    f.set_modified(t).unwrap();
}

#[test]
fn liveness_warns_after_consecutive_quiet_sessions() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    for i in 0..6u64 {
        let sid = format!("quiet-{i}");
        drop(init_db(root, &sid));
        set_db_mtime(root, &sid, 100 + i);
    }

    let live = ctxl::doctor::liveness(root, None);

    assert_eq!(live.recent_quiet_sessions, 6);
    assert!(live.last_interception_days_ago.is_none());
    let warning = live.warning.expect("6 quiet sessions must warn");
    assert!(warning.contains("hook chain may be broken"), "{warning}");
}

#[test]
fn liveness_recent_interception_clears_warning() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    for i in 0..5u64 {
        let sid = format!("older-quiet-{i}");
        drop(init_db(root, &sid));
        set_db_mtime(root, &sid, 5_000 + i);
    }
    let conn = init_db(root, "active");
    ctxl::store::write(
        &conn,
        serde_json::json!({"tool": "Bash", "output_mode": "stdout", "stdout": "big\n"}),
    )
    .unwrap();
    drop(conn);
    set_db_mtime(root, "active", 60);

    let live = ctxl::doctor::liveness(root, None);

    // Newest session intercepted — quiet streak is zero regardless of history.
    assert_eq!(live.recent_quiet_sessions, 0);
    assert_eq!(live.last_interception_days_ago, Some(0));
    assert!(live.warning.is_none());
}

#[test]
fn liveness_skips_test_sessions_and_current() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    drop(init_db(root, "test-synthetic"));
    drop(init_db(root, "current-session"));

    let live = ctxl::doctor::liveness(root, Some("current-session"));

    assert_eq!(live.scanned_sessions, 0);
    assert_eq!(live.recent_quiet_sessions, 0);
    assert!(live.warning.is_none());
}
