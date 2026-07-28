#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Phase 1 validation gates -- integration tests for ctxl reliability-critical
//! edge cases: passthrough conditions, output-mode combinations, WAL isolation,
//! FTS NULL safety, error propagation on schema failure, ANSI-stripped
//! threshold measurement, and threshold boundary behavior.

use ctxl::{
    db,
    intercept::{self, InterceptConfig, ToolResponse},
    payload::PostToolUsePayload,
};
use rusqlite::Connection;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn in_memory_conn() -> Connection {
    let conn = Connection::open_in_memory().expect("open_in_memory");
    db::apply_schema(&conn).expect("apply_schema");
    conn
}

fn make_payload(
    stdout: &str,
    stderr: &str,
    interrupted: bool,
    is_image: bool,
) -> PostToolUsePayload<ToolResponse> {
    PostToolUsePayload {
        tool_name: Some("Bash".into()),
        tool_response: ToolResponse {
            stdout: Some(stdout.into()),
            stderr: Some(stderr.into()),
            interrupted: Some(interrupted),
            is_image: Some(is_image),
            no_output_expected: Some(false),
            exit_code: None,
        },
    }
}

fn default_config(threshold: usize) -> InterceptConfig {
    InterceptConfig {
        threshold,
        line_threshold: 200,
        record: true,
        command: None,
        tool_input: None,
        cwd: None,
        tool_use_id: None,
    }
}

// ---------------------------------------------------------------------------
// 3a. Below-threshold passthrough verification
// ---------------------------------------------------------------------------

#[test]
fn test_below_threshold_passthrough_no_handle() {
    let conn = in_memory_conn();
    let payload = make_payload("small output\n", "", false, false);
    let config = default_config(8192);
    let mut output = Vec::new();

    intercept::run(payload, &mut output, &config, &conn).expect("run should succeed");

    assert!(
        output.is_empty(),
        "below-threshold payload should produce empty output, got {} bytes",
        output.len()
    );

    let handle_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM handles", [], |row| row.get(0)).unwrap();
    assert_eq!(handle_count, 0, "no handle should exist for below-threshold output");
}

#[test]
fn test_below_threshold_passthrough_no_calls_row() {
    let conn = in_memory_conn();
    let payload = make_payload("small output\n", "", false, false);
    let config = default_config(8192);
    let mut output = Vec::new();

    intercept::run(payload, &mut output, &config, &conn).expect("run should succeed");

    let calls_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM calls", [], |row| row.get(0)).unwrap();
    assert_eq!(calls_count, 0, "no calls row should exist for below-threshold output");
}

// ---------------------------------------------------------------------------
// 3b. stdout-only, stderr-only, mixed, empty output
// ---------------------------------------------------------------------------

#[test]
fn test_stdout_only_stored() {
    let conn = in_memory_conn();
    let big_stdout = "x".repeat(9000);
    let payload = make_payload(&big_stdout, "", false, false);
    let config = default_config(8192);
    let mut output = Vec::new();

    intercept::run(payload, &mut output, &config, &conn).expect("run should succeed");

    assert!(!output.is_empty(), "above-threshold stdout-only should be intercepted");

    let handle_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM handles", [], |row| row.get(0)).unwrap();
    assert_eq!(handle_count, 1, "exactly one handle should exist");

    let content: String =
        conn.query_row("SELECT content FROM handles", [], |row| row.get(0)).unwrap();
    assert!(content.contains(&"x".repeat(100)), "stored content should contain stdout data");
}

#[test]
fn test_stderr_only_stored() {
    let conn = in_memory_conn();
    let big_stderr = "e".repeat(9000);
    let payload = make_payload("", &big_stderr, false, false);
    let config = default_config(8192);
    let mut output = Vec::new();

    intercept::run(payload, &mut output, &config, &conn).expect("run should succeed");

    assert!(!output.is_empty(), "above-threshold stderr-only should be intercepted");

    let handle_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM handles", [], |row| row.get(0)).unwrap();
    assert_eq!(handle_count, 1, "exactly one handle should exist");

    let content: String =
        conn.query_row("SELECT content FROM handles", [], |row| row.get(0)).unwrap();
    assert!(content.contains(&"e".repeat(100)), "stored content should contain stderr data");
}

#[test]
fn test_mixed_stdout_stderr_stored() {
    let conn = in_memory_conn();
    let stdout_part = "o".repeat(5000);
    let stderr_part = "e".repeat(5000);
    let payload = make_payload(&stdout_part, &stderr_part, false, false);
    let config = default_config(8192);
    let mut output = Vec::new();

    intercept::run(payload, &mut output, &config, &conn).expect("run should succeed");

    assert!(!output.is_empty(), "above-threshold mixed output should be intercepted");

    let content: String =
        conn.query_row("SELECT content FROM handles", [], |row| row.get(0)).unwrap();

    assert!(content.contains(&"o".repeat(100)), "stored content should contain stdout data");
    assert!(content.contains(&"e".repeat(100)), "stored content should contain stderr data");

    let separator_pos = content.find(&"e".repeat(100)).expect("stderr data should exist");
    assert!(
        separator_pos > 0 && content.as_bytes()[separator_pos - 1] == b'\n',
        "newline separator should exist between stdout and stderr when stdout has no trailing newline"
    );
}

