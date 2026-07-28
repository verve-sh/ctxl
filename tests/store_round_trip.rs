#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used, clippy::panic)] // integration tests

use ctxl::{db, retrieve, store, CtxlError};
use rusqlite::Connection;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn in_memory_conn() -> Connection {
    let conn = Connection::open_in_memory().expect("open_in_memory");
    db::apply_schema(&conn).expect("apply_schema");
    conn
}

// ---------------------------------------------------------------------------
// AC-1112-01
// ---------------------------------------------------------------------------

// @ac AC-1112-01
#[test]
fn store_write_returns_valid_handle_id() {
    // Verify: Calling `store::write(&conn, payload_with_tool="Bash")` returns
    //         `Ok(id)` where `id` matches regex `^b_[0-9a-f]{6}$`.
    // Verify: The corresponding row in `handles` has `tool="Bash"`,
    //         `params` is the input JSON, `cwd` equals the payload's cwd.

    let conn = in_memory_conn();
    let payload = serde_json::json!({
        "tool": "Bash",
        "output_mode": "stdout",
        "cwd": "/tmp/testdir",
        "content": "hello from bash"
    });

    let id = store::write(&conn, payload.clone()).expect("write should succeed");

    // --- Format checks ---
    assert!(id.starts_with("b_"), "id should start with 'b_', got: {id}");
    let hex_part = &id[2..];
    assert_eq!(hex_part.len(), 6, "hex part should be 6 chars, got {hex_part:?}");
    assert!(
        hex_part.chars().all(|c| c.is_ascii_hexdigit()),
        "hex part must be lowercase hex, got {hex_part:?}"
    );

    // --- Database checks ---
    let (tool, params_json, cwd): (String, String, String) = conn
        .query_row("SELECT tool, params, cwd FROM handles WHERE id=?1", [&id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .expect("row should exist in handles");

    assert_eq!(tool, "Bash", "tool should be 'Bash'");
    assert_eq!(cwd, "/tmp/testdir", "cwd should match payload");

    let params_val: serde_json::Value =
        serde_json::from_str(&params_json).expect("params should be valid JSON");
    // M1: params strips bulky content keys (stdout/stderr/content) to avoid
    // redundant storage — content is in its own column.
    let mut expected = payload.clone();
    expected.as_object_mut().unwrap().remove("content");
    assert_eq!(
        params_val, expected,
        "params should equal the input payload minus stripped content keys"
    );
}

// ---------------------------------------------------------------------------
// AC-1112-02
// ---------------------------------------------------------------------------

// @ac AC-1112-02
#[test]
fn store_retrieve_round_trips_content_byte_exact() {
    // Verify: For each of stdout-only, stderr-only, and mixed inputs:
    //         after `store::write` returns handle `h`,
    //         `retrieve::show(&conn, h, ShowOpts::default())` returns
    //         the original content byte-for-byte (within the 80-line default).

    let conn = in_memory_conn();

    let cases: &[(&str, serde_json::Value)] = &[
        (
            "stdout-only",
            serde_json::json!({
                "tool": "Bash",
                "output_mode": "stdout",
                "cwd": "/",
                "content": "stdout line 1\nstdout line 2\nstdout line 3\n"
            }),
        ),
        (
            "stderr-only",
            serde_json::json!({
                "tool": "Bash",
                "output_mode": "stderr",
                "cwd": "/",
                "content": "stderr line 1\nstderr line 2\n"
            }),
        ),
        (
            "mixed",
            serde_json::json!({
                "tool": "Bash",
                "output_mode": "mixed",
                "cwd": "/",
                "content": "mixed line 1\nmixed line 2\nmixed line 3\n"
            }),
        ),
    ];

    for (label, payload) in cases {
        let original = payload["content"].as_str().expect("content field").to_string();

        let h = store::write(&conn, payload.clone())
            .unwrap_or_else(|e| panic!("[{label}] write failed: {e}"));

        let result = retrieve::show(&conn, &h, retrieve::ShowOpts::default())
            .unwrap_or_else(|e| panic!("[{label}] show failed: {e}"));

        assert_eq!(
            result, original,
            "[{label}] round-trip content mismatch\n  got:  {result:?}\n  want: {original:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// AC-1112-07
// ---------------------------------------------------------------------------

// @ac AC-1112-07
#[test]
fn handle_id_unique_collision_retries_with_extra_digit_then_errors() {
    // Verify: Forcing a UNIQUE constraint violation on first insert causes
    //         `store::write` to retry once with one additional hex digit.
    // Verify: If the retry also collides, returns `Err` with message
    //         containing "handle ID collision after retry".

    let conn = in_memory_conn();
    let payload = serde_json::json!({
        "tool": "Bash",
        "output_mode": "stdout",
        "cwd": "/",
        "content": "test"
    });

    // --- Scenario A: first collision, retry (7-char) succeeds ---
    //
    // Pre-insert the 6-char ID so the first attempt collides.
    conn.execute(
        "INSERT INTO handles \
         (id, tool, output_mode, params, cwd, content, line_count, created_at) \
         VALUES ('b_aaaaaa', 'Bash', 'stdout', '{}', '/', 'pre', 1, '0')",
        [],
    )
    .expect("pre-insert b_aaaaaa");

    let result_a = store::write_with_id_gen(&conn, payload.clone(), |len| {
        // gen(6) → 'aaaaaa'  (will collide with pre-inserted row)
        // gen(7) → 'bbbbbbb' (fresh, should succeed)
        Ok(if len == 6 { "aaaaaa".to_string() } else { "bbbbbbb".to_string() })
    })
    .expect("write should succeed after retry");

    assert_eq!(result_a, "b_bbbbbbb", "retry should produce a 7-char ID");

    // --- Scenario B: both attempts collide → error ---
    conn.execute(
        "INSERT INTO handles \
         (id, tool, output_mode, params, cwd, content, line_count, created_at) \
         VALUES ('b_cccccc', 'Bash', 'stdout', '{}', '/', 'pre', 1, '0')",
        [],
    )
    .expect("pre-insert b_cccccc");
    conn.execute(
        "INSERT INTO handles \
         (id, tool, output_mode, params, cwd, content, line_count, created_at) \
         VALUES ('b_ddddddd', 'Bash', 'stdout', '{}', '/', 'pre', 1, '0')",
        [],
    )
    .expect("pre-insert b_ddddddd");

    let result_b = store::write_with_id_gen(&conn, payload.clone(), |len| {
        Ok(if len == 6 { "cccccc".to_string() } else { "ddddddd".to_string() })
    });

    let err = result_b.expect_err("should return Err when both attempts collide");
    let msg = err.to_string();
    assert!(
        msg.contains("handle ID collision after retry"),
        "error message should contain 'handle ID collision after retry', got: {msg:?}"
    );
}

// ---------------------------------------------------------------------------
// CtxlError Display
// ---------------------------------------------------------------------------

#[test]
fn ctxl_error_display_strings_match_expected_format() {
    let not_found = CtxlError::HandleNotFound("b_test".into());
    assert!(
        not_found.to_string().contains("handle not found: b_test"),
        "HandleNotFound display should contain 'handle not found: b_test', got: {:?}",
        not_found.to_string()
    );

    let collision = CtxlError::HandleCollision;
    assert!(
        collision.to_string().contains("handle ID collision after retry"),
        "HandleCollision display should contain 'handle ID collision after retry', got: {:?}",
        collision.to_string()
    );
}
