#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Integration tests for the unified `intercept` subcommand router.
//!
//! These exercise the binary's `main.rs` router — the dispatch logic that
//! peeks `tool_name`, applies kill-switches, validates session IDs, and routes
//! to the correct handler.  Unit tests in each handler's test file cover the
//! handler-level logic; these tests cover the glue.

use std::io::Write as _;
use std::process::{Command, Stdio};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Spawn the ctxl binary with `intercept --session-id <sid>`, pipe `input`
/// to stdin, and return (stdout, stderr, exit_code).
fn run_intercept_binary(
    input: &str,
    session_id: &str,
    cache_root: &std::path::Path,
    extra_env: &[(&str, &str)],
) -> (Vec<u8>, Vec<u8>, i32) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ctxl"));
    cmd.args(["intercept", "--session-id", session_id])
        .env("CTXL_CACHE_ROOT", cache_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    for (key, val) in extra_env {
        cmd.env(key, val);
    }

    let mut child = cmd.spawn().expect("failed to spawn ctxl binary");

    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(input.as_bytes()).unwrap();
    }
    // Drop stdin to signal EOF.
    drop(child.stdin.take());

    let output = child.wait_with_output().expect("failed to wait for ctxl");
    let code = output.status.code().unwrap_or(-1);
    (output.stdout, output.stderr, code)
}

/// Create a temp dir with an initialized session DB.
fn setup_session(session_id: &str) -> tempfile::TempDir {
    let tmp = tempfile::TempDir::new().unwrap();
    ctxl::db::init_session_at(tmp.path(), session_id).unwrap();
    tmp
}

// ---------------------------------------------------------------------------
// CTXL_ENABLED=0 kill switch
// ---------------------------------------------------------------------------

#[test]
fn kill_switch_exits_zero_no_output() {
    let tmp = setup_session("kill-switch");
    let payload = r#"{"tool_name":"Bash","tool_response":{"stdout":"hello","stderr":""}}"#;

    let (stdout, _stderr, code) =
        run_intercept_binary(payload, "kill-switch", tmp.path(), &[("CTXL_ENABLED", "0")]);

    assert_eq!(code, 0, "CTXL_ENABLED=0 must exit 0");
    assert!(stdout.is_empty(), "CTXL_ENABLED=0 must produce no stdout");
}

// ---------------------------------------------------------------------------
// Unknown tool → passthrough (exit 0, no output, no DB write)
// ---------------------------------------------------------------------------

#[test]
fn unknown_tool_passthrough() {
    let tmp = setup_session("unknown-tool");
    let payload = r#"{"tool_name":"FancyNewTool","tool_response":"data"}"#;

    let (stdout, _stderr, code) = run_intercept_binary(payload, "unknown-tool", tmp.path(), &[]);

    assert_eq!(code, 0, "unknown tool must exit 0");
    assert!(stdout.is_empty(), "unknown tool must produce no stdout");
}

// ---------------------------------------------------------------------------
// Malformed JSON → fail-open (exit 0)
// ---------------------------------------------------------------------------

#[test]
fn malformed_json_fail_open() {
    let tmp = setup_session("malformed");

    let (stdout, _stderr, code) =
        run_intercept_binary("not json at all {{{", "malformed", tmp.path(), &[]);

    assert_eq!(code, 0, "malformed JSON must exit 0 (fail-open)");
    assert!(stdout.is_empty(), "malformed JSON must produce no stdout");
}

// ---------------------------------------------------------------------------
// Empty stdin → fail-open (exit 0)
// ---------------------------------------------------------------------------

#[test]
fn empty_stdin_fail_open() {
    let tmp = setup_session("empty-stdin");

    let (stdout, _stderr, code) = run_intercept_binary("", "empty-stdin", tmp.path(), &[]);

    assert_eq!(code, 0, "empty stdin must exit 0 (fail-open)");
    assert!(stdout.is_empty(), "empty stdin must produce no stdout");
}

// ---------------------------------------------------------------------------
// Session ID validation — path traversal rejected
// ---------------------------------------------------------------------------

#[test]
fn invalid_session_id_rejected() {
    let tmp = tempfile::TempDir::new().unwrap();
    let payload = r#"{"tool_name":"Bash","tool_response":{"stdout":"x","stderr":""}}"#;

    let (stdout, stderr, code) = run_intercept_binary(payload, "../../etc/passwd", tmp.path(), &[]);

    assert_eq!(code, 0, "invalid session_id must exit 0 (fail-open)");
    assert!(stdout.is_empty(), "invalid session_id must produce no stdout");
    let stderr_str = String::from_utf8_lossy(&stderr);
    assert!(
        stderr_str.contains("invalid session_id"),
        "must log validation error, got: {stderr_str}"
    );
}

