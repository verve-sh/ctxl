use rusqlite::Connection;

// ---------------------------------------------------------------------------
// OutputMode
// ---------------------------------------------------------------------------

/// Which stream(s) to extract from a stored payload.
///
/// Serialises to/from the lowercase strings `"stdout"`, `"stderr"`, `"mixed"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    Stdout,
    Stderr,
    Mixed,
}

impl std::fmt::Display for OutputMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OutputMode::Stdout => write!(f, "stdout"),
            OutputMode::Stderr => write!(f, "stderr"),
            OutputMode::Mixed => write!(f, "mixed"),
        }
    }
}

impl std::str::FromStr for OutputMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "stdout" => Ok(OutputMode::Stdout),
            "stderr" => Ok(OutputMode::Stderr),
            "mixed" => Ok(OutputMode::Mixed),
            other => Err(format!("unknown output mode: {other}")),
        }
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

#[allow(clippy::manual_div_ceil)]
fn gen_random_hex(len: usize) -> Result<String, crate::CtxlError> {
    let mut buf = vec![0u8; (len + 1) / 2];
    getrandom::getrandom(&mut buf).map_err(|e| std::io::Error::other(e.to_string()))?;
    let hex: String = buf.iter().map(|b| format!("{:02x}", b)).collect();
    Ok(hex[..len.min(hex.len())].to_string())
}

fn is_unique_violation(e: &rusqlite::Error) -> bool {
    matches!(
        e,
        rusqlite::Error::SqliteFailure(f, _)
            if f.extended_code == 2067  // SQLITE_CONSTRAINT_UNIQUE
            || f.extended_code == 1555  // SQLITE_CONSTRAINT_PRIMARYKEY
    )
}

/// Derive the content string from the payload.
///
/// Priority:
/// 1. `content` field (raw)
/// 2. `stdout` / `stderr` based on `output_mode`
fn extract_content(payload: &serde_json::Value) -> String {
    if let Some(c) = payload.get("content").and_then(|v| v.as_str()) {
        return c.to_string();
    }
    let mode_str = payload.get("output_mode").and_then(|v| v.as_str()).unwrap_or("stdout");
    let mode: OutputMode = mode_str.parse().unwrap_or(OutputMode::Stdout);
    match mode {
        OutputMode::Stderr => {
            payload.get("stderr").and_then(|v| v.as_str()).unwrap_or("").to_string()
        }
        OutputMode::Mixed => {
            let out = payload.get("stdout").and_then(|v| v.as_str()).unwrap_or("");
            let err = payload.get("stderr").and_then(|v| v.as_str()).unwrap_or("");
            if !out.is_empty() && !err.is_empty() && !out.ends_with('\n') {
                format!("{out}\n{err}")
            } else {
                format!("{out}{err}")
            }
        }
        OutputMode::Stdout => {
            payload.get("stdout").and_then(|v| v.as_str()).unwrap_or("").to_string()
        }
    }
}