#[test]
fn test_empty_output_passthrough() {
    let conn = in_memory_conn();
    let payload = make_payload("", "", false, false);
    let config = default_config(8192);
    let mut output = Vec::new();

    intercept::run(payload, &mut output, &config, &conn).expect("run should succeed");

    assert!(output.is_empty(), "empty output should passthrough");

    let handle_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM handles", [], |row| row.get(0)).unwrap();
    assert_eq!(handle_count, 0, "no handle should exist for empty output");
}

// ---------------------------------------------------------------------------
// 3c. Concurrent session isolation under WAL
// ---------------------------------------------------------------------------

#[test]
fn test_concurrent_wal_isolation() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let session_id = "phase1-wal-test";
    db::init_session_at(tmp.path(), session_id).expect("init_session_at");

    let db_path = db::session_dir_at(tmp.path(), session_id).join("store.db");

    {
        let conn = Connection::open(&db_path).expect("open db");
        let mode: String =
            conn.query_row("PRAGMA journal_mode", [], |row| row.get(0)).expect("journal_mode");
        assert_eq!(mode, "wal", "journal_mode should be wal");
    }

    let db_path_1 = db_path.clone();
    let db_path_2 = db_path.clone();

    let handle_1 = std::thread::spawn(move || {
        let conn = Connection::open(&db_path_1).expect("open db thread 1");
        let payload = serde_json::json!({
            "tool": "Bash",
            "output_mode": "stdout",
            "content": "thread 1 content"
        });
        ctxl::store::write(&conn, payload).expect("write thread 1")
    });

    let handle_2 = std::thread::spawn(move || {
        let conn = Connection::open(&db_path_2).expect("open db thread 2");
        let payload = serde_json::json!({
            "tool": "Bash",
            "output_mode": "stdout",
            "content": "thread 2 content"
        });
        ctxl::store::write(&conn, payload).expect("write thread 2")
    });

    let id1 = handle_1.join().expect("thread 1 join");
    let id2 = handle_2.join().expect("thread 2 join");

    assert_ne!(id1, id2, "concurrent handles should have distinct IDs");

    let conn = Connection::open(&db_path).expect("open db for verification");
    let handle_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM handles", [], |row| row.get(0)).unwrap();
    assert_eq!(handle_count, 2, "both handles should be persisted");

    // Verify each handle's content matches the thread that wrote it
    let content1: String = conn
        .query_row("SELECT content FROM handles WHERE id = ?1", [&id1], |row| row.get(0))
        .expect("query content for handle 1");
    assert!(
        content1.contains("thread 1 content"),
        "handle {id1} should contain thread 1's content, got: {content1}"
    );

    let content2: String = conn
        .query_row("SELECT content FROM handles WHERE id = ?1", [&id2], |row| row.get(0))
        .expect("query content for handle 2");
    assert!(
        content2.contains("thread 2 content"),
        "handle {id2} should contain thread 2's content, got: {content2}"
    );
}

// ---------------------------------------------------------------------------
// 3d. FTS NULL-content trigger behavior
// ---------------------------------------------------------------------------

#[test]
fn test_fts_null_content_no_crash() {
    let conn = in_memory_conn();

    conn.execute(
        "INSERT INTO handles (id, tool, output_mode, content, created_at) \
         VALUES ('b_test01', 'Bash', 'mixed', NULL, 0)",
        [],
    )
    .expect("insert with NULL content should succeed");

    let results =
        ctxl::retrieve::search(&conn, "b_test01", "anything", 10).expect("search should not error");
    assert!(results.is_empty(), "search on NULL-content handle should return empty Vec");

    let shown = ctxl::retrieve::show(&conn, "b_test01", ctxl::retrieve::ShowOpts::default())
        .expect("show should not error");
    assert!(shown.is_empty(), "show on NULL-content handle should return empty string");
}

// ---------------------------------------------------------------------------
// 3e. Error propagation on schema failure
// ---------------------------------------------------------------------------

#[test]
fn test_intercept_returns_err_on_schema_error() {
    let conn = Connection::open_in_memory().expect("open_in_memory");

    let big_stdout = "x".repeat(9000);
    let payload = make_payload(&big_stdout, "", false, false);
    let config = default_config(8192);
    let mut output = Vec::new();

    let result = intercept::run(payload, &mut output, &config, &conn);

    assert!(result.is_err(), "intercept::run should return Err on schema-less connection");

    let big_stdout2 = "y".repeat(9000);
    let payload2 = make_payload(&big_stdout2, "", false, false);
    let mut output2 = Vec::new();
    let config2 = default_config(8192);

    let result2 = intercept::run(payload2, &mut output2, &config2, &conn);
    assert!(
        result2.is_err(),
        "second intercept::run should also return Err on schema-less connection"
    );
}