// ---------------------------------------------------------------------------
// Norecord via CTXL_RECORD=0 — intercept still works, calls row suppressed
// ---------------------------------------------------------------------------

#[test]
fn norecord_env_suppresses_calls_row() {
    let tmp = setup_session("norecord");
    // Large enough to trigger interception (>8192 bytes).
    let big_output = "x".repeat(10_000);
    let payload = serde_json::json!({
        "tool_name": "Bash",
        "tool_response": {
            "stdout": big_output,
            "stderr": "",
            "interrupted": false,
            "isImage": false,
            "noOutputExpected": false,
        }
    })
    .to_string();

    let (stdout, _stderr, code) =
        run_intercept_binary(&payload, "norecord", tmp.path(), &[("CTXL_RECORD", "0")]);

    assert_eq!(code, 0);
    // Interception still happened — stdout has the envelope.
    assert!(!stdout.is_empty(), "intercept must still produce output with norecord");

    let envelope: serde_json::Value =
        serde_json::from_slice(&stdout).expect("stdout must be valid JSON");
    assert!(
        envelope.get("hookSpecificOutput").is_some(),
        "must produce hookSpecificOutput envelope"
    );

    // But calls table should be empty — recording suppressed.
    let db_path = ctxl::db::session_dir_at(tmp.path(), "norecord").join("store.db");
    let conn = rusqlite::Connection::open(db_path).unwrap();
    let calls_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM calls", [], |row| row.get(0)).unwrap();
    assert_eq!(calls_count, 0, "norecord must suppress calls INSERT");
}

// ---------------------------------------------------------------------------
// Norecord via temp file — same behavior as env var
// ---------------------------------------------------------------------------

#[test]
fn norecord_tempfile_suppresses_calls_row() {
    let tmp = setup_session("norecord-file");
    let big_output = "x".repeat(10_000);
    let payload = serde_json::json!({
        "tool_name": "Bash",
        "tool_response": {
            "stdout": big_output,
            "stderr": "",
            "interrupted": false,
            "isImage": false,
            "noOutputExpected": false,
        }
    })
    .to_string();

    // Create the norecord sentinel file.
    let tmpdir = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into());
    let sentinel = std::path::Path::new(&tmpdir).join("ctxl-norecord-norecord-file");
    std::fs::write(&sentinel, "").unwrap();

    let (stdout, _stderr, code) = run_intercept_binary(&payload, "norecord-file", tmp.path(), &[]);

    // Clean up sentinel.
    let _ = std::fs::remove_file(&sentinel);

    assert_eq!(code, 0);
    assert!(!stdout.is_empty(), "intercept must still work with norecord tempfile");

    let db_path = ctxl::db::session_dir_at(tmp.path(), "norecord-file").join("store.db");
    let conn = rusqlite::Connection::open(db_path).unwrap();
    let calls_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM calls", [], |row| row.get(0)).unwrap();
    assert_eq!(calls_count, 0, "norecord tempfile must suppress calls INSERT");
}

// ---------------------------------------------------------------------------
// Record-only tool (Read) — no interception, just calls row
// ---------------------------------------------------------------------------

#[test]
fn record_only_tool_writes_calls_row() {
    let tmp = setup_session("record-only");
    let payload = serde_json::json!({
        "tool_name": "Read",
        "tool_input": { "file_path": "/tmp/test.txt" },
        "tool_response": { "content": "file contents here" }
    })
    .to_string();

    let (stdout, _stderr, code) = run_intercept_binary(&payload, "record-only", tmp.path(), &[]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty(), "record-only tools must produce no stdout");

    let db_path = ctxl::db::session_dir_at(tmp.path(), "record-only").join("store.db");
    let conn = rusqlite::Connection::open(db_path).unwrap();
    let calls_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM calls WHERE tool = 'Read'", [], |row| row.get(0))
        .unwrap();
    assert_eq!(calls_count, 1, "record-only tool must insert a calls row");
}

// ---------------------------------------------------------------------------
// Record-only tool with norecord — no calls row, no output
// ---------------------------------------------------------------------------

