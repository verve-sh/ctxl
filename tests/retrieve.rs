#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used, clippy::panic)] // integration tests

use ctxl::{db, retrieve, store};
use rusqlite::Connection;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn in_memory_conn() -> Connection {
    let conn = Connection::open_in_memory().expect("open_in_memory");
    db::apply_schema(&conn).expect("apply_schema");
    conn
}

fn write_payload(conn: &Connection, content: &str) -> String {
    let payload = serde_json::json!({
        "tool": "Bash",
        "output_mode": "stdout",
        "cwd": "/",
        "content": content
    });
    store::write(conn, payload).expect("store::write")
}

// ---------------------------------------------------------------------------
// AC-1112-03
// ---------------------------------------------------------------------------

// @ac AC-1112-03
#[test]
fn ctxl_show_handle_bounds_output() {
    // Verify: Default invocation returns at most 80 lines.
    // Verify: `--head 200` returns at most 200 lines.
    // Verify: For a stored handle with 500 lines, default `show` returns
    //         exactly 80 newline-separated lines.

    let conn = in_memory_conn();

    // Build content with exactly 500 numbered lines (each ending with \n)
    let content: String = (1u32..=500).map(|i| format!("line {i}\n")).collect();
    let h = write_payload(&conn, &content);

    // --- default: 80 content lines + footer ---
    let result =
        retrieve::show(&conn, &h, retrieve::ShowOpts::default()).expect("show with default opts");
    let line_count = result.lines().count();
    // 80 display lines + 1 footer "(showing 80 of 500 lines)"
    assert_eq!(line_count, 81, "default show should return 80 lines + footer, got {line_count}");
    assert!(result.contains("(showing 80 of 500 lines)"), "footer should show total");

    // --- --head 200: 200 content lines + footer ---
    let result200 =
        retrieve::show(&conn, &h, retrieve::ShowOpts { head: 200, ..Default::default() })
            .expect("show head=200");
    let line_count200 = result200.lines().count();
    // 200 display lines + 1 footer
    assert_eq!(
        line_count200, 201,
        "--head 200 on a 500-line handle should return 200 lines + footer, got {line_count200}"
    );

    // --- head larger than content: returns all lines ---
    let short_content = "a\nb\nc\n";
    let h_short = write_payload(&conn, short_content);
    let result_short =
        retrieve::show(&conn, &h_short, retrieve::ShowOpts { head: 80, ..Default::default() })
            .expect("show short");
    assert_eq!(result_short.lines().count(), 3, "show on a 3-line handle should return 3 lines");
}

// ---------------------------------------------------------------------------
// AC-1112-04
// ---------------------------------------------------------------------------

// @ac AC-1112-04
#[test]
fn ctxl_search_handle_returns_bounded_fts5_results() {
    // Verify: For a stored handle whose content contains 100 lines each
    //         matching `useState`, the default limit returns at most 20 lines.
    // Verify: `--limit 5` returns at most 5 lines.
    // Verify: Each returned line contains the query token.

    let conn = in_memory_conn();

    // 100 lines, each containing "useState"
    let content: String = (1u32..=100)
        .map(|i| format!("  const [state{i}, setState{i}] = useState(null);\n"))
        .collect();
    let h = write_payload(&conn, &content);

    // --- default limit 20 ---
    let results = retrieve::search(&conn, &h, "useState", 20).expect("search default");
    assert!(
        results.len() <= 20,
        "default limit should return at most 20 results, got {}",
        results.len()
    );
    assert_eq!(results.len(), 20, "100 matching lines → should return exactly 20 with limit=20");
    for line in &results {
        assert!(line.contains("useState"), "each result must contain 'useState', got: {line:?}");
    }

    // --- limit 5 ---
    let results5 = retrieve::search(&conn, &h, "useState", 5).expect("search limit=5");
    assert!(
        results5.len() <= 5,
        "--limit 5 should return at most 5 results, got {}",
        results5.len()
    );
    assert_eq!(results5.len(), 5, "100 matching lines → exactly 5 with limit=5");
    for line in &results5 {
        assert!(line.contains("useState"), "each result must contain 'useState', got: {line:?}");
    }

    // --- no matches ---
    let no_results =
        retrieve::search(&conn, &h, "nonexistent_xyz_abc", 20).expect("search for absent token");
    assert!(no_results.is_empty(), "absent query should return empty results, got: {no_results:?}");
}

// ---------------------------------------------------------------------------
// Error message teaches usage
// ---------------------------------------------------------------------------

#[test]
fn unknown_handle_without_prefix_includes_usage_hint() {
    // When the handle ID doesn't start with b_, g_, or w_, the error
    // message should include a hint about correct handle format.
    let conn = in_memory_conn();
    let err =
        retrieve::show(&conn, "test", retrieve::ShowOpts::default()).expect_err("should fail");
    let msg = err.to_string();
    assert!(msg.contains("handle not found: test"), "base error present: {msg:?}");
    assert!(msg.contains("hint:"), "should include usage hint for non-handle ID: {msg:?}");
    assert!(msg.contains("b_"), "hint should mention handle prefixes: {msg:?}");
}

