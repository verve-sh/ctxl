#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ctxl::{
    compress::grep_preview::{grep_preview_content, grep_preview_count},
    db,
    intercept_grep::{self, GrepInterceptConfig, GrepToolResponse},
    payload::PostToolUsePayload,
    retrieve,
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

/// Build a typed Grep PostToolUse payload.
fn make_grep_payload(
    content: &str,
    mode: &str,
    num_files: u64,
    filenames: &[&str],
    num_matches: u64,
) -> PostToolUsePayload<GrepToolResponse> {
    PostToolUsePayload {
        tool_name: Some("Grep".into()),
        tool_response: GrepToolResponse {
            content: Some(content.into()),
            num_files: Some(num_files),
            filenames: Some(filenames.iter().map(|s| (*s).to_string()).collect()),
            num_matches: Some(num_matches),
            mode: Some(mode.into()),
        },
    }
}

/// Generate N lines of `file:line:text` content-mode grep output.
fn make_content_lines(n: usize) -> String {
    (0..n)
        .map(|i| {
            let file = if i % 3 == 0 {
                "src/lib.rs"
            } else if i % 3 == 1 {
                "src/main.rs"
            } else {
                "src/utils.rs"
            };
            format!("{file}:{}:fn match_{i}()", i + 1)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Run the Grep intercept with the given payload and threshold.
/// Returns the raw stdout bytes written by `run`.
fn run_grep_intercept(payload: PostToolUsePayload<GrepToolResponse>, threshold: usize) -> Vec<u8> {
    let conn = in_memory_conn();
    run_grep_intercept_with_conn(payload, threshold, &conn)
}

fn run_grep_intercept_with_conn(
    payload: PostToolUsePayload<GrepToolResponse>,
    threshold: usize,
    conn: &Connection,
) -> Vec<u8> {
    let config = GrepInterceptConfig {
        threshold,
        record: true,
        tool_input: None,
        cwd: None,
        tool_use_id: None,
    };
    let mut output = Vec::new();
    intercept_grep::run(payload, &mut output, &config, conn)
        .expect("intercept_grep::run should not fail");
    output
}

/// Extract the `g_XXXXXX` handle from a string.
fn find_grep_handle(s: &str) -> Option<String> {
    let pos = s.find("g_")?;
    let hex = &s[pos + 2..];
    let hex_len = hex.chars().take_while(|c| c.is_ascii_hexdigit()).count();
    if hex_len >= 6 {
        Some(s[pos..pos + 2 + hex_len].to_string())
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// AC-1371-01 — Grep intercept stores and replaces large output
// ---------------------------------------------------------------------------

// @ac AC-1371-01
#[test]
fn grep_intercept_stores_and_replaces_large_output() {
    // Verify: Given PostToolUse JSON with tool_name=Grep, `tool_response.mode="content"`, and line count > threshold (default 200 lines)
    // Verify: `ctxl intercept --session-id <uuid>` stores FULL content (no truncation at storage time)
    // Verify: emits valid `hookSpecificOutput.updatedToolOutput` JSON matching GrepOutput shape
    // Verify: (`content`, `numFiles`, `filenames`, `numMatches`, `mode`) with handle ID `g_XXXXXX` in replacement content
    // Verify: Also inserts a `calls` row with `intercepted=true` and `handle_id`
    let content = make_content_lines(300); // 300 lines > 200 threshold
    let filenames = &["src/lib.rs", "src/main.rs", "src/utils.rs"];
    let payload = make_grep_payload(&content, "content", 3, filenames, 300);

    let conn = in_memory_conn();
    let out = run_grep_intercept_with_conn(payload, 200, &conn);

    // Must produce output (not passthrough).
    assert!(!out.is_empty(), "should produce hookSpecificOutput for above-threshold input");

    let json: serde_json::Value = serde_json::from_slice(&out).expect("output must be valid JSON");
    let updated = &json["hookSpecificOutput"]["updatedToolOutput"];

    // Handle ID in content field.
    let content_field = updated["content"].as_str().expect("content must be a string");
    let handle_id = find_grep_handle(content_field).expect("content must contain g_XXXXXX handle");
    assert!(handle_id.starts_with("g_"), "handle must start with g_");

    // GrepOutput shape preserved.
    assert_eq!(updated["numFiles"], serde_json::json!(3));
    assert_eq!(updated["numMatches"], serde_json::json!(300));
    assert_eq!(updated["mode"], serde_json::json!("content"));
    let returned_filenames = updated["filenames"].as_array().expect("filenames must be array");
    assert_eq!(returned_filenames.len(), 3);

    // Stored content should be ALL 300 lines (full content, no truncation).
    let stored: String = conn
        .query_row("SELECT content FROM handles WHERE id=?1", [&handle_id], |row| row.get(0))
        .expect("handle row must exist");
    let stored_lines = stored.lines().count();
    assert_eq!(stored_lines, 300, "stored content must contain all lines (no truncation)");

    // Calls row must exist with intercepted=true and handle_id.
    let (intercepted, db_handle): (bool, String) = conn
        .query_row(
            "SELECT intercepted, handle_id FROM calls WHERE handle_id=?1",
            [&handle_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("calls row must be inserted");
    assert!(intercepted, "calls row must have intercepted=true");
    assert_eq!(db_handle, handle_id, "calls row handle_id must match");
}

// ---------------------------------------------------------------------------
// AC-1371-02 — Mode-aware preview for files_with_matches
// ---------------------------------------------------------------------------

// @ac AC-1371-02
#[test]
fn grep_preview_files_with_matches_mode_includes_file_manifest() {
    // Verify: Given Grep output in `files_with_matches` mode (`rg -l`), block message includes a file manifest (first N file paths)
    // Verify: No representative match lines — `files_with_matches` returns paths only
    // Verify: File count and total file list available via `ctxl files`

    // Build a large files_with_matches payload (300 file paths > 200 threshold).
    let file_paths: Vec<String> = (0..300).map(|i| format!("src/module_{i}.rs")).collect();
    let content = file_paths.join("\n");
    let payload = make_grep_payload(
        &content,
        "files_with_matches",
        300,
        &file_paths.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        0,
    );

    let conn = in_memory_conn();
    let out = run_grep_intercept_with_conn(payload, 200, &conn);
    assert!(!out.is_empty(), "should intercept above-threshold files_with_matches output");

    let json: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON");
    let block_msg = json["hookSpecificOutput"]["updatedToolOutput"]["content"]
        .as_str()
        .expect("content must be string");
    let handle_id = find_grep_handle(block_msg).expect("must have handle");

    // Block message must contain a file manifest with file paths.
    assert!(block_msg.contains("src/module_0.rs"), "file manifest must include first file");
    assert!(block_msg.contains("files with matches"), "must mention file count");
    // No match-line format (file:line:text) — only paths.
    assert!(!block_msg.contains(":1:"), "files_with_matches must not include match lines");

    // ctxl files returns tabular output with header, file rows, and summary.
    let files = retrieve::files_grep(&conn, &handle_id).expect("files_grep should work");
    assert!(!files.is_empty(), "files_grep must return rows");
    assert!(files[0].contains("Matches"), "first row must be table header");
    // At least one file row should be present.
    assert!(
        files.iter().any(|r| r.contains("src/module_")),
        "must include file paths in table rows"
    );
}

// ---------------------------------------------------------------------------
// AC-1371-03 — Handle retrieval supports Grep file flags
// ---------------------------------------------------------------------------

// @ac AC-1371-03
#[test]
fn grep_handle_retrieval_supports_file_flags() {
    // Verify: Given stored Grep handle `g_XXXXXX`, `ctxl show g_XXXXXX --file "src/lib.rs"` returns only matches from that file
    // Verify: `ctxl show g_XXXXXX --glob "*.rs"` returns matches from all `.rs` files (filtering applied before `--head` truncation)
    // Verify: `ctxl files g_XXXXXX` returns all files with match counts

    let content = make_content_lines(300); // lib.rs (0,3,6,...), main.rs (1,4,7,...), utils.rs (2,5,8,...)
    let payload = make_grep_payload(
        &content,
        "content",
        3,
        &["src/lib.rs", "src/main.rs", "src/utils.rs"],
        300,
    );

    let conn = in_memory_conn();
    let out = run_grep_intercept_with_conn(payload, 200, &conn);
    assert!(!out.is_empty());

    let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
    let handle_id = find_grep_handle(
        json["hookSpecificOutput"]["updatedToolOutput"]["content"].as_str().unwrap(),
    )
    .unwrap();

    // --file filter: only lib.rs lines.
    let lib_lines = retrieve::show_grep_file(&conn, &handle_id, Some("src/lib.rs"), None, 1000)
        .expect("show_grep_file --file");
    assert!(!lib_lines.is_empty(), "should return lib.rs matches");
    for line in lib_lines.lines() {
        assert!(line.starts_with("src/lib.rs"), "every line must be from lib.rs, got: {line}");
    }
    // main.rs must not appear.
    assert!(!lib_lines.contains("src/main.rs"), "--file should exclude other files");

    // --glob *.rs: all .rs files (all in this case).
    let rs_lines = retrieve::show_grep_file(&conn, &handle_id, None, Some("*.rs"), 1000)
        .expect("show_grep_file --glob");
    assert!(!rs_lines.is_empty(), "glob *.rs should return matches");
    // The stored content has all 300 lines (full content); all are *.rs — should return all.
    let rs_count = rs_lines.lines().count();
    assert_eq!(rs_count, 300, "glob *.rs should match all stored lines");

    // ctxl files: returns tabular per-file counts.
    let files = retrieve::files_grep(&conn, &handle_id).expect("files_grep");
    assert!(!files.is_empty(), "files_grep must return rows");
    assert!(files[0].contains("Matches"), "first row must be table header");
    // Check all three files appear in the tabular output.
    let joined = files.join("\n");
    assert!(joined.contains("src/lib.rs"), "files must include lib.rs");
    assert!(joined.contains("src/main.rs"), "files must include main.rs");
    assert!(joined.contains("src/utils.rs"), "files must include utils.rs");
}

// ---------------------------------------------------------------------------
// AC-1371-04 — GrepOutput shape preserved in updatedToolOutput
// ---------------------------------------------------------------------------

// @ac AC-1371-04
#[test]
fn grep_output_shape_preserved_in_updated_tool_output() {
    // Verify: `updatedToolOutput` includes `content` (replacement block message)
    // Verify: `numFiles`, `filenames`, `numMatches`, and `mode` fields passed through from the original Grep tool_response
    // Verify: matching native GrepOutput shape exactly
    let content = make_content_lines(250);
    let filenames = &["src/alpha.rs", "src/beta.rs"];
    let payload = make_grep_payload(&content, "content", 2, filenames, 250);

    let out = run_grep_intercept(payload, 200);
    assert!(!out.is_empty());

    let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
    let updated = &json["hookSpecificOutput"]["updatedToolOutput"];

    // All GrepOutput fields must be present.
    assert!(updated["content"].is_string(), "content must be string");
    assert!(updated["numFiles"].is_number(), "numFiles must be number");
    assert!(updated["filenames"].is_array(), "filenames must be array");
    assert!(updated["numMatches"].is_number(), "numMatches must be number");
    assert!(updated["mode"].is_string(), "mode must be string");

    // Passthrough values must match original.
    assert_eq!(updated["numFiles"], serde_json::json!(2));
    assert_eq!(updated["numMatches"], serde_json::json!(250));
    assert_eq!(updated["mode"], serde_json::json!("content"));
    let names: Vec<&str> =
        updated["filenames"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(names, vec!["src/alpha.rs", "src/beta.rs"]);

    // content field must reference a g_ handle.
    let content_str = updated["content"].as_str().unwrap();
    assert!(find_grep_handle(content_str).is_some(), "content must embed g_XXXXXX handle");
}

// ---------------------------------------------------------------------------
// AC-1371-05 — Below-threshold Grep passthrough
// ---------------------------------------------------------------------------

// @ac AC-1371-05
#[test]
fn grep_below_threshold_passthrough() {
    // Verify: Given PostToolUse JSON with tool_name=Grep and line count <= threshold (default 200 lines)
    // Verify: `ctxl intercept` exits 0 with empty stdout (no modification)

    // Exactly at threshold.
    let content_at = make_content_lines(200);
    let payload_at = make_grep_payload(&content_at, "content", 3, &["src/lib.rs"], 200);
    let out_at = run_grep_intercept(payload_at, 200);
    assert!(out_at.is_empty(), "at-threshold should passthrough (empty output)");

    // Below threshold.
    let content_below = make_content_lines(50);
    let payload_below = make_grep_payload(&content_below, "content", 1, &["src/lib.rs"], 50);
    let out_below = run_grep_intercept(payload_below, 200);
    assert!(out_below.is_empty(), "below-threshold should passthrough (empty output)");

    // Empty content.
    let payload_empty = make_grep_payload("", "content", 0, &[], 0);
    let out_empty = run_grep_intercept(payload_empty, 200);
    assert!(out_empty.is_empty(), "empty content should passthrough");
}

// ---------------------------------------------------------------------------
// AC-1371-06 — Unknown mode fallback
// ---------------------------------------------------------------------------

// @ac AC-1371-06
#[test]
fn grep_unknown_mode_fallback() {
    // Verify: Given PostToolUse JSON with an unrecognized `tool_response.mode` value
    // Verify: `ctxl intercept` falls back to passthrough compression (head/tail preview) instead of crashing
    // Verify: Exit 0 with valid output
    let content = make_content_lines(250);
    let payload = make_grep_payload(&content, "future_rg_mode", 3, &["src/lib.rs"], 250);

    // Should not panic; run returns Ok.
    let out = run_grep_intercept(payload, 200);
    assert!(!out.is_empty(), "unknown mode should still produce output above threshold");

    // Output must be valid JSON.
    let json: serde_json::Value = serde_json::from_slice(&out).expect("must be valid JSON");
    let updated = &json["hookSpecificOutput"]["updatedToolOutput"];
    assert!(updated["content"].is_string(), "content must be present");
    let content_str = updated["content"].as_str().unwrap();

    // Falls back to passthrough: must contain "unknown mode" marker and handle ID.
    assert!(content_str.contains("unknown mode"), "fallback block must mention unknown mode");
    assert!(find_grep_handle(content_str).is_some(), "must include handle ID");
}

// ---------------------------------------------------------------------------
// AC-1371-07 — Mode-aware preview for content mode
// ---------------------------------------------------------------------------

// @ac AC-1371-07
#[test]
fn grep_preview_content_mode_head_tail_and_file_manifest() {
    // Verify: Given Grep output in `content` mode (default), block message includes a head/tail preview of matching lines
    // Verify: Total match count included in preview
    // Verify: File manifest includes files sorted by match count

    // Build content where lib.rs has 150 hits, main.rs has 50 hits, utils.rs has 50 hits.
    let mut lines: Vec<String> =
        (0..150).map(|i| format!("src/lib.rs:{}:match_lib_{i}", i + 1)).collect();
    lines.extend((0..50).map(|i| format!("src/main.rs:{}:match_main_{i}", i + 1)));
    lines.extend((0..50).map(|i| format!("src/utils.rs:{}:match_utils_{i}", i + 1)));
    let content = lines.join("\n");

    let preview = grep_preview_content(&content, 10);

    // File manifest, sorted by match count.
    // lib.rs (150) should appear before main.rs (50) and utils.rs (50).
    let lib_pos = preview.find("src/lib.rs").expect("lib.rs must appear in manifest");
    let main_pos = preview.find("src/main.rs").expect("main.rs must appear in manifest");
    assert!(lib_pos < main_pos, "lib.rs (more matches) must appear before main.rs");

    // Match counts in manifest.
    assert!(preview.contains("150"), "manifest must show lib.rs match count");

    // Sample output section.
    assert!(preview.contains("Sample output"), "must include sample output section");
    assert!(preview.contains("250 lines total"), "must mention total line count");

    // head/tail preview: first line and last line present.
    assert!(preview.contains("src/lib.rs:1:match_lib_0"), "must include first line");
    assert!(preview.contains("src/utils.rs:50:match_utils_49"), "must include last line");
}

// ---------------------------------------------------------------------------
// AC-1371-08 — Count mode handled gracefully
// ---------------------------------------------------------------------------

// @ac AC-1371-08
#[test]
fn grep_count_mode_handled_gracefully() {
    // Verify: Given Grep output in `count` mode (`rg -c`, `file:N` format) above threshold
    // Verify: Output stored and previewed with file manifest derived from `file:N` rows (files with match counts)
    // Verify: No representative match lines (count mode has counts only)
    // Verify: `ctxl files`, `ctxl show --file`, and `ctxl show --glob` work against count handles

    // Build count-mode content with 250 lines (above threshold of 200).
    let count_lines: Vec<String> =
        (0..250).map(|i| format!("src/file_{i}.rs:{}", i % 20 + 1)).collect();
    let content = count_lines.join("\n");

    let payload = make_grep_payload(&content, "count", 250, &[], 0);
    let conn = in_memory_conn();
    let out = run_grep_intercept_with_conn(payload, 200, &conn);
    assert!(!out.is_empty(), "above-threshold count mode must be intercepted");

    let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
    let updated = &json["hookSpecificOutput"]["updatedToolOutput"];
    let block_msg = updated["content"].as_str().unwrap();
    let handle_id = find_grep_handle(block_msg).expect("must have handle");

    // Block message must mention count mode.
    assert!(block_msg.contains("count mode"), "block message must reference count mode");
    // No match-line format (file:line:text) in the preview body.
    assert!(!block_msg.contains(":fn "), "count mode must not include match lines");

    // Preview must have file manifest from file:N rows.
    let preview = grep_preview_count(&content, 10);
    assert!(preview.contains("Files with match counts"), "must include count manifest header");
    // Files with highest count (i%20+1 → max 20) should appear.
    assert!(preview.contains("src/file_"), "must include file paths");

    // ctxl files works against count handles — tabular output.
    let files = retrieve::files_grep(&conn, &handle_id).expect("files_grep on count handle");
    assert!(!files.is_empty(), "files_grep must return rows for count handles");
    assert!(files[0].contains("Matches"), "first row must be table header");
    assert!(files.iter().any(|r| r.contains("src/file_")), "must include file paths in table rows");

    // ctxl show --file works (returns stored line for that file).
    let file_key = "src/file_0.rs";
    let show_file = retrieve::show_grep_file(&conn, &handle_id, Some(file_key), None, 100)
        .expect("show_grep_file on count handle");
    assert!(show_file.contains(file_key), "show --file must return matching lines");

    // ctxl show --glob *.rs should return all stored lines.
    let show_glob = retrieve::show_grep_file(&conn, &handle_id, None, Some("*.rs"), 1000)
        .expect("show_grep_file --glob on count handle");
    assert!(!show_glob.is_empty(), "glob *.rs must return results for count handle");
}

// ---------------------------------------------------------------------------
// JSON round-trip — verifies #[serde(rename)] attributes on GrepToolResponse
// ---------------------------------------------------------------------------

#[test]
fn json_round_trip_camel_case_keys() {
    let json = serde_json::json!({
        "tool_name": "Grep",
        "tool_response": {
            "content": "src/lib.rs:1:fn main()",
            "numFiles": 3,
            "filenames": ["src/lib.rs"],
            "numMatches": 42,
            "mode": "content",
        }
    });

    let payload: PostToolUsePayload<GrepToolResponse> =
        serde_json::from_value(json).expect("camelCase keys must deserialize into typed payload");

    assert_eq!(payload.tool_name.as_deref(), Some("Grep"));
    assert_eq!(payload.tool_response.num_files, Some(3));
    assert_eq!(payload.tool_response.num_matches, Some(42));
}

// ---------------------------------------------------------------------------
// Wire payload protocol contract
// ---------------------------------------------------------------------------

const GREP_WIRE_PAYLOAD: &str = r#"{"tool_name":"Grep","tool_response":{"content":"src/lib.rs:1:fn main()","numFiles":1,"filenames":["src/lib.rs"],"numMatches":1,"mode":"content"}}"#;

#[test]
fn wire_payload_deserializes_all_fields() {
    let p: PostToolUsePayload<GrepToolResponse> =
        serde_json::from_str(GREP_WIRE_PAYLOAD).expect("protocol contract");
    assert_eq!(p.tool_name.as_deref(), Some("Grep"));
    assert_eq!(p.tool_response.content.as_deref(), Some("src/lib.rs:1:fn main()"));
    assert_eq!(p.tool_response.num_files, Some(1));
    assert_eq!(p.tool_response.num_matches, Some(1));
    assert_eq!(p.tool_response.mode.as_deref(), Some("content"));
    assert_eq!(
        p.tool_response.filenames.as_deref(),
        Some(vec!["src/lib.rs".to_string()].as_slice())
    );
}

// ---------------------------------------------------------------------------
// Truncation transparency — block message and truncated column
// ---------------------------------------------------------------------------

#[test]
fn full_content_block_message_omits_truncation_clause() {
    // Since full content is now stored (no truncation), the block message
    // should NOT contain "stored first X of Y lines".
    let content = make_content_lines(300);
    let filenames = &["src/lib.rs", "src/main.rs", "src/utils.rs"];
    let payload = make_grep_payload(&content, "content", 3, filenames, 300);

    let conn = in_memory_conn();
    let out = run_grep_intercept_with_conn(payload, 200, &conn);
    assert!(!out.is_empty());

    let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
    let block_msg = json["hookSpecificOutput"]["updatedToolOutput"]["content"].as_str().unwrap();

    assert!(
        !block_msg.contains("stored first"),
        "full-content block message must NOT include 'stored first X of Y lines', got: {block_msg}"
    );
}

#[test]
fn non_truncated_block_message_omits_stored_clause() {
    // At-threshold (200 lines) should passthrough, so use a helper that
    // tests the preview function directly with matching stored/total.
    let content = make_content_lines(200);
    let msg = ctxl::compress::grep_preview::grep_preview_ex(
        &content,
        "content",
        "g_test01",
        200,
        &["src/lib.rs".to_string()],
        Some(200),
        Some(200),
    );
    assert!(
        !msg.contains("stored first"),
        "non-truncated message must not include 'stored first', got: {msg}"
    );
}

#[test]
fn truncated_column_false_after_full_content_grep_interception() {
    // Full content is stored — truncated column should be 0 (false).
    let content = make_content_lines(300);
    let filenames = &["src/lib.rs"];
    let payload = make_grep_payload(&content, "content", 1, filenames, 300);

    let conn = in_memory_conn();
    let out = run_grep_intercept_with_conn(payload, 200, &conn);
    assert!(!out.is_empty());

    let truncated: i32 = conn
        .query_row("SELECT truncated FROM handles WHERE tool = 'Grep'", [], |row| row.get(0))
        .expect("handle row must exist");
    assert_eq!(truncated, 0, "truncated column must be 0 since full content is stored");
}

// ---------------------------------------------------------------------------
// #2128 — Grep field-value assertions on stored handles
// ---------------------------------------------------------------------------

#[test]
fn grep_stored_handle_has_correct_field_values() {
    // Assert actual field values in stored handles — not just that the handler ran.
    let content = make_content_lines(300);
    let filenames = &["src/lib.rs", "src/main.rs", "src/utils.rs"];
    let payload = make_grep_payload(&content, "content", 3, filenames, 300);

    let conn = in_memory_conn();
    let out = run_grep_intercept_with_conn(payload, 200, &conn);
    assert!(!out.is_empty());

    let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
    let handle_id = find_grep_handle(
        json["hookSpecificOutput"]["updatedToolOutput"]["content"].as_str().unwrap(),
    )
    .unwrap();

    // Verify stored handle metadata via inspect.
    let info = retrieve::inspect(&conn, &handle_id).expect("inspect should succeed");
    assert_eq!(info.tool, "Grep", "tool must be Grep");
    assert_eq!(info.output_mode, "content", "output_mode must be content");
    assert_eq!(info.line_count, Some(300), "line_count must be 300 (full content)");
    assert!(!info.truncated, "truncated must be false (full content stored)");
    assert!(info.token_est.is_some(), "token_est must be present");
    assert!(info.token_est.unwrap() > 0, "token_est must be positive");
    assert!(info.created_at > 0, "created_at must be set");

    // Verify compressed_method is set for content mode.
    // Content mode runs SimHash dedup; if it compressed, the method should be "grep_dedup".
    // If not compressed (deduped was empty or larger), it's None — both are valid.
    // Just verify the field is queryable.
    let _compressed: Option<String> = conn
        .query_row("SELECT compressed_method FROM handles WHERE id=?1", [&handle_id], |row| {
            row.get(0)
        })
        .expect("compressed_method must be queryable");
}

#[test]
fn grep_stored_handle_count_mode_field_values() {
    // Verify field values for count mode handles specifically.
    let count_lines: Vec<String> =
        (0..250).map(|i| format!("src/file_{i}.rs:{}", i % 20 + 1)).collect();
    let content = count_lines.join("\n");

    let payload = make_grep_payload(&content, "count", 250, &[], 0);
    let conn = in_memory_conn();
    let out = run_grep_intercept_with_conn(payload, 200, &conn);
    assert!(!out.is_empty());

    let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
    let handle_id = find_grep_handle(
        json["hookSpecificOutput"]["updatedToolOutput"]["content"].as_str().unwrap(),
    )
    .unwrap();

    let info = retrieve::inspect(&conn, &handle_id).expect("inspect should succeed");
    assert_eq!(info.tool, "Grep", "tool must be Grep");
    assert_eq!(info.output_mode, "count", "output_mode must be count");
    assert_eq!(info.line_count, Some(250), "line_count must be 250 (full content)");
    assert!(!info.truncated, "truncated must be false (full content stored)");
    // Count mode does NOT run SimHash dedup, so compressed_method should be None.
    assert!(info.compressed_method.is_none(), "count mode should not have compression");
}

// ---------------------------------------------------------------------------
// #2128 — Fail-open test for DB write failure
// ---------------------------------------------------------------------------

#[test]
fn grep_intercept_fails_open_on_db_error() {
    // When the DB connection is broken (read-only, closed, etc.), the intercept
    // must not crash — it should return an error that the caller can handle as
    // fail-open (exit 0, output passes through).
    //
    // We simulate this by using a read-only in-memory DB: opening a second
    // connection to an in-memory DB that was never schema-initialized.
    let conn = Connection::open_in_memory().expect("open_in_memory");
    // Deliberately NOT calling db::apply_schema — tables don't exist.

    let content = make_content_lines(300);
    let payload = make_grep_payload(&content, "content", 1, &["src/lib.rs"], 300);
    let config = GrepInterceptConfig {
        threshold: 200,
        record: true,
        tool_input: None,
        cwd: None,
        tool_use_id: None,
    };

    let mut output = Vec::new();
    let result = intercept_grep::run(payload, &mut output, &config, &conn);

    // The run should return an error (table doesn't exist), but it should not panic.
    assert!(result.is_err(), "should return error when DB has no schema");
    // Output should be empty (nothing written before the error).
    // The caller (main.rs router) catches this error and exits 0 — fail-open.
}
