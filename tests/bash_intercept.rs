#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used, clippy::panic)]
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

/// Build a typed PostToolUsePayload for the Bash handler.
fn make_payload(
    stdout: &str,
    stderr: &str,
    interrupted: bool,
    is_image: bool,
    no_output_expected: bool,
) -> PostToolUsePayload<ToolResponse> {
    PostToolUsePayload {
        tool_name: Some("Bash".into()),
        tool_response: ToolResponse {
            stdout: Some(stdout.into()),
            stderr: Some(stderr.into()),
            interrupted: Some(interrupted),
            is_image: Some(is_image),
            no_output_expected: Some(no_output_expected),
            exit_code: None,
        },
    }
}

/// Run intercept with `record=true` using a fresh in-memory DB.
fn run_intercept(payload: PostToolUsePayload<ToolResponse>, threshold: usize) -> Vec<u8> {
    let conn = in_memory_conn();
    run_intercept_with_conn(payload, threshold, true, &conn)
}

/// Run intercept with an explicit `record` flag and a caller-supplied connection.
fn run_intercept_with_conn(
    payload: PostToolUsePayload<ToolResponse>,
    threshold: usize,
    record: bool,
    conn: &Connection,
) -> Vec<u8> {
    let config = InterceptConfig {
        threshold,
        line_threshold: 200,
        record,
        command: None,
        tool_input: None,
        cwd: None,
        tool_use_id: None,
    };
    let mut output = Vec::new();
    intercept::run(payload, &mut output, &config, conn).expect("intercept::run should not fail");
    output
}