#[test]
fn unknown_handle_with_valid_prefix_no_hint() {
    // When the handle ID starts with a valid prefix, no hint needed —
    // the user knows the format, the handle just doesn't exist.
    let conn = in_memory_conn();
    let err = retrieve::show(&conn, "b_deadbeef", retrieve::ShowOpts::default())
        .expect_err("should fail");
    let msg = err.to_string();
    assert!(msg.contains("handle not found: b_deadbeef"), "base error: {msg:?}");
    assert!(!msg.contains("hint:"), "should NOT include hint for valid-prefix handle: {msg:?}");
}

// ---------------------------------------------------------------------------
// ctxl files tabular output
// ---------------------------------------------------------------------------

#[test]
fn files_grep_returns_tabular_format() {
    let conn = in_memory_conn();
    // Store grep-like content with multiple files
    let content = "src/lib.rs:1:fn main()\nsrc/lib.rs:2:}\nsrc/main.rs:1:use lib;\n";
    let payload = serde_json::json!({
        "tool": "Grep",
        "output_mode": "content",
        "content": content
    });
    let h = store::write(&conn, payload).expect("store::write");

    let rows = retrieve::files_grep(&conn, &h).expect("files_grep");
    // Should have header + 2 file rows + summary
    assert!(rows.len() >= 3, "expected header + file rows + summary, got: {rows:?}");
    assert!(rows[0].contains("Matches"), "first row should be header: {:?}", rows[0]);
    assert!(rows[1].contains("src/lib.rs"), "should contain lib.rs: {:?}", rows[1]);
    assert!(rows[1].contains("2"), "lib.rs should have 2 matches: {:?}", rows[1]);
    let last = rows.last().unwrap();
    assert!(last.contains("2 files"), "summary should mention file count: {last:?}");
    assert!(last.contains("3 total matches"), "summary should mention total: {last:?}");
}

// ---------------------------------------------------------------------------
// Underscore tokenization
// ---------------------------------------------------------------------------

#[test]
fn search_matches_underscore_identifiers() {
    // FTS5 with tokenchars="_" should treat `test_foo_bar` as a single token,
    // making it searchable as a phrase without splitting on underscores.
    let conn = in_memory_conn();
    let content = "fn test_foo_bar() { }\nfn other_func() { }\nfn test_foo_bar_baz() { }\n";
    let h = write_payload(&conn, content);

    let results = retrieve::search(&conn, &h, "test_foo_bar", 20).expect("search underscore id");
    assert!(!results.is_empty(), "should find matches for 'test_foo_bar'");
    // Should match the exact identifier and the longer one containing it
    assert!(
        results.iter().any(|r| r.contains("test_foo_bar()")),
        "should match exact identifier, got: {results:?}"
    );
}

// ---------------------------------------------------------------------------
// AC-1112-06
// ---------------------------------------------------------------------------

// @ac AC-1112-06
#[test]
fn ctxl_show_unknown_handle_surfaces_missing_handle() {
    // Verify: Running `ctxl show b_deadbeef` (handle not in DB) exits with
    //         status 1 and stderr containing "handle not found: b_deadbeef".

    // --- Library-level: verify error message ---
    let conn = in_memory_conn();
    let err = retrieve::show(&conn, "b_deadbeef", retrieve::ShowOpts::default())
        .expect_err("should fail");
    let msg = err.to_string();
    assert!(
        msg.contains("handle not found: b_deadbeef"),
        "error should contain 'handle not found: b_deadbeef', got: {msg:?}"
    );

    // --- Binary-level: verify exit code 1 and stderr content ---
    //
    // Creates a real (empty) session DB so the binary can open it and
    // proceed to the handle lookup step.
    let tmp = tempfile::tempdir().expect("tempdir");
    let session_id = "test-session-missing-handle";
    let db_dir = tmp.path().join("ctxl").join(session_id);
    std::fs::create_dir_all(&db_dir).expect("create session dir");

    let db_path = db_dir.join("store.db");
    let disk_conn = rusqlite::Connection::open(&db_path).expect("open db on disk");
    db::apply_schema(&disk_conn).expect("apply_schema on disk");
    drop(disk_conn);

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ctxl"))
        .args(["show", "b_deadbeef", "--session-id", session_id])
        .env("CTXL_CACHE_ROOT", tmp.path())
        .output()
        .expect("spawn ctxl");

    assert_eq!(
        output.status.code(),
        Some(1),
        "exit code should be 1, got {:?}",
        output.status.code()
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("handle not found: b_deadbeef"),
        "stderr should contain 'handle not found: b_deadbeef', got: {stderr:?}"
    );
}

// ---------------------------------------------------------------------------
// inspect
// ---------------------------------------------------------------------------