#[allow(clippy::print_stderr)] // Intentional: CLI warning to stderr for missing tool field
fn try_insert_raw(
    conn: &Connection,
    id: &str,
    payload: &serde_json::Value,
) -> Result<(), rusqlite::Error> {
    let tool = match payload.get("tool").and_then(|v| v.as_str()) {
        Some(t) => t,
        None => {
            eprintln!("[ctxl] warn: tool field absent in payload, defaulting to Bash");
            "Bash"
        }
    };
    let output_mode = payload.get("output_mode").and_then(|v| v.as_str()).unwrap_or("stdout");
    let cwd = payload.get("cwd").and_then(|v| v.as_str()).unwrap_or("");
    let content = extract_content(payload);
    let line_count = content.lines().count() as i64;
    let tok_est = token_estimate(&content);
    let truncated = payload.get("truncated").and_then(|v| v.as_bool()).unwrap_or(false) as i32;
    let tool_input = payload.get("tool_input").map(|v| v.to_string());
    let params = match payload.as_object() {
        Some(obj) => {
            let filtered: serde_json::Map<String, serde_json::Value> = obj
                .iter()
                .filter(|(k, _)| {
                    !matches!(k.as_str(), "stdout" | "stderr" | "content" | "tool_input")
                })
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            serde_json::Value::Object(filtered).to_string()
        }
        None => payload.to_string(),
    };
    // Safety: duration_since(UNIX_EPOCH) only fails if the system clock is
    // before 1970 — a broken-system invariant, not a recoverable condition.
    // This function returns rusqlite::Error (not CtxlError), so ? propagation
    // would require changing the return type and all match-arm callers.
    #[allow(clippy::expect_used)]
    let created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock precedes Unix epoch")
        .as_secs() as i64;

    conn.execute(
        "INSERT INTO handles \
         (id, tool, output_mode, params, cwd, tool_input, content, line_count, token_est, truncated, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        rusqlite::params![
            id,
            tool,
            output_mode,
            params,
            cwd,
            tool_input,
            content,
            line_count,
            tok_est,
            truncated,
            created_at
        ],
    )
    .map(|_| ())
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Write a payload to the handle store and return the generated handle ID.
///
/// The handle ID format is `<prefix>[0-9a-f]{6}`, where `<prefix>` is derived
/// from the `tool` field in `payload` (see [`derive_handle_prefix`]):
/// `b_` for Bash (and any unrecognized tool — backward-compatible default),
/// `g_` for Grep, and `i_` for index.  On a UNIQUE collision the function
/// retries once with a 7-character suffix.  A second collision returns
/// `Err("handle ID collision after retry")`.
pub fn write(conn: &Connection, payload: serde_json::Value) -> Result<String, crate::CtxlError> {
    write_with_id_gen(conn, payload, gen_random_hex)
}

/// Derive the handle ID prefix from the `tool` field in a payload.
///
/// - `"Grep"` → `"g_"` (distinguishes Grep handles from Bash handles)
/// - anything else → `"b_"` (backward-compatible default for Bash)
pub fn derive_handle_prefix(payload: &serde_json::Value) -> &'static str {
    match payload.get("tool").and_then(|v| v.as_str()) {
        Some("Grep") => "g_",
        Some("ctxl-index") => "i_",
        _ => "b_",
    }
}

/// Like [`write`] but accepts a custom ID generator for testing.
///
/// `gen(len)` must return a lowercase hex string of exactly `len` characters.
/// First call uses `len = 6`; on collision, `len = 7`.
///
/// The handle ID prefix is derived from the `tool` field in `payload` via
/// [`derive_handle_prefix`]: `"Grep"` produces `g_XXXXXX`, and all other
/// tools produce `b_XXXXXX`.
pub fn write_with_id_gen<F>(
    conn: &Connection,
    payload: serde_json::Value,
    mut gen: F,
) -> Result<String, crate::CtxlError>
where
    F: FnMut(usize) -> Result<String, crate::CtxlError>,
{
    let prefix = derive_handle_prefix(&payload);

    // First attempt: 6-char hex
    let candidate = format!("{}{}", prefix, gen(6)?);
    match try_insert_raw(conn, &candidate, &payload) {
        Ok(()) => return Ok(candidate),
        Err(e) if is_unique_violation(&e) => {} // fall through to retry
        Err(e) => return Err(crate::CtxlError::Sqlite(e)),
    }

    // Retry: 7-char hex
    let candidate2 = format!("{}{}", prefix, gen(7)?);
    match try_insert_raw(conn, &candidate2, &payload) {
        Ok(()) => Ok(candidate2),
        Err(e) if is_unique_violation(&e) => Err(crate::CtxlError::HandleCollision),
        Err(e) => Err(crate::CtxlError::Sqlite(e)),
    }
}

/// Estimate token count for `content` using tokenx-rs heuristic estimation.
///
/// Returns the estimate as `i64` for direct insertion into the `handles.token_est`
/// column. Uses a character-level heuristic (no vocabulary files required).
pub fn token_estimate(content: &str) -> i64 {
    tokenx_rs::estimate_token_count(content) as i64
}