/// Find the first `b_<hex>` token in `s` (handle ID format from store::write).
/// Returns the full token (e.g. "b_1a2b3c") if found.
fn find_handle_id(s: &str) -> Option<String> {
    let pos = s.find("b_")?;
    let hex = &s[pos + 2..];
    let hex_len = hex.chars().take_while(|c| c.is_ascii_hexdigit()).count();
    if hex_len >= 6 {
        Some(s[pos..pos + 2 + hex_len].to_string())
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// AC-1113-01
// ---------------------------------------------------------------------------

// @ac AC-1113-01
#[test]
fn below_threshold_passes_through_no_output() {
    // Verify: given PostToolUse payload with combined stdout+stderr (post-ANSI-strip) <= 8192 bytes
    // Verify: exits with status 0 and stdout is empty

    // 4 KB — well below the 8192-byte threshold.
    let stdout = "a".repeat(2048);
    let stderr = "b".repeat(2048);
    let payload = make_payload(&stdout, &stderr, false, false, false);

    let output = run_intercept(payload, 8192);

    assert!(
        output.is_empty(),
        "stdout should be empty for below-threshold payload, got {} bytes",
        output.len()
    );
}

// ---------------------------------------------------------------------------
// AC-1113-02
// ---------------------------------------------------------------------------

// @ac AC-1113-02
#[test]
fn above_threshold_emits_valid_hook_specific_output_envelope() {
    // Verify: given payload with combined size > threshold, stdout is valid JSON
    // Verify: JSON parses to { hookSpecificOutput: { hookEventName: "PostToolUse", ... } }
    // Verify: updatedToolOutput contains stdout, stderr, interrupted, isImage, noOutputExpected fields

    // 10 KB of stdout — above 8192-byte threshold.
    let stdout = "x".repeat(10_240);
    let payload = make_payload(&stdout, "", false, false, false);

    let output = run_intercept(payload, 8192);

    assert!(!output.is_empty(), "output should not be empty for above-threshold payload");

    let json: serde_json::Value =
        serde_json::from_slice(&output).expect("output should be valid JSON");

    let hook_output = json.get("hookSpecificOutput").expect("JSON must have 'hookSpecificOutput'");

    assert_eq!(
        hook_output.get("hookEventName").and_then(|v| v.as_str()),
        Some("PostToolUse"),
        "hookEventName must be 'PostToolUse'"
    );

    let updated = hook_output
        .get("updatedToolOutput")
        .expect("hookSpecificOutput must have 'updatedToolOutput'");

    // All required fields must be present with correct types.
    assert!(
        updated.get("stdout").and_then(|v| v.as_str()).is_some(),
        "updatedToolOutput.stdout must be a string"
    );
    assert!(
        updated.get("stderr").and_then(|v| v.as_str()).is_some(),
        "updatedToolOutput.stderr must be a string"
    );
    assert!(
        updated.get("interrupted").and_then(|v| v.as_bool()).is_some(),
        "updatedToolOutput.interrupted must be a bool"
    );
    assert!(
        updated.get("isImage").and_then(|v| v.as_bool()).is_some(),
        "updatedToolOutput.isImage must be a bool"
    );
    assert!(
        updated.get("noOutputExpected").and_then(|v| v.as_bool()).is_some(),
        "updatedToolOutput.noOutputExpected must be a bool"
    );
}

// ---------------------------------------------------------------------------
// AC-1113-03
// ---------------------------------------------------------------------------

// @ac AC-1113-03
#[test]
fn block_message_contains_handle_id_counts_and_retrieve_hint() {
    // Verify: updatedToolOutput.stdout contains substring matching b_[0-9a-f]{6}
    // Verify: updatedToolOutput.stdout contains the line count
    // Verify: updatedToolOutput.stdout contains the byte count
    // Verify: updatedToolOutput.stdout contains `ctxl show ` followed by the handle ID

    // Verify format_block_message produces a correctly-structured message.
    let msg = intercept::format_block_message("b_1a2b3c", 42, 9000, "test content line");
    assert!(
        find_handle_id(&msg).as_deref() == Some("b_1a2b3c"),
        "format_block_message must include the handle ID: {msg:?}"
    );
    assert!(msg.contains("42"), "format_block_message must include line count 42: {msg:?}");
    assert!(msg.contains("9000"), "format_block_message must include byte count 9000: {msg:?}");
    assert!(
        msg.contains("ctxl show b_1a2b3c"),
        "format_block_message must include 'ctxl show b_1a2b3c': {msg:?}"
    );

    // Also verify end-to-end through run(): a live blocked payload contains the same checks.
    // 100 lines × 100 chars = 10 000 bytes > 8192 threshold.
    let line = "y".repeat(100);
    let stdout: String = (0..100).map(|_| line.as_str()).collect::<Vec<_>>().join("\n");
    let payload = make_payload(&stdout, "", false, false, false);

    let output = run_intercept(payload, 8192);
    assert!(!output.is_empty(), "should have blocked output");

    let json: serde_json::Value = serde_json::from_slice(&output).expect("valid JSON");
    let block_stdout = json["hookSpecificOutput"]["updatedToolOutput"]["stdout"]
        .as_str()
        .expect("stdout must be a string");

    // (a) Handle ID matching b_[0-9a-f]{6} must be present.
    let handle_id = find_handle_id(block_stdout)
        .expect("block message must contain a handle ID matching b_[0-9a-f]{6+}");

    // (b) Line count
    let line_count = stdout.lines().count();
    assert!(
        block_stdout.contains(&line_count.to_string()),
        "block message must contain line count {line_count}: {block_stdout:?}"
    );

    // (c) Byte count (ANSI-stripped; no ANSI in this payload so == raw len)
    let byte_count = stdout.len();
    assert!(
        block_stdout.contains(&byte_count.to_string()),
        "block message must contain byte count {byte_count}: {block_stdout:?}"
    );

    // (d) ctxl show <handle_id>
    let expected_hint = format!("ctxl show {}", handle_id);
    assert!(
        block_stdout.contains(&expected_hint),
        "block message must contain '{expected_hint}': {block_stdout:?}"
    );
}

// ---------------------------------------------------------------------------
// AC-1113-04
// ---------------------------------------------------------------------------

// @ac AC-1113-04
#[test]
fn preserves_interrupted_is_image_no_output_expected_from_tool_response() {
    // Verify: given payload where tool_response.interrupted=true, isImage=false, noOutputExpected=false
    // Verify: emitted updatedToolOutput has interrupted=true
    // Verify: emitted updatedToolOutput has isImage=false
    // Verify: emitted updatedToolOutput has noOutputExpected=false
    //
    // Use size >= 2× threshold so the blocking path is reached despite interrupted=true.

    // 20 KB > 2 × 8192 = 16384 → blocking path reached.
    let stdout = "z".repeat(20_480);
    let payload = make_payload(
        &stdout, "", /*interrupted=*/ true, /*isImage=*/ false,
        /*noOutputExpected=*/ false,
    );

    let output = run_intercept(payload, 8192);
    assert!(!output.is_empty(), "should have blocked output (size >= 2× threshold)");

    let json: serde_json::Value = serde_json::from_slice(&output).expect("valid JSON");
    let updated = &json["hookSpecificOutput"]["updatedToolOutput"];

    assert_eq!(
        updated["interrupted"].as_bool(),
        Some(true),
        "updatedToolOutput.interrupted must be preserved as true"
    );
    assert_eq!(
        updated["isImage"].as_bool(),
        Some(false),
        "updatedToolOutput.isImage must be preserved as false"
    );
    assert_eq!(
        updated["noOutputExpected"].as_bool(),
        Some(false),
        "updatedToolOutput.noOutputExpected must be preserved as false"
    );
}

// ---------------------------------------------------------------------------
// AC-1113-05
// ---------------------------------------------------------------------------

// @ac AC-1113-05
#[test]
fn is_image_true_always_passes_through_regardless_of_size() {
    // Verify: given payload with tool_response.isImage=true and combined size > 100KB
    // Verify: exits 0 with empty stdout (passthrough) regardless of size exceeding threshold

    // 200 KB — over the 100 KB mentioned in the AC and well above any threshold.
    let stdout = "I".repeat(200_000);
    let payload = make_payload(&stdout, "", false, /*isImage=*/ true, false);

    let output = run_intercept(payload, 8192);

    assert!(
        output.is_empty(),
        "stdout should be empty for isImage=true payload, got {} bytes",
        output.len()
    );
}

// ---------------------------------------------------------------------------
// AC-1113-06
// ---------------------------------------------------------------------------

// @ac AC-1113-06
#[test]
fn interrupted_true_and_size_below_2x_threshold_passes_through() {
    // Verify: given tool_response.interrupted=true and combined size between 1x and 2x threshold
    //         (e.g. 12KB with threshold 8KB), exits 0 with empty stdout (passthrough)
    // Verify: given interrupted=true and size >= 2x threshold (e.g. 20KB), emits the replacement envelope

    const THRESHOLD: usize = 8192;

    // Part 1: 12 KB (between 1× and 2× threshold), interrupted=true → passthrough
    let stdout_12k = "p".repeat(12_288);
    let payload_below = make_payload(&stdout_12k, "", /*interrupted=*/ true, false, false);
    let output_below = run_intercept(payload_below, THRESHOLD);
    assert!(
        output_below.is_empty(),
        "interrupted=true with 12KB (< 2×8KB) should passthrough, got {} bytes",
        output_below.len()
    );

    // Part 2: 20 KB (>= 2× threshold), interrupted=true → block
    let stdout_20k = "q".repeat(20_480);
    let payload_above = make_payload(&stdout_20k, "", /*interrupted=*/ true, false, false);
    let output_above = run_intercept(payload_above, THRESHOLD);
    assert!(!output_above.is_empty(), "interrupted=true with 20KB (>= 2×8KB) should emit envelope");

    let json: serde_json::Value =
        serde_json::from_slice(&output_above).expect("envelope should be valid JSON");
    assert!(json.get("hookSpecificOutput").is_some(), "envelope must contain 'hookSpecificOutput'");
}

// ---------------------------------------------------------------------------
// AC-1113-07
// ---------------------------------------------------------------------------

// @ac AC-1113-07
#[test]
fn record_false_skips_calls_row_but_still_intercepts() {
    // Verify: record=false still intercepts (writes handle), but skips the calls INSERT.
    // The CTXL_ENABLED=0 kill-switch is now at binary entry level (see binary test below).

    const THRESHOLD: usize = 8192;
    // 20 KB — well above threshold.
    let stdout = "k".repeat(20_480);
    let conn = in_memory_conn();

    // Part 1: record=false → still intercepts, but no calls row
    let payload1 = make_payload(&stdout, "", false, false, false);
    let output_norecord =
        run_intercept_with_conn(payload1, THRESHOLD, /*record=*/ false, &conn);
    assert!(
        !output_norecord.is_empty(),
        "record=false should still intercept above-threshold output"
    );
    let calls_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM calls", [], |row| row.get(0)).unwrap();
    assert_eq!(calls_count, 0, "record=false should skip calls INSERT");

    // Part 2: record=true → intercepts AND inserts calls row
    let payload2 = make_payload(&stdout, "", false, false, false);
    let output_record = run_intercept_with_conn(payload2, THRESHOLD, /*record=*/ true, &conn);
    assert!(
        !output_record.is_empty(),
        "with record=true and size > threshold, should emit envelope"
    );
    let calls_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM calls", [], |row| row.get(0)).unwrap();
    assert_eq!(calls_count, 1, "record=true should insert calls row");
}

// ---------------------------------------------------------------------------
// Threshold boundary tests (#1252)
// ---------------------------------------------------------------------------

#[test]
fn exact_threshold_passes_through() {
    // Payload with exactly 8192 ANSI-stripped bytes -> passthrough (<=)
    let stdout = "a".repeat(8192);
    let payload = make_payload(&stdout, "", false, false, false);
    let output = run_intercept(payload, 8192);
    assert!(
        output.is_empty(),
        "exactly at threshold should passthrough, got {} bytes",
        output.len()
    );
}

#[test]
fn one_byte_over_threshold_blocks() {
    // 8193 bytes -> blocks
    let stdout = "a".repeat(8193);
    let payload = make_payload(&stdout, "", false, false, false);
    let output = run_intercept(payload, 8192);
    assert!(!output.is_empty(), "one byte over threshold should block");
    let json: serde_json::Value = serde_json::from_slice(&output).expect("valid JSON");
    assert!(json.get("hookSpecificOutput").is_some(), "should emit hookSpecificOutput envelope");
}

#[test]
fn no_output_expected_true_large_payload_blocks() {
    // >8192 bytes with noOutputExpected=true -> still blocks
    // (noOutputExpected is preserved in the envelope, not a passthrough condition)
    let stdout = "a".repeat(10_000);
    let payload = make_payload(&stdout, "", false, false, true);
    let output = run_intercept(payload, 8192);
    assert!(
        !output.is_empty(),
        "noOutputExpected=true should NOT cause passthrough for large output"
    );
}

#[test]
fn no_output_expected_preserved_in_envelope() {
    // >8192 bytes with noOutputExpected=true -> envelope has noOutputExpected=true
    let stdout = "a".repeat(10_000);
    let payload = make_payload(&stdout, "", false, false, true);
    let output = run_intercept(payload, 8192);
    let json: serde_json::Value = serde_json::from_slice(&output).expect("valid JSON");
    let noe = json["hookSpecificOutput"]["updatedToolOutput"]["noOutputExpected"].as_bool();
    assert_eq!(noe, Some(true), "noOutputExpected should be preserved as true in envelope");
}

#[test]
fn ansi_codes_excluded_from_measurement() {
    // Raw bytes > threshold, but stripped bytes < threshold -> passthrough
    // Each ANSI color sequence "\x1b[31m" is 5 bytes that get stripped
    // Build a string where raw > 8192 but stripped <= 8192
    let plain = "x".repeat(8000); // 8000 stripped bytes
                                  // Add enough ANSI codes to push raw over 8192
                                  // Each "\x1b[31m" adds 5 raw bytes but 0 stripped bytes
    let ansi_prefix = "\x1b[31m".repeat(40); // 200 raw bytes, 0 stripped
    let content = format!("{ansi_prefix}{plain}");
    assert!(content.len() > 8192, "raw bytes should exceed threshold");

    let payload = make_payload(&content, "", false, false, false);
    let output = run_intercept(payload, 8192);
    assert!(
        output.is_empty(),
        "ANSI codes should not count toward threshold; stripped={}, raw={}",
        8000,
        content.len()
    );
}

#[test]
fn empty_stdout_and_stderr() {
    // Both empty -> 0 bytes -> passthrough
    let payload = make_payload("", "", false, false, false);
    let output = run_intercept(payload, 8192);
    assert!(output.is_empty(), "empty output should passthrough");
}

#[test]
fn threshold_zero_blocks_any_nonempty() {
    // threshold=0, 1 byte of output -> blocks (1 > 0)
    let payload = make_payload("x", "", false, false, false);
    let output = run_intercept(payload, 0);
    assert!(!output.is_empty(), "threshold=0 with any output should block");
}

#[test]
fn interrupted_at_exact_2x_blocks() {
    // interrupted=true, exactly 2*threshold bytes -> blocks
    // intercept.rs line ~107: `combined_bytes < 2 * config.threshold` uses strict <
    // So exactly 2x is NOT < 2x, meaning it blocks
    let threshold = 4096;
    let stdout = "a".repeat(2 * threshold);
    let payload = make_payload(&stdout, "", true, false, false);
    let output = run_intercept(payload, threshold);
    assert!(!output.is_empty(), "interrupted=true at exactly 2x threshold should block (< not <=)");
}

// ---------------------------------------------------------------------------
// Line-count threshold
// ---------------------------------------------------------------------------

#[test]
fn line_count_triggers_interception_below_byte_threshold() {
    // 210 short lines (~1600 bytes) — below byte threshold but above line threshold (200).
    let stdout: String = (0..210).map(|i| format!("line{i}\n")).collect();
    assert!(stdout.len() < 8192, "payload should be below byte threshold");

    let payload = make_payload(&stdout, "", false, false, false);
    // line_threshold=200: 210 lines > 200 → should intercept
    let conn = in_memory_conn();
    let config = InterceptConfig {
        threshold: 8192,
        line_threshold: 200,
        record: true,
        command: None,
        tool_input: None,
        cwd: None,
        tool_use_id: None,
    };
    let mut output = Vec::new();
    intercept::run(payload, &mut output, &config, &conn).expect("intercept::run");

    assert!(!output.is_empty(), "210 lines (>200 line threshold) should trigger interception despite being below byte threshold");
}

#[test]
fn below_both_thresholds_passes_through() {
    // 10 lines, tiny bytes — below both thresholds.
    let stdout: String = (0..10).map(|i| format!("line{i}\n")).collect();
    assert!(stdout.len() < 8192);

    let payload = make_payload(&stdout, "", false, false, false);
    let conn = in_memory_conn();
    let config = InterceptConfig {
        threshold: 8192,
        line_threshold: 200,
        record: true,
        command: None,
        tool_input: None,
        cwd: None,
        tool_use_id: None,
    };
    let mut output = Vec::new();
    intercept::run(payload, &mut output, &config, &conn).expect("intercept::run");

    assert!(output.is_empty(), "10 lines and small bytes should passthrough");
}

// ---------------------------------------------------------------------------
// OutputMode
// ---------------------------------------------------------------------------

#[test]
fn output_mode_round_trips_all_variants() {
    use ctxl::store::OutputMode;
    for (s, expected) in [
        ("stdout", OutputMode::Stdout),
        ("stderr", OutputMode::Stderr),
        ("mixed", OutputMode::Mixed),
    ] {
        let parsed: OutputMode = s.parse().expect(s);
        assert_eq!(parsed, expected);
        assert_eq!(parsed.to_string(), s);
    }
}

#[test]
fn output_mode_rejects_unknown() {
    assert!(
        "unknown".parse::<ctxl::store::OutputMode>().is_err(),
        "OutputMode::from_str should reject unrecognised strings"
    );
    assert!(
        "STDOUT".parse::<ctxl::store::OutputMode>().is_err(),
        "OutputMode::from_str should be case-sensitive"
    );
}

// ---------------------------------------------------------------------------
// #1259 — Mixed-mode off-by-1 line count
// ---------------------------------------------------------------------------

#[test]
fn mixed_mode_no_trailing_newline_inserts_separator() {
    // stdout="line1" (no trailing newline) + stderr="err1\n"
    // Should produce "line1\nerr1\n" -> 2 lines, not "line1err1\n" -> 1 line
    let stdout = "line1";
    let stderr = "err1\n";
    // Need combined size > threshold to trigger blocking path.
    // Use threshold=1 so any non-empty output triggers blocking.
    let payload = make_payload(stdout, stderr, false, false, false);
    let output = run_intercept(payload, 1);

    assert!(!output.is_empty(), "should block above-threshold output");

    let json: serde_json::Value = serde_json::from_slice(&output).expect("valid JSON");
    let block_stdout =
        json["hookSpecificOutput"]["updatedToolOutput"]["stdout"].as_str().expect("stdout string");

    // line_count should be 2 (line1, err1)
    assert!(
        block_stdout.contains("2 lines"),
        "mixed mode with no trailing newline on stdout should count 2 lines, got: {block_stdout}"
    );
}

#[test]
fn mixed_mode_trailing_newline_no_extra_separator() {
    // stdout="line1\n" + stderr="err1\n" -> "line1\nerr1\n" -> 2 lines
    // No extra separator needed since stdout already ends with \n.
    let stdout = "line1\n";
    let stderr = "err1\n";
    let payload = make_payload(stdout, stderr, false, false, false);
    let output = run_intercept(payload, 1);

    assert!(!output.is_empty(), "should block above-threshold output");

    let json: serde_json::Value = serde_json::from_slice(&output).expect("valid JSON");
    let block_stdout =
        json["hookSpecificOutput"]["updatedToolOutput"]["stdout"].as_str().expect("stdout string");

    assert!(
        block_stdout.contains("2 lines"),
        "mixed mode with trailing newline should count 2 lines, got: {block_stdout}"
    );
}

// ---------------------------------------------------------------------------
// #1310 — Binary-level CTXL_ENABLED=0 kill switch
// ---------------------------------------------------------------------------

#[test]
fn binary_ctxl_enabled_zero_exits_clean() {
    // Verify: invoking the ctxl binary with CTXL_ENABLED=0 exits 0 with empty stdout.
    // This test exercises the env-var kill switch at the binary level so that
    // refactors of the `Intercept` match arm are caught by CI.
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ctxl"))
        .args(["intercept"])
        .env("CTXL_ENABLED", "0")
        .stdin(std::process::Stdio::piped())
        .output()
        .unwrap();
    assert!(output.status.success(), "CTXL_ENABLED=0 should exit 0, got: {:?}", output.status);
    assert!(
        output.stdout.is_empty(),
        "CTXL_ENABLED=0 should produce empty stdout, got {} bytes",
        output.stdout.len()
    );
}

// ---------------------------------------------------------------------------
// #1260 — Missing tool_name defaults with warning
// ---------------------------------------------------------------------------

#[test]
fn missing_tool_name_defaults_with_warning() {
    // Build payload WITHOUT tool_name — should still succeed.
    let payload = PostToolUsePayload {
        tool_name: None,
        tool_response: ToolResponse {
            stdout: Some("x".repeat(10_000)),
            stderr: Some(String::new()),
            interrupted: Some(false),
            is_image: Some(false),
            no_output_expected: Some(false),
            exit_code: None,
        },
    };

    let output = run_intercept(payload, 8192);
    assert!(!output.is_empty(), "should block above-threshold output even without tool_name");

    // Verify the envelope is still valid JSON with expected structure.
    let json: serde_json::Value = serde_json::from_slice(&output).expect("valid JSON");
    assert!(
        json.get("hookSpecificOutput").is_some(),
        "must produce valid hookSpecificOutput envelope without tool_name"
    );
}

// ---------------------------------------------------------------------------
// JSON round-trip — verifies #[serde(rename)] attributes on ToolResponse
// ---------------------------------------------------------------------------

#[test]
fn json_round_trip_camel_case_keys() {
    // Construct JSON with camelCase keys matching the wire format, then
    // deserialize into the typed payload struct.  This exercises the
    // #[serde(rename = "isImage")] and #[serde(rename = "noOutputExpected")]
    // attributes on ToolResponse.
    let json = serde_json::json!({
        "tool_name": "Bash",
        "tool_response": {
            "stdout": "hello",
            "stderr": "",
            "interrupted": false,
            "isImage": true,
            "noOutputExpected": true,
            "exitCode": 42,
        }
    });

    let payload: PostToolUsePayload<ToolResponse> =
        serde_json::from_value(json).expect("camelCase keys must deserialize into typed payload");

    assert_eq!(payload.tool_name.as_deref(), Some("Bash"));
    assert_eq!(payload.tool_response.stdout.as_deref(), Some("hello"));
    assert_eq!(payload.tool_response.is_image, Some(true));
    assert_eq!(payload.tool_response.no_output_expected, Some(true));
    assert_eq!(payload.tool_response.exit_code, Some(42));
}

// ---------------------------------------------------------------------------
// Wire payload protocol contract
// ---------------------------------------------------------------------------

const BASH_WIRE_PAYLOAD: &str = r#"{"tool_name":"Bash","tool_response":{"stdout":"x","stderr":"","interrupted":false,"isImage":false,"noOutputExpected":false,"exitCode":0}}"#;

#[test]
fn wire_payload_deserializes_all_fields() {
    let p: PostToolUsePayload<ToolResponse> =
        serde_json::from_str(BASH_WIRE_PAYLOAD).expect("protocol contract");
    assert_eq!(p.tool_name.as_deref(), Some("Bash"));
    assert_eq!(p.tool_response.stdout.as_deref(), Some("x"));
    assert_eq!(p.tool_response.stderr.as_deref(), Some(""));
    assert_eq!(p.tool_response.is_image, Some(false));
    assert_eq!(p.tool_response.no_output_expected, Some(false));
    assert_eq!(p.tool_response.interrupted, Some(false));
    assert_eq!(p.tool_response.exit_code, Some(0));
}

// ---------------------------------------------------------------------------
// Diff-aware block message tests
// ---------------------------------------------------------------------------

/// Helper: build a multi-file unified diff string for testing.
fn make_multi_file_diff() -> String {
    [
        "diff --git a/src/intercept.rs b/src/intercept.rs",
        "index abc..def 100644",
        "--- a/src/intercept.rs",
        "+++ b/src/intercept.rs",
        "@@ -5,6 +5,7 @@ fn foo() {",
        " let x = 1;",
        "+    let y = 2;",
        "@@ -20,3 +21,4 @@ fn bar() {",
        " let a = 3;",
        "+    let b = 4;",
        "@@ -40,3 +42,4 @@ fn baz() {",
        " let c = 5;",
        "+    let d = 6;",
        "@@ -60,3 +63,4 @@ fn qux() {",
        " let e = 7;",
        "+    let f = 8;",
        "@@ -80,3 +84,4 @@ fn quux() {",
        " let g = 9;",
        "+    let h = 10;",
        "diff --git a/src/main.rs b/src/main.rs",
        "index 111..222 100644",
        "--- a/src/main.rs",
        "+++ b/src/main.rs",
        "@@ -1,3 +1,4 @@",
        " fn main() {",
        "+    hello();",
        "@@ -10,3 +11,4 @@ fn helper() {",
        " let x = 1;",
        "+    let y = 2;",
        "@@ -20,3 +22,4 @@ fn util() {",
        " let a = 3;",
        "+    let b = 4;",
        "diff --git a/src/retrieve.rs b/src/retrieve.rs",
        "index 333..444 100644",
        "--- a/src/retrieve.rs",
        "+++ b/src/retrieve.rs",
        "@@ -1,3 +1,4 @@",
        " fn retrieve() {",
        "+    retrieving();",
        "@@ -10,3 +11,4 @@ fn search() {",
        " let x = 1;",
        "+    let y = 2;",
    ]
    .join("\n")
}

#[test]
fn test_block_message_diff_output() {
    let diff = make_multi_file_diff();
    let msg = intercept::format_block_message("b_abc123", 50, 2000, &diff);

    // Should contain file count (3 files).
    assert!(msg.contains("3 files"), "diff output should report file count: {msg}");

    // Top files line should use "hunks" unit.
    assert!(msg.contains("hunks"), "diff top files should use 'hunks' unit: {msg}");

    // Top file should be intercept.rs with 5 hunks.
    assert!(
        msg.contains("src/intercept.rs (5 hunks)"),
        "should show top file with hunk count: {msg}"
    );

    // Second file should be main.rs with 3 hunks.
    assert!(
        msg.contains("src/main.rs (3 hunks)"),
        "should show second file with hunk count: {msg}"
    );

    // Third file should be retrieve.rs with 2 hunks.
    assert!(
        msg.contains("src/retrieve.rs (2 hunks)"),
        "should show third file with hunk count: {msg}"
    );
}

#[test]
fn test_block_message_diff_includes_files_hint() {
    let diff = make_multi_file_diff();
    let msg = intercept::format_block_message("b_abc123", 50, 2000, &diff);

    assert!(
        msg.contains("ctxl files b_abc123"),
        "diff output should include 'ctxl files' hint: {msg}"
    );
    assert!(
        msg.contains("ctxl show b_abc123"),
        "diff output should include 'ctxl show' hint: {msg}"
    );
    assert!(
        msg.contains("ctxl search b_abc123"),
        "diff output should include 'ctxl search' hint: {msg}"
    );
}

#[test]
fn test_preview_diff_skips_lockfile() {
    // Build a diff where first file is package-lock.json and second is src/main.rs.
    let diff = [
        "diff --git a/package-lock.json b/package-lock.json",
        "index abc..def 100644",
        "--- a/package-lock.json",
        "+++ b/package-lock.json",
        "@@ -1,3 +1,4 @@",
        " {",
        "+  added",
        "diff --git a/src/main.rs b/src/main.rs",
        "index 111..222 100644",
        "--- a/src/main.rs",
        "+++ b/src/main.rs",
        "@@ -1,3 +1,4 @@",
        " fn main() {",
        "+    hello();",
    ]
    .join("\n");

    let msg = intercept::format_block_message("b_abc123", 15, 500, &diff);

    // Preview should show src/main.rs, not package-lock.json.
    assert!(
        msg.contains("src/main.rs"),
        "preview should show first non-generated file, got: {msg}"
    );
    // The preview arrow line should NOT contain package-lock.json.
    for line in msg.lines() {
        if line.trim_start().starts_with("→") || line.contains("→") {
            assert!(
                !line.contains("package-lock.json"),
                "preview should skip lockfile, got: {line}"
            );
        }
    }
}

#[test]
fn test_block_message_grep_not_regressed() {
    // Existing grep-like output format should be unchanged.
    let lines: Vec<String> = (1..=20).map(|i| format!("src/lib.rs:{i}:match_{i}")).collect();
    let content = lines.join("\n");
    let msg = intercept::format_block_message("b_abc123", 20, 500, &content);

    // Grep output should still use bare counts (no "hunks").
    assert!(
        msg.contains("Top: src/lib.rs (20)"),
        "grep top files should use bare count, not hunks: {msg}"
    );
    assert!(!msg.contains("hunks"), "grep output should NOT use 'hunks' unit: {msg}");
    assert!(
        msg.contains("ctxl files b_abc123"),
        "grep output should still include files hint: {msg}"
    );
    assert!(msg.contains("1 files"), "grep output should report file count: {msg}");
}

#[test]
fn test_extract_preview_diff_no_plus_lines_falls_through() {
    // Diff header present but no '+++ b/' lines — should fall through to
    // generic preview logic (last non-empty line).
    let diff =
        ["diff --git a/src/foo.rs b/src/foo.rs", "index abc..def 100644", "Binary files differ"]
            .join("\n");

    let msg = intercept::format_block_message("b_abc123", 3, 100, &diff);

    // Should still produce a valid block message (diff-aware stats path).
    assert!(msg.contains("b_abc123"), "must contain handle ID: {msg}");
    // Preview should fall through to the last non-empty line since there
    // are no '+++ b/' lines to extract a file path from.
    assert!(msg.contains("Binary files differ"), "preview should fall through to last line: {msg}");
}

// ---------------------------------------------------------------------------
// Exit code surfacing (#1955)
// ---------------------------------------------------------------------------

#[test]
fn nonzero_exit_code_shows_failed_message() {
    let conn = in_memory_conn();
    let stdout = "error[E0308]: mismatched types\n".repeat(210);
    let payload = PostToolUsePayload {
        tool_name: Some("Bash".into()),
        tool_response: ToolResponse {
            stdout: Some(stdout),
            stderr: Some(String::new()),
            interrupted: Some(false),
            is_image: Some(false),
            no_output_expected: Some(false),
            exit_code: Some(101),
        },
    };
    let config = InterceptConfig {
        threshold: 8192,
        line_threshold: 200,
        record: true,
        command: None,
        tool_input: None,
        cwd: None,
        tool_use_id: None,
    };
    let mut output = Vec::new();
    intercept::run(payload, &mut output, &config, &conn).expect("run");

    assert!(!output.is_empty(), "should intercept");
    let json: serde_json::Value = serde_json::from_slice(&output).expect("valid JSON");
    let block_stdout =
        json["hookSpecificOutput"]["updatedToolOutput"]["stdout"].as_str().expect("stdout string");

    assert!(
        block_stdout.contains("Command failed"),
        "non-zero exit code should show 'Command failed': {block_stdout}"
    );
    assert!(block_stdout.contains("exit 101"), "should show exit code 101: {block_stdout}");
    assert!(
        block_stdout.contains("ctxl search"),
        "should suggest search for error: {block_stdout}"
    );
}

#[test]
fn zero_exit_code_shows_normal_message() {
    let conn = in_memory_conn();
    let stdout = "x".repeat(10_000);
    let payload = PostToolUsePayload {
        tool_name: Some("Bash".into()),
        tool_response: ToolResponse {
            stdout: Some(stdout),
            stderr: Some(String::new()),
            interrupted: Some(false),
            is_image: Some(false),
            no_output_expected: Some(false),
            exit_code: Some(0),
        },
    };
    let config = InterceptConfig {
        threshold: 8192,
        line_threshold: 200,
        record: true,
        command: None,
        tool_input: None,
        cwd: None,
        tool_use_id: None,
    };
    let mut output = Vec::new();
    intercept::run(payload, &mut output, &config, &conn).expect("run");

    assert!(!output.is_empty());
    let json: serde_json::Value = serde_json::from_slice(&output).expect("valid JSON");
    let block_stdout =
        json["hookSpecificOutput"]["updatedToolOutput"]["stdout"].as_str().expect("stdout string");

    assert!(
        !block_stdout.contains("Command failed"),
        "exit code 0 should NOT show failed message: {block_stdout}"
    );
    assert!(
        block_stdout.contains("Output captured"),
        "exit code 0 should show normal message: {block_stdout}"
    );
}

// ---------------------------------------------------------------------------
// Preview extraction — stderr visibility and ANSI stripping
// ---------------------------------------------------------------------------

#[test]
fn success_path_preview_sees_stderr() {
    // Exit 0 with bulky stdout and the decisive summary on stderr — where
    // cargo/vitest put it. The success-path block message must build its
    // preview from combined output, not stdout alone.
    let stdout = "line\n".repeat(300);
    let stderr = "test result: FAILED. 3 passed; 2 failed; 0 ignored\n";
    let payload = make_payload(&stdout, stderr, false, false, false);

    let output = run_intercept(payload, 8192);
    let json: serde_json::Value = serde_json::from_slice(&output).expect("valid JSON");
    let block_stdout =
        json["hookSpecificOutput"]["updatedToolOutput"]["stdout"].as_str().expect("stdout string");

    assert!(
        block_stdout.contains("2 failed"),
        "success-path preview should surface stderr content: {block_stdout}"
    );
}

#[test]
fn failure_preview_detects_ansi_colored_error_and_strips_escapes() {
    // cargo colors diagnostics (`\x1b[…merror[E0308]\x1b[0m`). Preview
    // extraction must run on ANSI-stripped text: raw text defeats the
    // `error[E` prefix match and leaks ESC bytes into the handle message.
    let filler = "   Compiling foo v0.1.0\n".repeat(300);
    let colored = "\u{1b}[1m\u{1b}[38;5;9merror[E0308]\u{1b}[0m: mismatched types\n";
    let payload = PostToolUsePayload {
        tool_name: Some("Bash".into()),
        tool_response: ToolResponse {
            stdout: Some(format!("{filler}{colored}")),
            stderr: Some(String::new()),
            interrupted: Some(false),
            is_image: Some(false),
            no_output_expected: Some(false),
            exit_code: Some(101),
        },
    };

    let output = run_intercept(payload, 8192);
    let json: serde_json::Value = serde_json::from_slice(&output).expect("valid JSON");
    let block_stdout =
        json["hookSpecificOutput"]["updatedToolOutput"]["stdout"].as_str().expect("stdout string");

    assert!(
        block_stdout.contains("error[E0308]"),
        "preview should detect the colored diagnostic: {block_stdout}"
    );
    assert!(
        !block_stdout.contains('\u{1b}'),
        "handle message must not leak ANSI escapes: {block_stdout}"
    );
}