#[test]
fn inspect_returns_metadata_for_stored_handle() {
    let conn = in_memory_conn();
    let h = write_payload(&conn, "fn main() { println!(\"hello\"); }\n");

    let info = retrieve::inspect(&conn, &h).expect("inspect should succeed");
    assert_eq!(info.id, h);
    assert_eq!(info.tool, "Bash");
    assert_eq!(info.output_mode, "stdout");
    assert_eq!(info.line_count, Some(1));
    assert!(info.token_est.is_some());
    assert!(!info.truncated);
    assert!(info.compressed_method.is_none());
    assert!(info.created_at > 0);
    assert_eq!(info.retrieval_count, 0);
    assert!(info.last_retrieved_at.is_none());
}

#[test]
fn inspect_returns_handle_not_found_for_missing() {
    let conn = in_memory_conn();
    let err = retrieve::inspect(&conn, "b_nonexistent").expect_err("should fail");
    let msg = err.to_string();
    assert!(
        msg.contains("handle not found: b_nonexistent"),
        "error should mention missing handle: {msg:?}"
    );
}

#[test]
fn inspect_shows_truncated_for_truncated_handle() {
    let conn = in_memory_conn();
    let payload = serde_json::json!({
        "tool": "Grep",
        "output_mode": "content",
        "cwd": "/",
        "content": "src/lib.rs:1:fn main()",
        "truncated": true,
    });
    let h = store::write(&conn, payload).expect("store::write");

    let info = retrieve::inspect(&conn, &h).expect("inspect should succeed");
    assert!(info.truncated, "truncated must be true for handle stored with truncated=true");
}

// ---------------------------------------------------------------------------
// retrieval tracking
// ---------------------------------------------------------------------------

#[test]
fn retrieval_updates_handle_tracking_columns() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let session_id = "test-retrieval-tracking";
    let db_dir = tmp.path().join("ctxl").join(session_id);
    std::fs::create_dir_all(&db_dir).expect("create session dir");

    let db_path = db_dir.join("store.db");
    let conn = rusqlite::Connection::open(&db_path).expect("open db");
    db::apply_schema(&conn).expect("schema");
    let h = write_payload(&conn, "line 1\nline 2\nline 3\n");
    drop(conn);

    for _ in 0..2 {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_ctxl"))
            .args(["show", &h, "--session-id", session_id])
            .env("CTXL_CACHE_ROOT", tmp.path())
            .output()
            .expect("spawn ctxl");
        assert!(output.status.success(), "ctxl show should succeed");
    }

    let conn = rusqlite::Connection::open(&db_path).expect("reopen db");
    let (count, ts): (i64, Option<i64>) = conn
        .query_row(
            "SELECT retrieval_count, last_retrieved_at FROM handles WHERE id = ?1",
            [&h],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("query tracking columns");
    assert_eq!(count, 2, "retrieval_count should be 2 after two show calls");
    assert!(ts.is_some(), "last_retrieved_at should be set after retrieval");
}

// ---------------------------------------------------------------------------
// show --tail
// ---------------------------------------------------------------------------

#[test]
fn show_tail_returns_last_n_lines() {
    let conn = in_memory_conn();
    let content: String = (1u32..=100).map(|i| format!("line {i}\n")).collect();
    let h = write_payload(&conn, &content);

    let result = retrieve::show(
        &conn,
        &h,
        retrieve::ShowOpts { head: 80, tail: Some(10), offset: 0, compressed: false },
    )
    .expect("show with tail");

    let lines: Vec<&str> = result.lines().collect();
    // 10 content lines + 1 footer
    assert_eq!(lines.len(), 11, "tail 10 should return 10 lines + footer");
    assert!(lines[0].contains("line 91"), "first tail line should be 91, got: {}", lines[0]);
    assert!(lines[9].contains("line 100"), "last tail line should be 100, got: {}", lines[9]);
    assert!(lines[10].contains("(showing last 10 of 100 lines)"), "footer present");
}

#[test]
fn show_tail_returns_all_when_under_limit() {
    let conn = in_memory_conn();
    let h = write_payload(&conn, "a\nb\nc\n");

    let result = retrieve::show(
        &conn,
        &h,
        retrieve::ShowOpts { head: 80, tail: Some(10), offset: 0, compressed: false },
    )
    .expect("show with tail");

    assert_eq!(result.lines().count(), 3, "tail on 3-line content should return all 3");
}

// ---------------------------------------------------------------------------
// search_all — cross-handle FTS5 search
// ---------------------------------------------------------------------------

fn write_payload_with_tool(conn: &Connection, content: &str, tool: &str) -> String {
    let payload = serde_json::json!({
        "tool": tool,
        "output_mode": "stdout",
        "cwd": "/",
        "content": content
    });
    store::write(conn, payload).expect("store::write")
}

