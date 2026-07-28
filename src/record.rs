use rusqlite::Connection;

// ---------------------------------------------------------------------------
// Internal structs
// ---------------------------------------------------------------------------

/// Deserialised PostToolUse hook payload for recording purposes.
#[derive(serde::Deserialize)]
struct RecordPayload {
    tool_name: String,
    tool_use_id: Option<String>,
    tool_input: Option<serde_json::Value>,
    tool_response: Option<RecordToolResponse>,
    /// Working directory — may be present at the top level of the hook payload.
    cwd: Option<String>,
}

#[derive(serde::Deserialize)]
struct RecordToolResponse {
    /// String content returned by the tool (e.g. file body for Read/Glob).
    /// Absent for Edit, Write, and other write-only tools.
    content: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Record a PostToolUse call into the `calls` table.
///
/// Reads a PostToolUse JSON payload from `input`, and inserts a row into
/// `calls` with `intercepted=false` and `handle_id=NULL`.
///
/// # Silent failure handling
///
/// If `input` cannot be parsed (missing required fields, invalid JSON), the
/// function logs a warning to stderr and returns `Ok(())` — recording failures
/// are non-fatal.
///
/// # Busy timeout
///
/// Sets `PRAGMA busy_timeout=5000` on `conn` before any write.  This ensures
/// concurrent `BEGIN IMMEDIATE` transactions queue rather than error immediately.
/// Propagates `SQLITE_BUSY` as `Err` if the timeout expires, which the caller
/// (`main.rs`) converts to `exit 0`.
// `eprintln!` is intentional here — the binary logs to stderr so Claude Code
// sees warnings without affecting the hook output protocol on stdout.
// Same pattern as intercept.rs and store.rs.
#[allow(clippy::print_stderr)]
pub fn record(conn: &mut Connection, input: &str) -> Result<(), crate::CtxlError> {
    // Set busy timeout before any DB operations so concurrent writes queue.
    // Defaults to 5000 ms (5 s).  Override via CTXL_BUSY_TIMEOUT_MS for
    // testing to avoid 5-second delays in test suites.
    let timeout_ms = std::env::var("CTXL_BUSY_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(5000);
    conn.pragma_update(None, "busy_timeout", timeout_ms)?;

    // Parse payload — malformed input is handled silently.
    let payload: RecordPayload = match serde_json::from_str(input) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[ctxl] warn: failed to parse PostToolUse payload: {e}");
            return Ok(());
        }
    };

    let tool = &payload.tool_name;
    let tool_use_id = payload.tool_use_id.as_deref();
    let params = payload.tool_input.as_ref().map(|v| v.to_string());
    let cwd = payload.cwd.as_deref();

    // Extract string content for line_count and token_est.
    // Only tools that return a content string (Read, Glob) populate these.
    // Edit, Write, and other write-only tools have no content → NULL.
    let content_str: Option<String> =
        payload.tool_response.as_ref().and_then(|r| r.content.as_ref()).and_then(|c| match c {
            serde_json::Value::String(s) => Some(s.clone()),
            _ => None,
        });

    let line_count: Option<i64> = content_str.as_ref().map(|c| c.lines().count() as i64);
    let token_est: Option<i64> =
        content_str.as_ref().map(|c| tokenx_rs::estimate_token_count(c) as i64);

    let created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    // BEGIN IMMEDIATE for WAL-mode concurrent write serialisation.
    // A second concurrent writer will wait up to busy_timeout for the lock.
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    tx.execute(
        "INSERT INTO calls \
         (tool, tool_use_id, params, cwd, intercepted, handle_id, line_count, token_est, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?7, ?8)",
        rusqlite::params![
            tool,
            tool_use_id,
            params,
            cwd,
            false,
            line_count,
            token_est,
            created_at
        ],
    )?;
    tx.commit()?;

    crate::debug::debug_log(&format!(
        "[record] tool={tool} tool_use_id={tool_use_id:?} intercepted=false"
    ));

    Ok(())
}