#[test]
fn record_only_tool_norecord_skips_everything() {
    let tmp = setup_session("record-norecord");
    let payload = r#"{"tool_name":"Read","tool_response":{"content":"contents"}}"#;

    let (stdout, _stderr, code) =
        run_intercept_binary(payload, "record-norecord", tmp.path(), &[("CTXL_RECORD", "0")]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());

    let db_path = ctxl::db::session_dir_at(tmp.path(), "record-norecord").join("store.db");
    let conn = rusqlite::Connection::open(db_path).unwrap();
    let calls_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM calls", [], |row| row.get(0)).unwrap();
    assert_eq!(calls_count, 0, "norecord must skip record-only tools entirely");
}

// ---------------------------------------------------------------------------
// Handler error → fail-open (exit 0) — wrong tool_response shape
// ---------------------------------------------------------------------------

#[test]
fn handler_error_fail_open() {
    let tmp = setup_session("handler-err");
    // Bash handler expects tool_response as object with stdout/stderr,
    // but we send a bare string — serde deserialization will fail.
    let payload = r#"{"tool_name":"Bash","tool_response":"not an object"}"#;

    let (stdout, stderr, code) = run_intercept_binary(payload, "handler-err", tmp.path(), &[]);

    assert_eq!(code, 0, "handler error must exit 0 (fail-open), got: {code}");
    assert!(stdout.is_empty(), "handler error must produce no stdout");
    let stderr_str = String::from_utf8_lossy(&stderr);
    assert!(stderr_str.contains("[ctxl] error"), "must log error to stderr, got: {stderr_str}");
}

// ---------------------------------------------------------------------------
// Bash below threshold → passthrough (exit 0, no output)
// ---------------------------------------------------------------------------

#[test]
fn bash_below_threshold_passthrough() {
    let tmp = setup_session("below-thresh");
    let payload = serde_json::json!({
        "tool_name": "Bash",
        "tool_response": {
            "stdout": "short output",
            "stderr": "",
            "interrupted": false,
            "isImage": false,
            "noOutputExpected": false,
        }
    })
    .to_string();

    let (stdout, _stderr, code) = run_intercept_binary(&payload, "below-thresh", tmp.path(), &[]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty(), "below-threshold output must pass through unchanged");
}

// ---------------------------------------------------------------------------
// Bash above threshold → intercept produces envelope
// ---------------------------------------------------------------------------

#[test]
fn bash_above_threshold_intercepts() {
    let tmp = setup_session("above-thresh");
    let big = "x".repeat(10_000);
    let payload = serde_json::json!({
        "tool_name": "Bash",
        "tool_response": {
            "stdout": big,
            "stderr": "",
            "interrupted": false,
            "isImage": false,
            "noOutputExpected": false,
        }
    })
    .to_string();

    let (stdout, _stderr, code) = run_intercept_binary(&payload, "above-thresh", tmp.path(), &[]);

    assert_eq!(code, 0);
    assert!(!stdout.is_empty(), "above-threshold must produce intercept envelope");

    let envelope: serde_json::Value = serde_json::from_slice(&stdout).expect("must be valid JSON");
    let updated = &envelope["hookSpecificOutput"]["updatedToolOutput"];
    assert!(!updated.is_null(), "must have updatedToolOutput");

    let content = updated["stdout"].as_str().expect("stdout must be string");
    assert!(content.contains("b_"), "must contain a b_ handle reference");
}

// ---------------------------------------------------------------------------
// Grep routing — above threshold produces g_ handle
// ---------------------------------------------------------------------------

#[test]
fn grep_above_threshold_intercepts() {
    let tmp = setup_session("grep-route");
    // Build content with >200 lines (default grep threshold).
    let lines: Vec<String> =
        (0..250).map(|i| format!("src/file{i}.rs:10:match line {i}")).collect();
    let content = lines.join("\n");
    let filenames: Vec<String> = (0..250).map(|i| format!("src/file{i}.rs")).collect();
    let payload = serde_json::json!({
        "tool_name": "Grep",
        "tool_input": { "pattern": "match", "path": "src/" },
        "tool_response": {
            "content": content,
            "numFiles": 250,
            "filenames": filenames,
            "numMatches": 250,
            "mode": "content",
        }
    })
    .to_string();

    let (stdout, _stderr, code) = run_intercept_binary(&payload, "grep-route", tmp.path(), &[]);

    assert_eq!(code, 0);
    assert!(!stdout.is_empty(), "grep above threshold must produce intercept envelope");

    let envelope: serde_json::Value = serde_json::from_slice(&stdout).expect("must be valid JSON");
    let content_field =
        envelope["hookSpecificOutput"]["updatedToolOutput"]["content"].as_str().unwrap();
    assert!(content_field.contains("g_"), "grep handle must use g_ prefix");
}