#[test]
fn search_all_finds_matches_across_handles() {
    let conn = in_memory_conn();
    write_payload_with_tool(&conn, "hello world content here\nsome other line\n", "Bash");
    write_payload_with_tool(&conn, "different content entirely\nno match here\n", "WebFetch");
    write_payload_with_tool(&conn, "third handle content block\nalso has content\n", "Grep");

    let results = retrieve::search_all(&conn, "content", 20).expect("search_all");
    assert!(
        results.len() >= 3,
        "should find 'content' across multiple handles, got {}",
        results.len()
    );

    let handle_ids: Vec<&str> = results.iter().map(|m| m.handle_id.as_str()).collect();
    let unique: std::collections::HashSet<&&str> = handle_ids.iter().collect();
    assert!(unique.len() >= 2, "should span at least 2 handles, got {} unique", unique.len());
}

#[test]
fn search_all_zero_match_returns_empty() {
    let conn = in_memory_conn();
    write_payload(&conn, "hello world\nfoo bar\n");

    let results =
        retrieve::search_all(&conn, "nonexistent_xyz_token", 20).expect("search_all no match");
    assert!(results.is_empty(), "should return empty for no matches");
}

#[test]
fn search_all_respects_limit() {
    let conn = in_memory_conn();
    // Insert multiple handles each with many matching lines.
    for i in 0..5 {
        let content: String = (0..20).map(|j| format!("matching_token line {i}-{j}\n")).collect();
        write_payload(&conn, &content);
    }

    let results = retrieve::search_all(&conn, "matching_token", 10).expect("search_all limited");
    assert!(results.len() <= 10, "should respect limit=10, got {}", results.len());
}

#[test]
fn search_all_format_groups_by_handle() {
    let conn = in_memory_conn();
    write_payload_with_tool(&conn, "hello content\n", "Bash");
    write_payload_with_tool(&conn, "world content\n", "WebFetch");

    let results = retrieve::search_all(&conn, "content", 20).expect("search_all");
    let formatted = retrieve::format_search_all(&results);
    assert!(formatted.contains("(Bash)"), "should show tool name: {formatted}");
    assert!(formatted.contains("(WebFetch)"), "should show tool name: {formatted}");
    assert!(formatted.contains("---"), "should have separators: {formatted}");
}

// ---------------------------------------------------------------------------
// calls --intercepted filter + ctxl last
// ---------------------------------------------------------------------------

fn insert_test_call(conn: &Connection, tool: &str, intercepted: bool, handle_id: Option<&str>) {
    let created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    conn.execute(
        "INSERT INTO calls (tool, intercepted, handle_id, line_count, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![tool, intercepted, handle_id, 10i64, created_at],
    )
    .unwrap();
}

#[test]
fn calls_intercepted_filter() {
    let conn = in_memory_conn();
    insert_test_call(&conn, "Bash", true, Some("b_aaa111"));
    insert_test_call(&conn, "Read", false, None);
    insert_test_call(&conn, "Grep", true, Some("g_bbb222"));
    insert_test_call(&conn, "Edit", false, None);

    let all =
        retrieve::calls(&conn, retrieve::CallsOpts { last: 100, tool: None, intercepted: None })
            .unwrap();
    assert_eq!(all.len(), 4, "all calls returned without filter");

    let intercepted = retrieve::calls(
        &conn,
        retrieve::CallsOpts { last: 100, tool: None, intercepted: Some(true) },
    )
    .unwrap();
    assert_eq!(intercepted.len(), 2, "only intercepted calls");
    for row in &intercepted {
        let v: serde_json::Value = serde_json::from_str(row).unwrap();
        assert_eq!(v["intercepted"], true);
    }
}

#[test]
fn calls_last_one_row() {
    let conn = in_memory_conn();
    insert_test_call(&conn, "Bash", true, Some("b_111111"));
    insert_test_call(&conn, "Bash", true, Some("b_222222"));
    insert_test_call(&conn, "Bash", true, Some("b_333333"));

    let rows =
        retrieve::calls(&conn, retrieve::CallsOpts { last: 1, tool: None, intercepted: None })
            .unwrap();
    assert_eq!(rows.len(), 1, "last: 1 should return exactly 1 row");
}

#[test]
fn last_intercepted_skips_recording() {
    let conn = in_memory_conn();
    insert_test_call(&conn, "Read", false, None);
    insert_test_call(&conn, "Bash", true, Some("b_aaa111"));
    insert_test_call(&conn, "Edit", false, None);

    let rows = retrieve::calls(
        &conn,
        retrieve::CallsOpts { last: 1, tool: None, intercepted: Some(true) },
    )
    .unwrap();
    assert_eq!(rows.len(), 1);
    let v: serde_json::Value = serde_json::from_str(&rows[0]).unwrap();
    assert_eq!(v["intercepted"], true);
    assert_eq!(v["handle_id"], "b_aaa111");
}

// ---------------------------------------------------------------------------
// FTS5 highlight search (#1483)
// ---------------------------------------------------------------------------

