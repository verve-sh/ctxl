use rusqlite::Connection;
use std::io::Write;

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Configuration for the Grep PostToolUse intercept command.
#[derive(Debug, Clone)]
pub struct GrepInterceptConfig {
    /// Maximum Grep output lines before interception.  Default: 200.
    pub threshold: usize,
    /// When true, record an `insert_calls_row` after interception.
    /// The router sets this to `false` when norecord is active.
    pub record: bool,
    /// Serialized tool_input JSON from the payload.
    pub tool_input: Option<String>,
    /// Working directory from the payload.
    pub cwd: Option<String>,
    /// Tool use ID from the payload.
    pub tool_use_id: Option<String>,
}

impl GrepInterceptConfig {
    /// Build config from environment variables with compiled-in defaults.
    pub fn from_env() -> Self {
        Self {
            threshold: crate::env_parse("CTXL_GREP_THRESHOLD", 200),
            record: true,
            tool_input: None,
            cwd: None,
            tool_use_id: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Response types — Grep tool_response shape
// ---------------------------------------------------------------------------

/// GrepOutput shape as returned by the Grep PostToolUse hook.
#[derive(serde::Deserialize, Debug)]
pub struct GrepToolResponse {
    /// The raw match output (in `content` / `files_with_matches` / `count` format).
    pub content: Option<String>,
    #[serde(rename = "numFiles")]
    pub num_files: Option<u64>,
    pub filenames: Option<Vec<String>>,
    #[serde(rename = "numMatches")]
    pub num_matches: Option<u64>,
    pub mode: Option<String>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Core Grep PostToolUse intercept logic.
///
/// Processes a deserialized PostToolUse payload.  If passthrough conditions
/// are met, `writer` remains empty.  Otherwise the **full** content is stored
/// via [`crate::store::write`] and a `hookSpecificOutput` envelope matching
/// the GrepOutput shape is written to `writer`.  Truncation happens only at
/// display time (in `retrieve.rs`), not at storage time — matching the Bash
/// handler's behavior.
///
/// A `calls` row with `intercepted=true` and the handle ID is inserted into
/// the session DB whenever the output is intercepted.
///
/// # Passthrough conditions
///
/// 1. `content` is absent or empty
/// 2. `content` line count <= `config.threshold`
#[allow(clippy::print_stderr)] // Intentional: CLI diagnostic to stderr
pub fn run<W: Write>(
    payload: crate::payload::PostToolUsePayload<GrepToolResponse>,
    writer: &mut W,
    config: &GrepInterceptConfig,
    conn: &Connection,
) -> Result<(), crate::CtxlError> {
    let resp = &payload.tool_response;

    let content_str = resp.content.as_deref().unwrap_or("");
    let mode = resp.mode.as_deref().unwrap_or("content");
    let num_files = resp.num_files.unwrap_or(0);
    let filenames = resp.filenames.clone().unwrap_or_default();
    let num_matches = resp.num_matches.unwrap_or(0);

    // 1. Empty content — passthrough.
    if content_str.is_empty() {
        return Ok(());
    }

    let line_count = content_str.lines().count();

    // 2. Below or at threshold — passthrough.
    if line_count <= config.threshold {
        return Ok(());
    }

    // --- Interception path ---

    // Store full content (truncation happens at display time in retrieve.rs).
    let tool = match payload.tool_name.as_deref() {
        Some(t) => t,
        None => {
            eprintln!("[ctxl] warn: tool_name absent, defaulting to Grep");
            "Grep"
        }
    };

    let mut store_payload = serde_json::json!({
        "tool": tool,
        "output_mode": mode,
        "content": content_str,
    });
    if let Some(ti) = &config.tool_input {
        crate::payload::set_tool_input(&mut store_payload, ti);
    }

    let handle_id = crate::store::write(conn, store_payload)?;

    // Cache write-through (fail-open — session handle already stored above).
    crate::intercept::try_cache_write_through(
        conn,
        &handle_id,
        content_str,
        tool,
        mode,
        config.tool_input.as_deref(),
    );

    // Content mode: apply SimHash dedup compression.
    if mode == "content" {
        let deduped = crate::compress::grep_dedup::compress(content_str);
        if !deduped.is_empty() && deduped.len() < content_str.len() {
            let _ = conn.execute(
                "UPDATE handles SET compressed_body = ?1, compressed_method = ?2 WHERE id = ?3",
                rusqlite::params![deduped, "grep_dedup", &handle_id],
            );
        }
    }

    // Correct num_matches when payload reports 0 but content has lines.
    let num_matches_display =
        if num_matches == 0 && line_count > 0 { line_count as u64 } else { num_matches };

    // Generate mode-aware block message (preview only — full content is in DB).
    let block_msg = crate::compress::grep_preview::grep_preview_ex(
        content_str,
        mode,
        &handle_id,
        num_matches_display,
        &filenames,
        Some(line_count),
        Some(line_count),
    );

    // Insert a calls row with intercepted=true (gated by config.record).
    if config.record {
        crate::calls::insert_calls_row(
            conn,
            &crate::calls::InterceptCallMeta {
                tool,
                handle_id: &handle_id,
                line_count: line_count as i64,
                token_est: Some(crate::store::token_estimate(content_str)),
                tool_input: config.tool_input.as_deref(),
                cwd: config.cwd.as_deref(),
                tool_use_id: config.tool_use_id.as_deref(),
                exit_code: None,
            },
        )?;
    }

    // Emit the hookSpecificOutput envelope matching GrepOutput shape.
    let envelope = serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "updatedToolOutput": {
                "content": block_msg,
                "numFiles": num_files,
                "filenames": filenames,
                "numMatches": num_matches,
                "mode": mode,
            }
        }
    });

    write!(writer, "{}", envelope)?;
    Ok(())
}