// ---------------------------------------------------------------------------
// WebFetch — no longer intercepted (passthrough)
// ---------------------------------------------------------------------------

#[test]
fn webfetch_passthrough_not_intercepted() {
    let tmp = setup_session("webfetch-pass");
    let lines: Vec<String> = (0..250).map(|i| format!("<p>paragraph {i}</p>")).collect();
    let content = lines.join("\n");
    let payload = serde_json::json!({
        "tool_name": "WebFetch",
        "tool_input": { "url": "https://example.com" },
        "tool_response": {
            "content": content,
            "url": "https://example.com",
            "status_code": 200,
            "content_type": "text/html",
        }
    })
    .to_string();

    let (stdout, _stderr, code) = run_intercept_binary(&payload, "webfetch-pass", tmp.path(), &[]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty(), "WebFetch must not be intercepted (removed from pipeline)");
}

// ---------------------------------------------------------------------------
// Env threshold override — CTXL_BASH_THRESHOLD respected by router
// ---------------------------------------------------------------------------

#[test]
fn env_threshold_override_respected() {
    let tmp = setup_session("env-override");
    // Send 50 bytes — below default 8192, but above override of 10.
    let payload = serde_json::json!({
        "tool_name": "Bash",
        "tool_response": {
            "stdout": "x".repeat(50),
            "stderr": "",
            "interrupted": false,
            "isImage": false,
            "noOutputExpected": false,
        }
    })
    .to_string();

    let (stdout, _stderr, code) = run_intercept_binary(
        &payload,
        "env-override",
        tmp.path(),
        &[("CTXL_BASH_THRESHOLD", "10")],
    );

    assert_eq!(code, 0);
    assert!(!stdout.is_empty(), "env threshold override must cause interception of smaller output");
}

// ---------------------------------------------------------------------------
// Cascading interception — ctxl commands bypass Bash handler
// ---------------------------------------------------------------------------

#[test]
fn ctxl_show_command_not_intercepted() {
    let tmp = setup_session("ctxl-cascade");
    let big = "x".repeat(10_000);
    let payload = serde_json::json!({
        "tool_name": "Bash",
        "tool_input": { "command": "ctxl show b_abc123" },
        "tool_response": {
            "stdout": big,
            "stderr": "",
            "interrupted": false,
            "isImage": false,
            "noOutputExpected": false,
        }
    })
    .to_string();

    let (stdout, _stderr, code) = run_intercept_binary(&payload, "ctxl-cascade", tmp.path(), &[]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty(), "ctxl commands must not be re-intercepted");
}

#[test]
fn ctxl_search_command_not_intercepted() {
    let tmp = setup_session("ctxl-cascade-search");
    let big = "x".repeat(10_000);
    let payload = serde_json::json!({
        "tool_name": "Bash",
        "tool_input": { "command": "ctxl search b_abc123 \"error\"" },
        "tool_response": {
            "stdout": big,
            "stderr": "",
            "interrupted": false,
            "isImage": false,
            "noOutputExpected": false,
        }
    })
    .to_string();

    let (stdout, _stderr, code) =
        run_intercept_binary(&payload, "ctxl-cascade-search", tmp.path(), &[]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty(), "ctxl search must not be re-intercepted");
}

#[test]
fn ctxl_files_command_not_intercepted() {
    let tmp = setup_session("ctxl-cascade-files");
    let big = "x".repeat(10_000);
    let payload = serde_json::json!({
        "tool_name": "Bash",
        "tool_input": { "command": "  ctxl files g_abc123" },
        "tool_response": {
            "stdout": big,
            "stderr": "",
            "interrupted": false,
            "isImage": false,
            "noOutputExpected": false,
        }
    })
    .to_string();

    let (stdout, _stderr, code) =
        run_intercept_binary(&payload, "ctxl-cascade-files", tmp.path(), &[]);

    assert_eq!(code, 0);
    assert!(
        stdout.is_empty(),
        "ctxl files must not be re-intercepted (even with leading whitespace)"
    );
}

// ---------------------------------------------------------------------------
// CTXL_RECORD=0 — record-only tools produce zero DB I/O (no session DB created)
// ---------------------------------------------------------------------------