#[test]
fn search_highlight_preserves_clean_output() {
    // Verify no \x01/\x02 sentinel markers leak into returned results
    let conn = in_memory_conn();
    let content: String =
        (1u32..=20).map(|i| format!("const value{i} = useState(null);\n")).collect();
    let h = write_payload(&conn, &content);

    let results = retrieve::search(&conn, &h, "useState", 20).expect("search");
    for line in &results {
        assert!(
            !line.contains('\x01') && !line.contains('\x02'),
            "result must not contain sentinel markers: {line:?}"
        );
        assert!(line.contains("useState"), "each result should match query: {line:?}");
    }
}

#[test]
fn search_highlight_respects_limit() {
    let conn = in_memory_conn();
    let content: String =
        (1u32..=100).map(|i| format!("const item{i} = useState(null);\n")).collect();
    let h = write_payload(&conn, &content);

    let results = retrieve::search(&conn, &h, "useState", 5).expect("search");
    assert!(results.len() <= 5, "limit=5 should return at most 5, got {}", results.len());
}

#[test]
fn search_fallback_on_partial_token() {
    // FTS5 treats `test_foo_bar` as a single token (tokenchars includes _).
    // Searching for just "foo" should fail the FTS5 highlight tier but
    // succeed via the substring fallback.
    let conn = in_memory_conn();
    let content = "fn test_foo_bar() { }\nfn unrelated() { }\n";
    let h = write_payload(&conn, content);

    let results = retrieve::search(&conn, &h, "foo", 20).expect("search partial token");
    assert!(
        !results.is_empty(),
        "substring fallback should catch partial-token match 'foo' in 'test_foo_bar'"
    );
    assert!(
        results.iter().any(|r| r.contains("test_foo_bar")),
        "should find the line containing test_foo_bar: {results:?}"
    );
}

#[test]
fn search_all_highlight_cross_handle() {
    // Verify cross-handle search uses highlight path and returns clean output
    let conn = in_memory_conn();
    write_payload_with_tool(&conn, "alpha content here\nno match\n", "Bash");
    write_payload_with_tool(&conn, "beta content present\nno match\n", "Grep");

    let results = retrieve::search_all(&conn, "content", 20).expect("search_all");
    assert!(results.len() >= 2, "should find 'content' across handles, got {}", results.len());
    for m in &results {
        assert!(
            !m.line.contains('\x01') && !m.line.contains('\x02'),
            "cross-handle result must not contain sentinel markers: {:?}",
            m.line
        );
        assert!(m.line.contains("content"), "each result should match query: {:?}", m.line);
    }
}

#[test]
fn search_all_fallback_for_partial_token() {
    // search_all now falls back to substring scanning (matching per-handle search).
    // A partial-token query that fails FTS5 should succeed via substring fallback
    // on the most recent 10 handles.
    let conn = in_memory_conn();
    // "test_foo_bar" is a single FTS5 token (underscore joins);
    // searching for just "foo" won't match via FTS5 but should hit substring fallback.
    write_payload_with_tool(&conn, "fn test_foo_bar() { }\nfn unrelated() { }\n", "Bash");

    let results = retrieve::search_all(&conn, "foo", 20).expect("search_all partial token");
    assert!(
        !results.is_empty(),
        "search_all should find partial-token match via substring fallback, got 0 results"
    );
    assert!(
        results.iter().any(|r| r.line.contains("test_foo_bar")),
        "should find the line containing test_foo_bar: {results:?}"
    );
}

// ---------------------------------------------------------------------------
// Search named flags + reversed positional detection (#1466)
// ---------------------------------------------------------------------------