// ---------------------------------------------------------------------------
// 3f. Threshold measured on ANSI-stripped size
// ---------------------------------------------------------------------------

#[test]
fn test_ansi_stripped_before_threshold() {
    let conn = in_memory_conn();

    let plain = "x".repeat(7000);
    let ansi_padding = "\x1b[31m".repeat(240);
    let content = format!("{ansi_padding}{plain}");
    assert!(content.len() > 8192, "raw bytes should exceed threshold");

    let payload = make_payload(&content, "", false, false);
    let config = default_config(8192);
    let mut output = Vec::new();

    intercept::run(payload, &mut output, &config, &conn).expect("run should succeed");

    assert!(
        output.is_empty(),
        "stripped size (7000) is below threshold, should passthrough, but got {} bytes of output",
        output.len()
    );
}

#[test]
fn test_ansi_raw_stored_when_above_threshold() {
    let conn = in_memory_conn();

    let plain = "y".repeat(9000);
    let ansi_content = format!("\x1b[31m{plain}\x1b[0m");

    let payload = make_payload(&ansi_content, "", false, false);
    let config = default_config(8192);
    let mut output = Vec::new();

    intercept::run(payload, &mut output, &config, &conn).expect("run should succeed");

    assert!(!output.is_empty(), "above-threshold content should be intercepted");

    let handle_id: String = conn.query_row("SELECT id FROM handles", [], |row| row.get(0)).unwrap();

    let stored = ctxl::retrieve::show(&conn, &handle_id, ctxl::retrieve::ShowOpts::default())
        .expect("show should succeed");

    assert!(stored.contains("\x1b["), "stored content should retain raw ANSI sequences");
}

// ---------------------------------------------------------------------------
// 3g. Line threshold triggers interception
// ---------------------------------------------------------------------------

#[test]
fn test_line_threshold_triggers_interception() {
    let conn = in_memory_conn();

    let stdout: String = (0..210).map(|i| format!("short line {:03}\n", i)).collect();
    assert!(stdout.len() < 8192, "payload should be below byte threshold");
    assert!(stdout.lines().count() >= 210, "payload should have at least 210 lines");

    let payload = make_payload(&stdout, "", false, false);
    let config = default_config(8192);
    let mut output = Vec::new();

    intercept::run(payload, &mut output, &config, &conn).expect("run should succeed");

    assert!(!output.is_empty(), "210 lines (> 200 line threshold) should trigger interception");

    let handle_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM handles", [], |row| row.get(0)).unwrap();
    assert_eq!(handle_count, 1, "handle should be created when line threshold exceeded");
}

// ---------------------------------------------------------------------------
// 3h. Interrupted passthrough boundary tests
// ---------------------------------------------------------------------------

#[test]
fn test_interrupted_below_2x_passthrough() {
    let conn = in_memory_conn();

    let stdout = "i".repeat(12000);
    let payload = make_payload(&stdout, "", true, false);
    let config = default_config(8192);
    let mut output = Vec::new();

    intercept::run(payload, &mut output, &config, &conn).expect("run should succeed");

    assert!(
        output.is_empty(),
        "interrupted=true with 12000 bytes (< 2x threshold of 16384) should passthrough",
    );

    let handle_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM handles", [], |row| row.get(0)).unwrap();
    assert_eq!(handle_count, 0, "no handle should exist for interrupted below-2x passthrough");
}

#[test]
fn test_interrupted_above_2x_intercepted() {
    let conn = in_memory_conn();

    let stdout = "j".repeat(20000);
    let payload = make_payload(&stdout, "", true, false);
    let config = default_config(8192);
    let mut output = Vec::new();

    intercept::run(payload, &mut output, &config, &conn).expect("run should succeed");

    assert!(
        !output.is_empty(),
        "interrupted=true with 20000 bytes (>= 2x threshold of 16384) should be intercepted",
    );

    let json: serde_json::Value =
        serde_json::from_slice(&output).expect("output should be valid JSON");
    assert!(
        json.get("hookSpecificOutput").is_some(),
        "intercepted output must contain hookSpecificOutput"
    );

    let interrupted_val = json["hookSpecificOutput"]["updatedToolOutput"]["interrupted"].as_bool();
    assert_eq!(
        interrupted_val,
        Some(true),
        "interrupted flag should be preserved as true in envelope"
    );

    let stdout_val =
        json["hookSpecificOutput"]["updatedToolOutput"]["stdout"].as_str().unwrap_or("");
    assert!(!stdout_val.is_empty(), "stdout field should contain a non-empty block message");
    assert!(
        stdout_val.contains("b_"),
        "stdout block message should contain handle ID with 'b_' prefix, got: {}",
        &stdout_val[..stdout_val.len().min(200)]
    );
}