/// Verify that `CTXL_RECORD=0` causes record-only tools (Read/Edit/Write/Glob)
/// to exit before the session database is opened.  The guarantee is:
///   • exit 0
///   • no stdout output
///   • no session directory created (zero filesystem I/O for the DB)
///
/// This is the key property cited in issue #1476: the kill switch must
/// short-circuit before any SQLite I/O, not merely suppress the INSERT.
#[test]
fn norecord_read_tool_skips_db_open() {
    // Use a fresh temp dir with NO pre-initialised session — the binary must
    // return early before creating the session directory at all.
    let tmp = tempfile::TempDir::new().unwrap();
    let session_id = "norecord-zero-io";
    let payload = r#"{"tool_name":"Read","tool_input":{"file_path":"/tmp/x.txt"},"tool_response":{"content":"hello\nworld\n"}}"#;

    let (stdout, _stderr, code) =
        run_intercept_binary(payload, session_id, tmp.path(), &[("CTXL_RECORD", "0")]);

    assert_eq!(code, 0, "CTXL_RECORD=0 with Read must exit 0");
    assert!(stdout.is_empty(), "CTXL_RECORD=0 with Read must produce no stdout");

    // The session directory must not have been created — no DB I/O at all.
    let session_dir = ctxl::db::session_dir_at(tmp.path(), session_id);
    assert!(
        !session_dir.exists(),
        "session directory must not be created when CTXL_RECORD=0 and tool is Read"
    );
}

/// Same as above but for Edit (write-only tool — no content in tool_response).
#[test]
fn norecord_edit_tool_skips_db_open() {
    let tmp = tempfile::TempDir::new().unwrap();
    let session_id = "norecord-edit-io";
    let payload = r#"{"tool_name":"Edit","tool_input":{"file_path":"/tmp/x.txt","old_string":"a","new_string":"b"},"tool_response":{}}"#;

    let (stdout, _stderr, code) =
        run_intercept_binary(payload, session_id, tmp.path(), &[("CTXL_RECORD", "0")]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());

    let session_dir = ctxl::db::session_dir_at(tmp.path(), session_id);
    assert!(
        !session_dir.exists(),
        "session directory must not be created when CTXL_RECORD=0 and tool is Edit"
    );
}

#[test]
fn non_ctxl_bash_still_intercepted() {
    let tmp = setup_session("ctxl-cascade-other");
    let big = "x".repeat(10_000);
    let payload = serde_json::json!({
        "tool_name": "Bash",
        "tool_input": { "command": "grep -r foo src/" },
        "tool_response": {
            "stdout": big,
            "stderr": "",
            "interrupted": false,
            "isImage": false,
            "noOutputExpected": false,
        }
    })
    .to_string();

    let (stdout, _stderr, code) =
        run_intercept_binary(&payload, "ctxl-cascade-other", tmp.path(), &[]);

    assert_eq!(code, 0);
    assert!(!stdout.is_empty(), "non-ctxl Bash commands must still be intercepted");
}

// ---------------------------------------------------------------------------
// Full-path ctxl invocations not intercepted (#1952)
// ---------------------------------------------------------------------------

#[test]
fn ctxl_full_path_not_intercepted() {
    let tmp = setup_session("ctxl-fullpath");
    let big = "x".repeat(10_000);
    let payload = serde_json::json!({
        "tool_name": "Bash",
        "tool_input": { "command": "/Users/alex/.claude/plugins/ctxl/target/release/ctxl show b_abc123" },
        "tool_response": {
            "stdout": big,
            "stderr": "",
            "interrupted": false,
            "isImage": false,
            "noOutputExpected": false,
        }
    })
    .to_string();

    let (stdout, _stderr, code) = run_intercept_binary(&payload, "ctxl-fullpath", tmp.path(), &[]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty(), "full-path ctxl invocations must not be re-intercepted");
}

#[test]
fn ctxl_bin_env_var_not_intercepted() {
    let tmp = setup_session("ctxl-envvar");
    let big = "x".repeat(10_000);
    let ctxl_bin_path = "/opt/custom/bin/ctxl";
    let cmd = format!("{ctxl_bin_path} show b_abc123");
    let payload = serde_json::json!({
        "tool_name": "Bash",
        "tool_input": { "command": cmd },
        "tool_response": {
            "stdout": big,
            "stderr": "",
            "interrupted": false,
            "isImage": false,
            "noOutputExpected": false,
        }
    })
    .to_string();

    let (stdout, _stderr, code) =
        run_intercept_binary(&payload, "ctxl-envvar", tmp.path(), &[("CTXL_BIN", ctxl_bin_path)]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty(), "CTXL_BIN env var path must not be re-intercepted");
}