#[test]
fn search_named_flags_order_independent() {
    // --query "X" --handle b_xxx should work same as positional
    let tmp = tempfile::tempdir().expect("tempdir");
    let session_id = "test-named-flags";
    let db_dir = tmp.path().join("ctxl").join(session_id);
    std::fs::create_dir_all(&db_dir).expect("create session dir");

    let db_path = db_dir.join("store.db");
    let conn = rusqlite::Connection::open(&db_path).expect("open db");
    db::apply_schema(&conn).expect("schema");
    let h = write_payload(&conn, "hello world content here\nother line\n");
    drop(conn);

    // Named flags: --query first, --handle second (reversed from positional order)
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ctxl"))
        .args(["search", "--query", "content", "--handle", &h, "--session-id", session_id])
        .env("CTXL_CACHE_ROOT", tmp.path())
        .output()
        .expect("spawn ctxl");

    assert!(
        output.status.success(),
        "named flags should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("content"), "should find 'content' via named flags: {stdout}");
}

#[test]
fn search_reversed_positional_errors() {
    // query-first positional should produce exit 1 with "reversed" in stderr
    let tmp = tempfile::tempdir().expect("tempdir");
    let session_id = "test-reversed-pos";
    let db_dir = tmp.path().join("ctxl").join(session_id);
    std::fs::create_dir_all(&db_dir).expect("create session dir");

    let db_path = db_dir.join("store.db");
    let conn = rusqlite::Connection::open(&db_path).expect("open db");
    db::apply_schema(&conn).expect("schema");
    drop(conn);

    // Reversed: "some query" first, then a handle-like ID
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ctxl"))
        .args(["search", "some query", "b_1a2b3c4d", "--session-id", session_id])
        .env("CTXL_CACHE_ROOT", tmp.path())
        .output()
        .expect("spawn ctxl");

    assert_eq!(
        output.status.code(),
        Some(1),
        "reversed positionals should exit 1, got {:?}",
        output.status.code()
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("reversed"), "stderr should mention 'reversed': {stderr}");
}

// ---------------------------------------------------------------------------
// 1a: show --offset
// ---------------------------------------------------------------------------

#[test]
fn test_show_offset() {
    let conn = in_memory_conn();
    let content: String = (1u32..=20).map(|i| format!("line {i}\n")).collect();
    let h = write_payload(&conn, &content);

    let result = retrieve::show(
        &conn,
        &h,
        retrieve::ShowOpts { head: 10, tail: None, offset: 5, compressed: false },
    )
    .expect("show with offset");

    let lines: Vec<&str> = result.lines().collect();
    assert_eq!(lines.len(), 10, "offset=5, head=10 on 20 lines should return 10 lines");
    assert!(
        lines[0].contains("line 6"),
        "first line after offset=5 should be line 6, got: {}",
        lines[0]
    );
    assert!(lines[9].contains("line 15"), "last line should be line 15, got: {}", lines[9]);
}

#[test]
fn test_show_offset_beyond_content() {
    let conn = in_memory_conn();
    let h = write_payload(&conn, "a\nb\nc\n");

    let result = retrieve::show(
        &conn,
        &h,
        retrieve::ShowOpts { head: 80, tail: None, offset: 100, compressed: false },
    )
    .expect("show with offset beyond content");

    assert!(result.is_empty(), "offset beyond content should return empty, got: {result:?}");
}

// ---------------------------------------------------------------------------
// 1b: search rejects regex metacharacters
// ---------------------------------------------------------------------------

#[test]
fn test_search_rejects_regex() {
    let conn = in_memory_conn();
    let h = write_payload(&conn, "some content here\n");

    let err =
        retrieve::search(&conn, &h, "listen\\|unlisten", 20).expect_err("should reject regex");
    let msg = err.to_string();
    assert!(msg.contains("regex syntax"), "error should mention regex syntax: {msg:?}");
}

#[test]
fn test_search_regex_error_message() {
    let conn = in_memory_conn();
    let h = write_payload(&conn, "some content here\n");

    let err = retrieve::search(&conn, &h, "foo\\(bar\\)", 20).expect_err("should reject regex");
    let msg = err.to_string();
    assert!(msg.contains("regex syntax"), "error should mention regex syntax: {msg:?}");
    // Should include suggestion
    assert!(msg.contains("FTS5 does not support"), "error should mention FTS5: {msg:?}");
}

// ---------------------------------------------------------------------------
// 1c: files command with diff content
// ---------------------------------------------------------------------------

#[test]
fn test_files_diff_handle() {
    let conn = in_memory_conn();
    let diff_content = concat!(
        "diff --git a/src/main.rs b/src/main.rs\n",
        "index abc..def 100644\n",
        "--- a/src/main.rs\n",
        "+++ b/src/main.rs\n",
        "@@ -1,3 +1,4 @@\n",
        " fn main() {\n",
        "+    println!(\"hello\");\n",
        " }\n",
        "@@ -10,3 +11,4 @@\n",
        " fn other() {\n",
        "+    println!(\"world\");\n",
        " }\n",
        "diff --git a/src/lib.rs b/src/lib.rs\n",
        "index ghi..jkl 100644\n",
        "--- a/src/lib.rs\n",
        "+++ b/src/lib.rs\n",
        "@@ -5,3 +5,4 @@\n",
        " pub fn greet() {\n",
        "+    println!(\"hi\");\n",
        " }\n",
    );
    let h = write_payload(&conn, diff_content);

    let rows = retrieve::files(&conn, &h).expect("files on diff handle");
    assert!(rows.len() >= 3, "expected header + file rows + summary, got: {rows:?}");
    assert!(rows[0].contains("Hunks"), "header should say Hunks, got: {:?}", rows[0]);
    // main.rs has 2 hunks, lib.rs has 1
    let main_row = rows.iter().find(|r| r.contains("src/main.rs"));
    assert!(main_row.is_some(), "should contain main.rs: {rows:?}");
    assert!(main_row.unwrap().contains("2"), "main.rs should have 2 hunks: {:?}", main_row);
    let lib_row = rows.iter().find(|r| r.contains("src/lib.rs"));
    assert!(lib_row.is_some(), "should contain lib.rs: {rows:?}");
    assert!(lib_row.unwrap().contains("1"), "lib.rs should have 1 hunk: {:?}", lib_row);
}

#[test]
fn test_files_grep_still_works() {
    let conn = in_memory_conn();
    let content = "src/lib.rs:1:fn main()\nsrc/lib.rs:2:}\nsrc/main.rs:1:use lib;\n";
    let payload = serde_json::json!({
        "tool": "Grep",
        "output_mode": "content",
        "content": content
    });
    let h = store::write(&conn, payload).expect("store::write");

    let rows = retrieve::files(&conn, &h).expect("files on grep handle");
    assert!(rows.len() >= 3, "expected header + file rows + summary, got: {rows:?}");
    assert!(rows[0].contains("Matches"), "header should say Matches for grep: {:?}", rows[0]);
    assert!(rows.iter().any(|r| r.contains("src/lib.rs")), "should contain lib.rs: {rows:?}");
}

// ---------------------------------------------------------------------------
// 1d: show --file and --glob on diff content
// ---------------------------------------------------------------------------

#[test]
fn test_show_file_diff() {
    let conn = in_memory_conn();
    let diff_content = concat!(
        "diff --git a/src/main.rs b/src/main.rs\n",
        "--- a/src/main.rs\n",
        "+++ b/src/main.rs\n",
        "@@ -1,3 +1,4 @@\n",
        " fn main() {\n",
        "+    println!(\"hello\");\n",
        " }\n",
        "diff --git a/src/lib.rs b/src/lib.rs\n",
        "--- a/src/lib.rs\n",
        "+++ b/src/lib.rs\n",
        "@@ -5,3 +5,4 @@\n",
        " pub fn greet() {\n",
        "+    println!(\"hi\");\n",
        " }\n",
    );
    let h = write_payload(&conn, diff_content);

    let result = retrieve::show_filtered(&conn, &h, Some("src/main.rs"), None, &[], 100, 0)
        .expect("show --file on diff");

    assert!(result.contains("src/main.rs"), "should contain main.rs content: {result:?}");
    assert!(!result.contains("src/lib.rs"), "should NOT contain lib.rs content: {result:?}");
    assert!(result.contains("fn main()"), "should contain main.rs hunks: {result:?}");
}

#[test]
fn test_show_glob_diff() {
    let conn = in_memory_conn();
    let diff_content = concat!(
        "diff --git a/src/main.rs b/src/main.rs\n",
        "--- a/src/main.rs\n",
        "+++ b/src/main.rs\n",
        "@@ -1,3 +1,4 @@\n",
        "+    let x = 1;\n",
        "diff --git a/config.json b/config.json\n",
        "--- a/config.json\n",
        "+++ b/config.json\n",
        "@@ -1,2 +1,3 @@\n",
        "+    \"key\": \"value\"\n",
    );
    let h = write_payload(&conn, diff_content);

    let result = retrieve::show_filtered(&conn, &h, None, Some("*.rs"), &[], 100, 0)
        .expect("show --glob on diff");

    assert!(result.contains("main.rs"), "should contain .rs files: {result:?}");
    assert!(!result.contains("config.json"), "should NOT contain .json files: {result:?}");
}

// ---------------------------------------------------------------------------
// 1e: show --exclude
// ---------------------------------------------------------------------------

#[test]
fn test_show_exclude_diff() {
    let conn = in_memory_conn();
    let diff_content = concat!(
        "diff --git a/src/main.rs b/src/main.rs\n",
        "--- a/src/main.rs\n",
        "+++ b/src/main.rs\n",
        "@@ -1,3 +1,4 @@\n",
        "+    let x = 1;\n",
        "diff --git a/config.json b/config.json\n",
        "--- a/config.json\n",
        "+++ b/config.json\n",
        "@@ -1,2 +1,3 @@\n",
        "+    \"key\": \"value\"\n",
    );
    let h = write_payload(&conn, diff_content);

    let result = retrieve::show_filtered(&conn, &h, None, None, &["*.json".to_string()], 100, 0)
        .expect("show --exclude on diff");

    assert!(result.contains("main.rs"), "should contain non-excluded files: {result:?}");
    assert!(!result.contains("config.json"), "should exclude .json files: {result:?}");
}

#[test]
fn test_show_exclude_grep() {
    let conn = in_memory_conn();
    let content = "src/lib.rs:1:fn main()\ntests/test_lib.rs:1:fn test()\nsrc/main.rs:1:use lib;\n";
    let h = write_payload(&conn, content);

    let result = retrieve::show_filtered(&conn, &h, None, None, &["tests/*".to_string()], 100, 0)
        .expect("show --exclude on grep");

    assert!(result.contains("src/lib.rs"), "should contain non-excluded files: {result:?}");
    assert!(result.contains("src/main.rs"), "should contain non-excluded files: {result:?}");
    assert!(!result.contains("tests/"), "should exclude tests/ files: {result:?}");
}

// ---------------------------------------------------------------------------
// search_all rejects regex metacharacters
// ---------------------------------------------------------------------------

#[test]
fn search_all_rejects_regex_metacharacters() {
    let conn = in_memory_conn();
    write_payload(&conn, "some content here\n");

    let err = retrieve::search_all(&conn, "listen\\|unlisten", 20)
        .expect_err("search_all should reject regex syntax");
    let msg = err.to_string();
    assert!(msg.contains("regex syntax"), "error should mention regex syntax: {msg:?}");
}

// ---------------------------------------------------------------------------
// show_filtered offset on filtered content
// ---------------------------------------------------------------------------

#[test]
fn show_filtered_offset_on_glob_filtered_content() {
    // Exercise the offset path inside show_filtered's filtered branch
    // (distinct from the no-filter early-return path tested by test_show_offset).
    let conn = in_memory_conn();
    let content = "\
src/alpha.rs:1:fn alpha()\n\
src/alpha.rs:2:fn alpha2()\n\
src/beta.rs:1:fn beta()\n\
src/beta.rs:2:fn beta2()\n\
src/beta.rs:3:fn beta3()\n\
lib/gamma.ts:1:export const gamma\n\
lib/gamma.ts:2:export const gamma2\n";
    let h = write_payload(&conn, content);

    // Glob *.rs filters to 5 lines (alpha x2 + beta x3), then offset=2 skips first 2
    let result = retrieve::show_filtered(&conn, &h, None, Some("*.rs"), &[], 100, 2)
        .expect("show_filtered with glob + offset");

    let lines: Vec<&str> = result.lines().collect();
    // After filtering to *.rs (5 lines) and skipping 2, we expect 3 lines remaining
    assert_eq!(lines.len(), 3, "glob *.rs (5 lines) minus offset 2 = 3 lines, got {}", lines.len());
    assert!(
        lines[0].contains("beta()"),
        "first post-offset line should be beta(), got: {}",
        lines[0]
    );
    assert!(
        !lines.iter().any(|l| l.contains("gamma")),
        "should not contain .ts files after glob filter"
    );
}

// ---------------------------------------------------------------------------
// #2126 — FTS5 prefix search support
// ---------------------------------------------------------------------------

#[test]
fn search_prefix_matches_partial_tokens() {
    // `grep*` should match `grep_dedup` via FTS5 prefix syntax ("grep"*).
    let conn = in_memory_conn();
    let content = "fn grep_dedup() { }\nfn compress() { }\nfn grep_preview() { }\n";
    let h = write_payload(&conn, content);

    let results = retrieve::search(&conn, &h, "grep*", 20).expect("search prefix");
    assert!(
        results.len() >= 2,
        "prefix 'grep*' should match grep_dedup and grep_preview, got {}: {results:?}",
        results.len()
    );
    assert!(
        results.iter().any(|r| r.contains("grep_dedup")),
        "should match grep_dedup: {results:?}"
    );
    assert!(
        results.iter().any(|r| r.contains("grep_preview")),
        "should match grep_preview: {results:?}"
    );
    // Should NOT match "compress" (no grep prefix).
    assert!(
        !results.iter().any(|r| r.contains("compress")),
        "should not match compress: {results:?}"
    );
}

#[test]
fn search_prefix_without_star_is_exact() {
    // Without `*`, "grep" should NOT match "grep_dedup" via FTS5
    // (FTS5 treats grep_dedup as a single token due to tokenchars="_").
    // But it WILL match via substring fallback.
    let conn = in_memory_conn();
    let content = "fn grep_dedup() { }\nfn compress() { }\n";
    let h = write_payload(&conn, content);

    let results = retrieve::search(&conn, &h, "grep", 20).expect("search exact");
    // Should find via substring fallback (grep is substring of grep_dedup).
    assert!(!results.is_empty(), "should find 'grep' in 'grep_dedup' via substring fallback");
}

#[test]
fn search_all_prefix_matches() {
    // Verify prefix search works across handles via search_all.
    let conn = in_memory_conn();
    write_payload_with_tool(&conn, "fn grep_dedup() { }\nfn other() { }\n", "Bash");
    write_payload_with_tool(&conn, "fn grep_preview() { }\nfn unrelated() { }\n", "Grep");

    let results = retrieve::search_all(&conn, "grep*", 20).expect("search_all prefix");
    assert!(
        results.len() >= 2,
        "prefix 'grep*' should match across handles, got {}: {results:?}",
        results.len()
    );
}

// ---------------------------------------------------------------------------
// #2126 — search_all substring fallback
// ---------------------------------------------------------------------------

#[test]
fn search_all_substring_fallback_finds_partial_match() {
    // search_all should fall back to substring matching when FTS5 returns no results.
    let conn = in_memory_conn();
    write_payload_with_tool(&conn, "fn test_foo_bar() { }\nfn unrelated() { }\n", "Bash");
    write_payload_with_tool(&conn, "fn another_foo_thing() { }\nfn no_match() { }\n", "Grep");

    let results = retrieve::search_all(&conn, "foo", 20).expect("search_all substring fallback");
    assert!(
        results.len() >= 2,
        "substring fallback should find 'foo' across handles, got {}: {results:?}",
        results.len()
    );
    assert!(
        results.iter().any(|r| r.line.contains("test_foo_bar")),
        "should find test_foo_bar: {results:?}"
    );
    assert!(
        results.iter().any(|r| r.line.contains("another_foo_thing")),
        "should find another_foo_thing: {results:?}"
    );
}
