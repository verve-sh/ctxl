use rusqlite::{Connection, OptionalExtension};
use std::path::PathBuf;

/// Sentinel markers for FTS5 highlight() — non-printable control chars
/// that will never appear in real content, used to identify matched lines.
const MATCH_START: &str = "\x01";
const MATCH_END: &str = "\x02";

use crate::{detect_regex_metacharacters, sanitize_fts5_query};

pub struct ShowOpts {
    pub head: usize,
    pub tail: Option<usize>,
    /// Number of lines to skip before applying head/tail bounds.
    pub offset: usize,
    /// When true, return `compressed_body` instead of raw `content`.
    /// Falls back to `content` with a note when `compressed_body` is NULL.
    pub compressed: bool,
}

impl Default for ShowOpts {
    fn default() -> Self {
        ShowOpts { head: 80, tail: None, offset: 0, compressed: false }
    }
}

// ---------------------------------------------------------------------------
// Global blob resolution (ATTACH+JOIN fallback)
// ---------------------------------------------------------------------------

/// Resolve content from the global DB when a session handle has `content=NULL`
/// and a `blob_id` pointing to the cross-session cache.
///
/// Returns `None` when:
/// - `blob_id` is absent from the session handle
/// - The global DB path cannot be determined
/// - The global DB cannot be opened (fail-open)
/// - The blob is not found in the global DB
///
/// This is called by `show`, `search`, `show_filtered`, and `files` whenever
/// the session DB returns `content = NULL`.
fn resolve_global_content(conn: &Connection, handle: &str) -> Option<String> {
    // Fetch blob_id from the session handle.
    let blob_id: Option<i64> = conn
        .query_row("SELECT blob_id FROM handles WHERE id=?1", [handle], |row| row.get(0))
        .optional()
        .ok()
        .flatten()
        .flatten();

    let blob_id = blob_id?;

    // Locate the global DB.
    let db_path: PathBuf = crate::global_db::global_db_path()?;

    // Open the global DB directly (no ATTACH — avoids SQLite ATTACH
    // permission issues in sandboxed environments).
    let gconn = crate::global_db::open_global_db(&db_path).ok()?;

    // Touch last_used so the blob isn't cleaned up while it's being used.
    let _ = crate::global_db::touch_blob(&gconn, blob_id);

    crate::global_db::fetch_blob_content(&gconn, blob_id).ok().flatten()
}

// ---------------------------------------------------------------------------
// show
// ---------------------------------------------------------------------------

/// Return the stored content for `handle`, bounded to `opts.head` lines.
///
/// When `opts.compressed` is true, returns `compressed_body` instead of raw
/// `content`. Falls back to `content` with a note when `compressed_body` is NULL.
///
/// Returns `Err("handle not found: <id>")` when the handle is absent from
/// the database.
pub fn show(conn: &Connection, handle: &str, opts: ShowOpts) -> Result<String, crate::CtxlError> {
    if opts.compressed {
        let result: Option<(Option<String>, Option<String>)> = conn
            .query_row(
                "SELECT compressed_body, content FROM handles WHERE id=?1",
                [handle],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        return match result {
            None => Err(crate::CtxlError::HandleNotFound(handle.to_string())),
            Some((Some(compressed), _)) => {
                let effective = apply_offset(&compressed, opts.offset);
                if opts.offset > 0 {
                    // Manual pagination: clean head slice, no head+tail split
                    let lines: Vec<&str> = effective.lines().take(opts.head).collect();
                    let mut result = lines.join("\n");
                    if !result.is_empty() {
                        result.push('\n');
                    }
                    Ok(result)
                } else if let Some(n) = opts.tail {
                    Ok(tail_lines(&effective, n))
                } else {
                    Ok(bound_lines(&effective, opts.head))
                }
            }
            Some((None, content_opt)) => {
                // No compressed_body — fall back to raw content with a note.
                let note =
                    "(No structural compression available for this handle — showing raw content)\n";
                // For cache-linked handles (content=NULL, blob_id set), resolve
                // content from the global DB — mirrors the non-compressed path.
                let content = match content_opt {
                    Some(c) => c,
                    None => resolve_global_content(conn, handle).unwrap_or_default(),
                };
                if content.is_empty() {
                    return Ok(note.to_string());
                }
                let effective = apply_offset(&content, opts.offset);
                let bounded = if opts.offset > 0 {
                    // Manual pagination: clean head slice, no head+tail split
                    let lines: Vec<&str> = effective.lines().take(opts.head).collect();
                    let mut r = lines.join("\n");
                    if !r.is_empty() {
                        r.push('\n');
                    }
                    r
                } else if let Some(n) = opts.tail {
                    tail_lines(&effective, n)
                } else {
                    bound_lines(&effective, opts.head)
                };
                Ok(format!("{note}{bounded}"))
            }
        };
    }

    let result: Option<Option<String>> = conn
        .query_row("SELECT content FROM handles WHERE id=?1", [handle], |row| row.get(0))
        .optional()?;

    match result {
        None => Err(crate::CtxlError::HandleNotFound(handle.to_string())),
        Some(None) => {
            // content IS NULL — attempt global blob resolution.
            let content = resolve_global_content(conn, handle).unwrap_or_default();
            if content.is_empty() {
                return Ok(String::new());
            }
            let effective = apply_offset(&content, opts.offset);
            if opts.offset > 0 {
                // Manual pagination: clean head slice, no head+tail split
                let lines: Vec<&str> = effective.lines().take(opts.head).collect();
                let mut result = lines.join("\n");
                if !result.is_empty() {
                    result.push('\n');
                }
                Ok(result)
            } else if let Some(n) = opts.tail {
                Ok(tail_lines(&effective, n))
            } else {
                Ok(bound_lines(&effective, opts.head))
            }
        }
        Some(Some(content)) => {
            let effective = apply_offset(&content, opts.offset);
            if opts.offset > 0 {
                // Manual pagination: clean head slice, no head+tail split
                let lines: Vec<&str> = effective.lines().take(opts.head).collect();
                let mut result = lines.join("\n");
                if !result.is_empty() {
                    result.push('\n');
                }
                Ok(result)
            } else if let Some(n) = opts.tail {
                Ok(tail_lines(&effective, n))
            } else {
                Ok(bound_lines(&effective, opts.head))
            }
        }
    }
}

/// Skip the first `offset` lines from `text`, preserving trailing newline.
///
/// Returns the full text unchanged when `offset` is 0, or an empty string
/// when `offset` exceeds the line count.
fn apply_offset(text: &str, offset: usize) -> String {
    if offset == 0 {
        return text.to_string();
    }
    let lines: Vec<&str> = text.lines().collect();
    if offset >= lines.len() {
        return String::new();
    }
    let joined = lines[offset..].join("\n");
    if text.ends_with('\n') {
        joined + "\n"
    } else {
        joined
    }
}

/// When `offset > 0`, return a simple head slice (no head+tail split).
/// When `offset == 0`, delegate to `bound_lines` for the head+tail strategy.
///
/// This prevents `bound_lines` from stealing lines for its tail section when
/// the user is manually paginating with `--offset`, which would cause
/// consecutive offset windows to not tile cleanly.
fn offset_aware_bound(text: &str, max_lines: usize, offset: usize) -> String {
    if offset > 0 {
        let lines: Vec<&str> = text.lines().take(max_lines).collect();
        let mut result = lines.join("\n");
        if !result.is_empty() {
            result.push('\n');
        }
        result
    } else {
        bound_lines(text, max_lines)
    }
}

/// Truncate `text` to at most `max_lines` lines using a head+tail strategy.
///
/// When truncation is needed, shows the first `max_lines - TAIL_LINES` lines,
/// a separator, and the last `TAIL_LINES` lines. This captures both initial
/// context (headers, compilation messages) and final summaries (test results,
/// exit status) without requiring `--head 200`.
fn bound_lines(text: &str, max_lines: usize) -> String {
    /// Lines reserved for the tail section when truncating.
    const TAIL_LINES: usize = 10;

    let all_lines: Vec<&str> = text.lines().collect();
    let total = all_lines.len();

    if total <= max_lines {
        // No truncation needed — return as-is with trailing newline preserved.
        let mut result = all_lines.join("\n");
        if text.ends_with('\n') {
            result.push('\n');
        }
        return result;
    }

    // Not enough room for a meaningful head+tail split — just take head.
    if max_lines <= TAIL_LINES + 2 {
        let mut result = all_lines[..max_lines].join("\n");
        result.push('\n');
        result.push_str(&format!("(showing {max_lines} of {total} lines)\n"));
        return result;
    }

    let head_count = max_lines - TAIL_LINES - 1; // -1 for separator line
    let omitted = total - head_count - TAIL_LINES;

    let mut parts: Vec<&str> = Vec::with_capacity(max_lines + 1);
    parts.extend_from_slice(&all_lines[..head_count]);

    let sep = format!("... ({omitted} lines omitted) ...");
    parts.push(&sep);

    parts.extend_from_slice(&all_lines[total - TAIL_LINES..]);

    let mut result = parts.join("\n");
    result.push('\n');
    result.push_str(&format!("(showing {max_lines} of {total} lines)\n"));
    result
}

/// Return the last `max_lines` lines from `text`.
///
/// When the text has fewer than `max_lines` lines, returns the full content.
fn tail_lines(text: &str, max_lines: usize) -> String {
    let all_lines: Vec<&str> = text.lines().collect();
    let total = all_lines.len();

    if total <= max_lines {
        let mut result = all_lines.join("\n");
        if text.ends_with('\n') {
            result.push('\n');
        }
        return result;
    }

    let start = total - max_lines;
    let mut result = all_lines[start..].join("\n");
    result.push('\n');
    result.push_str(&format!("(showing last {max_lines} of {total} lines)\n"));
    result
}

// ---------------------------------------------------------------------------
// search
// ---------------------------------------------------------------------------

/// Return lines from the stored content of `handle` that match `query`,
/// bounded to `limit` results.
///
/// Uses a two-tier strategy:
/// 1. **FTS5 highlight** (fast path) — token-boundary matching via `highlight()`.
/// 2. **Substring fallback** — catches partial-token matches (e.g., `"foo"` inside
///    `test_foo_bar` where FTS5 treats the identifier as a single token).
///
/// Returns `Err("handle not found: <id>")` when the handle is absent.
pub fn search(
    conn: &Connection,
    handle: &str,
    query: &str,
    limit: usize,
) -> Result<Vec<String>, crate::CtxlError> {
    if query.trim().is_empty() {
        return Err(crate::CtxlError::EmptyQuery);
    }

    // Guard: reject regex metacharacters before querying FTS5
    if let Some(msg) = detect_regex_metacharacters(query) {
        return Err(crate::CtxlError::RegexInQuery(format!("error: {msg}")));
    }

    // Retrieve rowid + content for the handle
    let result: Option<(i64, Option<String>)> = conn
        .query_row("SELECT rowid, content FROM handles WHERE id=?1", [handle], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()?;

    let (rowid, content_opt) =
        result.ok_or_else(|| crate::CtxlError::HandleNotFound(handle.to_string()))?;

    // When content IS NULL, attempt global blob resolution before giving up.
    let content = match content_opt {
        Some(c) => c,
        None => resolve_global_content(conn, handle).unwrap_or_default(),
    };

    if content.is_empty() {
        return Ok(vec![]);
    }

    let fts_query = sanitize_fts5_query(query);

    // Tier 1: FTS5 highlight — fast path (token-boundary matching)
    let mut results: Vec<String> = match conn.query_row(
        "SELECT highlight(content_fts, 0, ?1, ?2) \
         FROM content_fts \
         WHERE rowid=?3 AND content_fts MATCH ?4",
        rusqlite::params![MATCH_START, MATCH_END, rowid, fts_query],
        |row| row.get::<_, String>(0),
    ) {
        Ok(highlighted) => highlighted
            .lines()
            .enumerate()
            .filter(|(_, line)| line.contains(MATCH_START))
            .take(limit)
            .map(|(i, line)| {
                let clean = line.replace(MATCH_START, "").replace(MATCH_END, "");
                format!("{}:{}", i + 1, clean)
            })
            .collect(),
        Err(rusqlite::Error::QueryReturnedNoRows) => Vec::new(),
        Err(e) => {
            eprintln!("[ctxl] fts5 highlight error (falling back to substring): {e:?}");
            Vec::new()
        }
    };

    // Tier 2: Substring fallback — catches partial-token matches
    // (e.g., "foo" in "test_foo_bar" where FTS5 treats test_foo_bar as single token)
    if results.is_empty() {
        let q_lower = query.to_lowercase();
        results = content
            .lines()
            .enumerate()
            .filter(|(_, line)| line.to_lowercase().contains(&q_lower))
            .take(limit)
            .map(|(i, line)| format!("{}:{}", i + 1, line))
            .collect();
    }

    Ok(results)
}

// ---------------------------------------------------------------------------
// search_all — cross-handle FTS5 search
// ---------------------------------------------------------------------------

/// A single match from cross-handle search.
#[derive(Debug)]
pub struct CrossHandleMatch {
    pub handle_id: String,
    pub tool: String,
    pub line: String,
}

/// Search across all handles in the session DB using FTS5 highlight.
///
/// Returns up to `limit` matching lines, grouped by handle. Handles are
/// ranked by BM25 relevance with recency as tiebreaker. At most 10 handles
/// are scanned.
///
/// When FTS5 returns zero results, falls back to substring matching on
/// the most recent 10 handles — this catches partial-token matches that
/// FTS5 tokenization misses (e.g., `"foo"` inside `test_foo_bar`).
pub fn search_all(
    conn: &Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<CrossHandleMatch>, crate::CtxlError> {
    if query.trim().is_empty() {
        return Err(crate::CtxlError::EmptyQuery);
    }

    // Guard: reject regex metacharacters before querying FTS5
    if let Some(msg) = detect_regex_metacharacters(query) {
        return Err(crate::CtxlError::RegexInQuery(format!("error: {msg}")));
    }

    let fts_query = sanitize_fts5_query(query);

    let mut stmt = conn.prepare(
        "SELECT h.id, h.tool, highlight(content_fts, 0, ?2, ?3) \
         FROM content_fts \
         JOIN handles h ON content_fts.rowid = h.rowid \
         WHERE content_fts MATCH ?1 \
         ORDER BY bm25(content_fts), h.created_at DESC \
         LIMIT 10",
    )?;

    let mut results = Vec::new();

    let rows = stmt.query_map(rusqlite::params![fts_query, MATCH_START, MATCH_END], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?))
    })?;

    for row in rows {
        let (handle_id, tool, content_opt) = row?;
        let content = content_opt.unwrap_or_default();

        for (i, line) in content.lines().enumerate() {
            if results.len() >= limit {
                break;
            }
            if line.contains(MATCH_START) {
                results.push(CrossHandleMatch {
                    handle_id: handle_id.clone(),
                    tool: tool.clone(),
                    line: format!(
                        "{}:{}",
                        i + 1,
                        line.replace(MATCH_START, "").replace(MATCH_END, "")
                    ),
                });
            }
        }

        if results.len() >= limit {
            break;
        }
    }

    // Tier 2: Substring fallback — scan recent handles when FTS5 found nothing.
    if results.is_empty() {
        let q_lower = query.to_lowercase();
        let mut fallback_stmt = conn.prepare(
            "SELECT id, tool, content FROM handles \
             ORDER BY created_at DESC \
             LIMIT 10",
        )?;

        let fallback_rows = fallback_stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?;

        for row in fallback_rows {
            let (handle_id, tool, content_opt) = row?;
            // For NULL-content handles, try to resolve from global DB.
            let content = match content_opt {
                Some(c) => c,
                None => resolve_global_content(conn, &handle_id).unwrap_or_default(),
            };

            for (i, line) in content.lines().enumerate() {
                if results.len() >= limit {
                    break;
                }
                if line.to_lowercase().contains(&q_lower) {
                    results.push(CrossHandleMatch {
                        handle_id: handle_id.clone(),
                        tool: tool.clone(),
                        line: format!("{}:{}", i + 1, line),
                    });
                }
            }

            if results.len() >= limit {
                break;
            }
        }
    }

    Ok(results)
}

/// Format cross-handle search results grouped by handle.
pub fn format_search_all(matches: &[CrossHandleMatch]) -> String {
    if matches.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    let mut current_handle: Option<&str> = None;

    for m in matches {
        if current_handle != Some(&m.handle_id) {
            if current_handle.is_some() {
                out.push('\n');
            }
            out.push_str(&format!("--- {} ({}) ---\n", m.handle_id, m.tool));
            current_handle = Some(&m.handle_id);
        }
        out.push_str(&m.line);
        out.push('\n');
    }

    out
}

// ---------------------------------------------------------------------------
// inspect
// ---------------------------------------------------------------------------

/// Metadata about a stored handle, returned by [`inspect`].
#[derive(Debug)]
pub struct InspectResult {
    pub id: String,
    pub tool: String,
    pub output_mode: String,
    pub line_count: Option<i64>,
    pub token_est: Option<i64>,
    pub truncated: bool,
    pub compressed_method: Option<String>,
    pub cwd: Option<String>,
    pub tool_input: Option<String>,
    pub created_at: i64,
    pub retrieval_count: i64,
    pub last_retrieved_at: Option<i64>,
}

/// Return metadata for a stored handle without retrieving content.
pub fn inspect(conn: &Connection, handle: &str) -> Result<InspectResult, crate::CtxlError> {
    let result = conn
        .query_row(
            "SELECT id, tool, output_mode, line_count, token_est, truncated, \
             compressed_method, cwd, tool_input, created_at, \
             retrieval_count, last_retrieved_at \
             FROM handles WHERE id = ?1",
            [handle],
            |row| {
                Ok(InspectResult {
                    id: row.get(0)?,
                    tool: row.get(1)?,
                    output_mode: row.get(2)?,
                    line_count: row.get(3)?,
                    token_est: row.get(4)?,
                    truncated: row.get::<_, i32>(5).map(|v| v != 0)?,
                    compressed_method: row.get(6)?,
                    cwd: row.get(7)?,
                    tool_input: row.get(8)?,
                    created_at: row.get(9)?,
                    retrieval_count: row.get(10)?,
                    last_retrieved_at: row.get(11)?,
                })
            },
        )
        .optional()?;

    result.ok_or_else(|| crate::CtxlError::HandleNotFound(handle.to_string()))
}

// ---------------------------------------------------------------------------
// handles listing
// ---------------------------------------------------------------------------

/// Summary of a stored handle, returned by [`list_handles`].
#[derive(Debug)]
pub struct HandleEntry {
    pub id: String,
    pub tool: String,
    pub line_count: Option<i64>,
    pub token_est: Option<i64>,
    pub created_at: i64,
    pub retrieval_count: i64,
}

/// List handles in the session, most recent first.
pub fn list_handles(conn: &Connection, limit: usize) -> Result<Vec<HandleEntry>, crate::CtxlError> {
    let mut stmt = conn.prepare(
        "SELECT id, tool, line_count, token_est, created_at, retrieval_count \
         FROM handles ORDER BY created_at DESC LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![limit as i64], |row| {
            Ok(HandleEntry {
                id: row.get(0)?,
                tool: row.get(1)?,
                line_count: row.get(2)?,
                token_est: row.get(3)?,
                created_at: row.get(4)?,
                retrieval_count: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ---------------------------------------------------------------------------
// calls
// ---------------------------------------------------------------------------

/// Options for the `calls` query.
pub struct CallsOpts {
    /// Maximum number of rows to return (default 20).
    pub last: usize,
    /// Filter by tool name (None = all tools).
    pub tool: Option<String>,
    /// Filter by intercepted flag (None = all, Some(true) = only intercepted).
    pub intercepted: Option<bool>,
}

impl Default for CallsOpts {
    fn default() -> Self {
        CallsOpts { last: 20, tool: None, intercepted: None }
    }
}

/// Return bounded call history from the `calls` table, ordered by `id DESC`.
///
/// Each returned string is a JSON object representing one row.  Rows are
/// ordered newest-first (`id DESC`), bounded to `opts.last` entries.  When
/// `opts.tool` is `Some`, only rows with a matching `tool` column are returned.
pub fn calls(conn: &Connection, opts: CallsOpts) -> Result<Vec<String>, crate::CtxlError> {
    // A single parameterised query handles both the filtered and unfiltered
    // cases: when `?1 IS NULL` the tool filter is a no-op.
    let mut stmt = conn.prepare(
        "SELECT id, tool, tool_use_id, params, cwd, intercepted, handle_id, \
         line_count, token_est, duration_ms, exit_code, created_at \
         FROM calls \
         WHERE (?1 IS NULL OR tool = ?1) \
         AND (?3 IS NULL OR intercepted = ?3) \
         ORDER BY id DESC \
         LIMIT ?2",
    )?;

    let intercepted_param = opts.intercepted.map(|b| b as i32);
    let rows: Result<Vec<String>, rusqlite::Error> = stmt
        .query_map(
            rusqlite::params![opts.tool.as_deref(), opts.last as i64, intercepted_param],
            |row| {
                let id: i64 = row.get(0)?;
                let tool: String = row.get(1)?;
                let tool_use_id: Option<String> = row.get(2)?;
                let params: Option<String> = row.get(3)?;
                let cwd: Option<String> = row.get(4)?;
                let intercepted: bool = row.get(5)?;
                let handle_id: Option<String> = row.get(6)?;
                let line_count: Option<i64> = row.get(7)?;
                let token_est: Option<i64> = row.get(8)?;
                let duration_ms: Option<i64> = row.get(9)?;
                let exit_code: Option<i64> = row.get(10)?;
                let created_at: i64 = row.get(11)?;

                let obj = serde_json::json!({
                    "id": id,
                    "tool": tool,
                    "tool_use_id": tool_use_id,
                    "params": params,
                    "cwd": cwd,
                    "intercepted": intercepted,
                    "handle_id": handle_id,
                    "line_count": line_count,
                    "token_est": token_est,
                    "duration_ms": duration_ms,
                    "exit_code": exit_code,
                    "created_at": created_at,
                });
                Ok(obj.to_string())
            },
        )?
        .collect();

    Ok(rows?)
}

// ---------------------------------------------------------------------------
// Grep-specific retrieval
// ---------------------------------------------------------------------------

/// Return lines from a stored Grep handle filtered by an exact file path or
/// glob pattern.
///
/// For `content` mode stored handles (`g_XXXXXX`), each line is in
/// `file:line_number:text` format.  For `count` mode handles, each line is
/// `file:count`.  For `files_with_matches` mode, each line is a file path.
///
/// Filtering is applied to the file-path component of each line before
/// `head` truncation.
///
/// At most one of `file` and `glob` should be `Some`.  If both are `None`,
/// behaves identically to `show`.
///
/// # Errors
///
/// Returns `Err("handle not found: <id>")` when absent from the database.
pub fn show_filtered(
    conn: &Connection,
    handle: &str,
    file: Option<&str>,
    glob: Option<&str>,
    exclude: &[String],
    head: usize,
    offset: usize,
) -> Result<String, crate::CtxlError> {
    let result: Option<Option<String>> = conn
        .query_row("SELECT content FROM handles WHERE id=?1", [handle], |row| row.get(0))
        .optional()?;

    match result {
        None => Err(crate::CtxlError::HandleNotFound(handle.to_string())),
        Some(None) => {
            // content IS NULL — attempt global blob resolution.
            let content = resolve_global_content(conn, handle).unwrap_or_default();
            if content.is_empty() {
                return Ok(String::new());
            }
            let no_include = file.is_none() && glob.is_none();
            let no_exclude = exclude.is_empty();
            if no_include && no_exclude {
                let effective = apply_offset(&content, offset);
                return Ok(offset_aware_bound(&effective, head, offset));
            }
            let filtered = if content.starts_with("diff --git ") {
                filter_diff_content(&content, file, glob, exclude)
            } else {
                filter_grep_content(&content, file, glob, exclude)
            };
            let effective = apply_offset(&filtered, offset);
            Ok(offset_aware_bound(&effective, head, offset))
        }
        Some(Some(content)) => {
            let no_include = file.is_none() && glob.is_none();
            let no_exclude = exclude.is_empty();

            if no_include && no_exclude {
                let effective = apply_offset(&content, offset);
                return Ok(offset_aware_bound(&effective, head, offset));
            }

            let filtered = if content.starts_with("diff --git ") {
                filter_diff_content(&content, file, glob, exclude)
            } else {
                filter_grep_content(&content, file, glob, exclude)
            };

            let effective = apply_offset(&filtered, offset);
            Ok(offset_aware_bound(&effective, head, offset))
        }
    }
}

/// Keep `show_grep_file` as a public alias for backward compatibility.
pub fn show_grep_file(
    conn: &Connection,
    handle: &str,
    file: Option<&str>,
    glob: Option<&str>,
    head: usize,
) -> Result<String, crate::CtxlError> {
    show_filtered(conn, handle, file, glob, &[], head, 0)
}

/// Filter grep content by file path include/exclude patterns.
fn filter_grep_content(
    content: &str,
    file: Option<&str>,
    glob: Option<&str>,
    exclude: &[String],
) -> String {
    let filtered: Vec<&str> = content
        .lines()
        .filter(|line| {
            let path = extract_grep_line_path(line);
            let included = match (file, glob) {
                (Some(f), _) => path_matches_file(path, f),
                (_, Some(g)) => matches_glob(path, g),
                _ => true,
            };
            if !included {
                return false;
            }
            if !exclude.is_empty() && exclude.iter().any(|pat| matches_glob(path, pat)) {
                return false;
            }
            true
        })
        .collect();
    let joined = filtered.join("\n");
    // Preserve trailing newline from original content for consistency
    // with filter_diff_content which always appends \n per line.
    if !joined.is_empty() && content.ends_with('\n') {
        joined + "\n"
    } else {
        joined
    }
}

/// Filter diff content by file path include/exclude patterns.
fn filter_diff_content(
    content: &str,
    file: Option<&str>,
    glob: Option<&str>,
    exclude: &[String],
) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let mut result = String::new();

    let mut boundaries: Vec<(usize, String)> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if line.starts_with("diff --git ") {
            let path = crate::compress::diff::extract_diff_path(line);
            boundaries.push((i, path));
        }
    }

    for (idx, (start, ref path)) in boundaries.iter().enumerate() {
        let end = if idx + 1 < boundaries.len() { boundaries[idx + 1].0 } else { lines.len() };

        let mut effective_path = path.clone();
        for line in &lines[*start..end] {
            if let Some(p) = line.strip_prefix("+++ b/") {
                if p != "/dev/null" && !p.is_empty() {
                    effective_path = p.to_string();
                }
                break;
            } else if let Some(p) = line.strip_prefix("+++ ") {
                // Handle non-standard diffs (e.g. git diff --no-prefix)
                if p != "/dev/null" && !p.is_empty() {
                    effective_path = p.to_string();
                }
                break;
            }
        }

        let included = match (file, glob) {
            (Some(f), _) => path_matches_file(&effective_path, f),
            (_, Some(g)) => matches_glob(&effective_path, g),
            _ => true,
        };
        if !included {
            continue;
        }
        if !exclude.is_empty() && exclude.iter().any(|pat| matches_glob(&effective_path, pat)) {
            continue;
        }

        for line in &lines[*start..end] {
            result.push_str(line);
            result.push('\n');
        }
    }

    result
}

pub fn files(conn: &Connection, handle: &str) -> Result<Vec<String>, crate::CtxlError> {
    let result: Option<(Option<String>, String)> = conn
        .query_row("SELECT content, output_mode FROM handles WHERE id=?1", [handle], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()?;

    match result {
        None => Err(crate::CtxlError::HandleNotFound(handle.to_string())),
        Some((None, output_mode)) => {
            // content IS NULL — attempt global blob resolution.
            let content = resolve_global_content(conn, handle).unwrap_or_default();
            if content.is_empty() {
                return Ok(vec![]);
            }
            // Re-use the Some(content) path by falling through.
            files_with_content(&content, &output_mode)
        }
        Some((Some(content), output_mode)) => files_with_content(&content, &output_mode),
    }
}

/// Keep `files_grep` as a public alias for backward compatibility.
pub fn files_grep(conn: &Connection, handle: &str) -> Result<Vec<String>, crate::CtxlError> {
    files(conn, handle)
}

/// Compute the file manifest from content and output_mode.
///
/// Extracted from `files()` so it can be shared with the global-blob fallback
/// path (when `content IS NULL` in the session DB but `blob_id` resolves to
/// global content).
fn files_with_content(content: &str, output_mode: &str) -> Result<Vec<String>, crate::CtxlError> {
    if content.starts_with("diff --git ") {
        let file_counts = files_diff(content);
        return Ok(format_file_table_labeled(&file_counts, "Hunks"));
    }

    let file_counts: Vec<(String, usize)> = match output_mode {
        "count" => crate::compress::grep_preview::parse_count_file_counts(content),
        "files_with_matches" => content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| (l.trim().to_string(), 1))
            .collect(),
        _ => {
            let mut counts: std::collections::HashMap<String, usize> = Default::default();
            for line in content.lines() {
                let path = extract_grep_line_path(line);
                if !path.is_empty() {
                    *counts.entry(path.to_string()).or_default() += 1;
                }
            }
            let mut v: Vec<(String, usize)> = counts.into_iter().collect();
            v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            v
        }
    };

    Ok(format_file_table(&file_counts))
}

/// Extract file manifest with hunk counts from unified diff content.
fn files_diff(content: &str) -> Vec<(String, usize)> {
    let mut file_counts: Vec<(String, usize)> = Vec::new();
    let mut current_path: Option<String> = None;
    let mut hunk_count: usize = 0;

    for line in content.lines() {
        if line.starts_with("diff --git ") {
            if let Some(path) = current_path.take() {
                file_counts.push((path, hunk_count));
            }
            current_path = Some(crate::compress::diff::extract_diff_path(line));
            hunk_count = 0;
        } else if let Some(stripped) = line.strip_prefix("+++ b/") {
            if let Some(ref mut path) = current_path {
                let new_path = stripped.to_string();
                if new_path != "/dev/null" && !new_path.is_empty() {
                    *path = new_path;
                }
            }
        } else if let Some(stripped) = line.strip_prefix("+++ ") {
            // Handle non-standard diffs (e.g. git diff --no-prefix)
            if let Some(ref mut path) = current_path {
                let new_path = stripped.to_string();
                if new_path != "/dev/null" && !new_path.is_empty() {
                    *path = new_path;
                }
            }
        } else if line.starts_with("@@ ") {
            hunk_count += 1;
        }
    }

    if let Some(path) = current_path.take() {
        file_counts.push((path, hunk_count));
    }

    file_counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    file_counts
}

/// Format file counts as a right-aligned tabular display with "Matches" header.
fn format_file_table(file_counts: &[(String, usize)]) -> Vec<String> {
    format_file_table_labeled(file_counts, "Matches")
}

/// Format file counts as a right-aligned tabular display with a custom header label.
fn format_file_table_labeled(file_counts: &[(String, usize)], label: &str) -> Vec<String> {
    if file_counts.is_empty() {
        return vec!["(no files)".to_string()];
    }

    let total_matches: usize = file_counts.iter().map(|(_, c)| c).sum();
    let max_count = file_counts.iter().map(|(_, c)| *c).max().unwrap_or(0);
    let count_width = max_count.to_string().len().max(label.len());

    let mut rows = Vec::with_capacity(file_counts.len() + 2);
    rows.push(format!("{:>width$}  File", label, width = count_width));
    for (file, count) in file_counts {
        rows.push(format!("{:>width$}  {file}", count, width = count_width));
    }
    let label_lower = label.to_lowercase();
    rows.push(format!("({} files, {} total {label_lower})", file_counts.len(), total_matches));
    rows
}

/// Extract the file path from a grep output line.
///
/// - `content` mode: `file:line:text` → returns `file`
/// - `count` mode: `file:count` → returns `file`
/// - `files_with_matches` mode: line is the file path → returns the whole line
///
/// Delegates to `grep_path_colon_pos` which finds the path-terminating colon
/// while correctly skipping Windows drive letters (e.g. `C:`).  Count mode uses
/// `rfind` in `parse_count_file_counts` because its format is `file:count` and
/// the last colon separates the path from the count value.
fn extract_grep_line_path(line: &str) -> &str {
    let trimmed = line.trim();
    match crate::compress::grep_path_colon_pos(trimmed) {
        Some(pos) => &trimmed[..pos],
        None => trimmed,
    }
}

/// Match a stored path against a user-supplied `--file` argument.
///
/// Supports both exact match and suffix match: a relative path like
/// `src/lib.rs` matches a stored absolute path `/Users/alex/project/src/lib.rs`
/// as long as the character preceding the suffix is `/`.
///
/// Strips `./` prefix from both sides before comparison so that
/// `./src/lib.rs` matches `src/lib.rs` and vice versa.
fn path_matches_file(stored: &str, filter: &str) -> bool {
    let stored = stored.strip_prefix("./").unwrap_or(stored);
    let filter = filter.strip_prefix("./").unwrap_or(filter);
    if stored == filter {
        return true;
    }
    if let Some(prefix) = stored.strip_suffix(filter) {
        prefix.ends_with('/') || prefix.ends_with('\\')
    } else {
        false
    }
}

/// Match a file path against a simple glob pattern.
///
/// Supports `*` (matches any sequence of characters except `/`) and literal
/// characters.  Does **not** support `**` (recursive glob).
///
/// Examples:
/// - `"*.rs"` matches `"src/lib.rs"` (applied to just the filename segment? No — applied to full path)
///
/// Wait — for `"*.rs"` we want to match any path ending in `.rs`.  Our `*`
/// matches within a segment, but we apply the pattern to the **full** path.
/// To make `"*.rs"` match `"src/lib.rs"`, we use a last-segment match when the
/// pattern has no `/`: we match the basename against the pattern.
fn matches_glob(path: &str, pattern: &str) -> bool {
    if !pattern.contains('/') {
        // Pattern has no slash: match against the basename only.
        let basename = path.rsplit('/').next().unwrap_or(path);
        glob_match(basename, pattern)
    } else {
        glob_match(path, pattern)
    }
}

/// Recursive glob matcher: `*` matches any sequence of chars except `/`.
fn glob_match(haystack: &str, pattern: &str) -> bool {
    if pattern.is_empty() {
        return haystack.is_empty();
    }
    let mut p_chars = pattern.char_indices();
    if let Some((_, pc)) = p_chars.next() {
        if pc == '*' {
            let rest_p = &pattern[1..];
            // Match zero chars.
            if glob_match(haystack, rest_p) {
                return true;
            }
            // Match one non-'/' char at a time.
            for (i, c) in haystack.char_indices() {
                if c == '/' {
                    break;
                }
                if glob_match(&haystack[i + c.len_utf8()..], rest_p) {
                    return true;
                }
            }
            false
        } else {
            // Literal char match.
            let mut h_chars = haystack.char_indices();
            match h_chars.next() {
                None => false,
                Some((_, hc)) => {
                    hc == pc && glob_match(&haystack[hc.len_utf8()..], &pattern[pc.len_utf8()..])
                }
            }
        }
    } else {
        haystack.is_empty()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn detect_regex_backslash_pipe() {
        let result = detect_regex_metacharacters("listen\\|unlisten");
        assert!(result.is_some(), "should detect \\|");
        let msg = result.unwrap();
        assert!(msg.contains("regex syntax"), "message should mention regex: {msg}");
    }

    #[test]
    fn detect_regex_brackets() {
        let result = detect_regex_metacharacters("[a-z]+");
        assert!(result.is_some(), "should detect [ ]");
    }

    #[test]
    fn detect_regex_clean_query() {
        let result = detect_regex_metacharacters("normal query text");
        assert!(result.is_none(), "plain text should pass");
    }

    #[test]
    fn path_matches_exact() {
        assert!(path_matches_file("src/lib.rs", "src/lib.rs"));
    }

    #[test]
    fn path_matches_dot_slash_prefix() {
        assert!(path_matches_file("./src/lib.rs", "src/lib.rs"));
        assert!(path_matches_file("src/lib.rs", "./src/lib.rs"));
        assert!(path_matches_file("./src/lib.rs", "./src/lib.rs"));
    }

    #[test]
    fn path_matches_suffix() {
        assert!(path_matches_file("/Users/alex/project/src/lib.rs", "src/lib.rs"));
    }

    #[test]
    fn path_rejects_partial_segment() {
        // "lib.rs" should not match "stdlib.rs"
        assert!(!path_matches_file("/Users/alex/project/stdlib.rs", "lib.rs"));
    }

    #[test]
    fn path_rejects_no_separator() {
        // Suffix without preceding '/' should not match
        assert!(!path_matches_file("prefixsrc/lib.rs", "src/lib.rs"));
    }

    #[test]
    fn path_matches_windows_backslash_suffix() {
        assert!(path_matches_file(r"C:\Users\dev\src\lib.rs", "lib.rs"));
    }

    #[test]
    fn path_matches_mixed_separator_suffix() {
        assert!(path_matches_file(r"C:\Users\dev\src\lib.rs", r"src\lib.rs"));
    }

    #[test]
    fn path_rejects_windows_partial_segment() {
        assert!(!path_matches_file(r"C:\Users\dev\stdlib.rs", "lib.rs"));
    }

    #[test]
    fn path_matches_absolute_to_absolute() {
        assert!(path_matches_file(
            "/Users/alex/project/src/lib.rs",
            "/Users/alex/project/src/lib.rs"
        ));
    }

    #[test]
    fn glob_matches_basename() {
        assert!(matches_glob("src/lib.rs", "*.rs"));
        assert!(!matches_glob("src/lib.rs", "*.ts"));
    }

    #[test]
    fn glob_matches_full_path() {
        assert!(matches_glob("src/lib.rs", "src/*.rs"));
        assert!(!matches_glob("other/lib.rs", "src/*.rs"));
    }

    #[test]
    fn bound_lines_no_truncation() {
        let text = "line 1\nline 2\nline 3\n";
        let result = bound_lines(text, 80);
        assert_eq!(result, text);
    }

    #[test]
    fn bound_lines_head_tail_split() {
        // 30 lines, limit 20 → head 9 + separator + tail 10
        let lines: Vec<String> = (1..=30).map(|i| format!("line {i}")).collect();
        let text = lines.join("\n") + "\n";
        let result = bound_lines(&text, 20);

        assert!(result.contains("line 1"), "should include first line");
        assert!(result.contains("line 9"), "should include last head line");
        assert!(result.contains("omitted"), "should have separator");
        assert!(result.contains("line 21"), "should include first tail line");
        assert!(result.contains("line 30"), "should include last line");
        assert!(!result.contains("line 10"), "should omit middle content");
    }

    #[test]
    fn bound_lines_small_limit_falls_back_to_head() {
        let lines: Vec<String> = (1..=30).map(|i| format!("line {i}")).collect();
        let text = lines.join("\n") + "\n";
        // Limit of 5 is too small for head+tail split (needs > TAIL_LINES + 2)
        let result = bound_lines(&text, 5);
        assert!(result.contains("line 1"));
        assert!(result.contains("line 5"));
        assert!(!result.contains("line 6"));
        assert!(!result.contains("omitted"));
    }

    #[test]
    fn tail_lines_returns_last_n() {
        let lines: Vec<String> = (1..=100).map(|i| format!("line {i}")).collect();
        let text = lines.join("\n") + "\n";
        let result = tail_lines(&text, 10);
        assert!(result.contains("line 91"));
        assert!(result.contains("line 100"));
        assert!(!result.contains("\nline 90\n"));
        // 10 content lines + 1 footer line
        assert_eq!(result.lines().count(), 11);
        assert!(result.contains("(showing last 10 of 100 lines)"), "should have footer");
    }

    #[test]
    fn tail_lines_returns_all_when_under_limit() {
        let text = "line 1\nline 2\nline 3\nline 4\nline 5\n";
        let result = tail_lines(text, 10);
        assert_eq!(result.lines().count(), 5);
        assert!(result.contains("line 1"));
        assert!(result.contains("line 5"));
    }

    #[test]
    fn sanitize_fts5_plain_query() {
        assert_eq!(sanitize_fts5_query("hello"), "\"hello\"");
    }

    #[test]
    fn sanitize_fts5_prefix_query() {
        // Trailing * should produce FTS5 prefix syntax: "grep"*
        assert_eq!(sanitize_fts5_query("grep*"), "\"grep\"*");
    }

    #[test]
    fn sanitize_fts5_preserves_underscores() {
        assert_eq!(sanitize_fts5_query("grep_dedup"), "\"grep_dedup\"");
    }

    #[test]
    fn sanitize_fts5_prefix_with_underscore() {
        assert_eq!(sanitize_fts5_query("grep_*"), "\"grep_\"*");
    }

    #[test]
    fn sanitize_fts5_strips_operators() {
        assert_eq!(sanitize_fts5_query("foo+bar"), "\"foobar\"");
        assert_eq!(sanitize_fts5_query("^hello"), "\"hello\"");
    }

    #[test]
    fn sanitize_fts5_escapes_quotes() {
        // Input: say "hello" → quotes doubled: say ""hello"" → wrapped: "say ""hello"""
        assert_eq!(sanitize_fts5_query(r#"say "hello""#), r#""say ""hello""""#);
    }

    #[test]
    fn sanitize_fts5_trims_whitespace() {
        assert_eq!(sanitize_fts5_query("  hello  "), "\"hello\"");
    }

    #[test]
    fn sanitize_fts5_prefix_with_whitespace() {
        assert_eq!(sanitize_fts5_query("  grep*  "), "\"grep\"*");
    }

    #[test]
    fn sanitize_fts5_star_only_produces_empty_prefix() {
        // Edge case: just "*" should produce ""* which is empty prefix
        assert_eq!(sanitize_fts5_query("*"), "\"\"*");
    }

    fn make_test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::apply_schema(&conn).unwrap();
        conn
    }

    fn insert_handle(conn: &Connection, id: &str, content: &str, compressed_body: Option<&str>) {
        conn.execute(
            "INSERT INTO handles (id, tool, output_mode, content, compressed_body, compressed_method, line_count, token_est, truncated, created_at) \
             VALUES (?1, 'Bash', 'mixed', ?2, ?3, ?4, ?5, 10, 0, 1000)",
            rusqlite::params![
                id,
                content,
                compressed_body,
                compressed_body.map(|_| "diff"),
                content.lines().count() as i64,
            ],
        ).unwrap();
    }

    #[test]
    fn show_compressed_returns_compressed_body() {
        let conn = make_test_conn();
        insert_handle(
            &conn,
            "b_aabbcc",
            "raw line 1\nraw line 2\n",
            Some("file.rs\tfn foo\tadded"),
        );
        let result =
            show(&conn, "b_aabbcc", ShowOpts { compressed: true, ..Default::default() }).unwrap();
        assert!(
            result.contains("file.rs\tfn foo\tadded"),
            "should return compressed body, got: {result}"
        );
        assert!(!result.contains("raw line 1"), "should not contain raw content");
    }

    #[test]
    fn show_compressed_falls_back_when_null() {
        let conn = make_test_conn();
        insert_handle(&conn, "b_aabbcc", "raw line 1\nraw line 2\n", None);
        let result =
            show(&conn, "b_aabbcc", ShowOpts { compressed: true, ..Default::default() }).unwrap();
        assert!(
            result.contains("No structural compression available"),
            "should show fallback note, got: {result}"
        );
        assert!(result.contains("raw line 1"), "should fall back to raw content, got: {result}");
    }

    #[test]
    fn show_compressed_false_returns_raw() {
        let conn = make_test_conn();
        insert_handle(&conn, "b_aabbcc", "raw line 1\nraw line 2\n", Some("compressed stuff"));
        let result =
            show(&conn, "b_aabbcc", ShowOpts { compressed: false, ..Default::default() }).unwrap();
        assert!(result.contains("raw line 1"), "should return raw content, got: {result}");
        assert!(!result.contains("compressed stuff"), "should not contain compressed body");
    }

    #[test]
    fn show_compressed_handle_not_found() {
        let conn = make_test_conn();
        let result =
            show(&conn, "b_nonexistent", ShowOpts { compressed: true, ..Default::default() });
        assert!(result.is_err());
    }

    #[test]
    fn show_offset_tiles_without_overlap() {
        let conn = make_test_conn();
        let lines: Vec<String> = (1..=500).map(|i| format!("line {i}")).collect();
        let content = lines.join("\n") + "\n";
        insert_handle(&conn, "b_tile01", &content, None);

        let chunk1 =
            show(&conn, "b_tile01", ShowOpts { head: 80, offset: 80, ..Default::default() })
                .unwrap();
        let chunk2 =
            show(&conn, "b_tile01", ShowOpts { head: 80, offset: 160, ..Default::default() })
                .unwrap();

        let lines1: Vec<&str> = chunk1.lines().collect();
        let lines2: Vec<&str> = chunk2.lines().collect();

        assert_eq!(lines1.len(), 80, "first offset chunk should be exactly 80 lines");
        assert_eq!(lines2.len(), 80, "second offset chunk should be exactly 80 lines");
        assert_eq!(lines1[0], "line 81");
        assert_eq!(lines1[79], "line 160");
        assert_eq!(lines2[0], "line 161");
        assert_eq!(lines2[79], "line 240");
    }

    #[test]
    fn search_results_include_line_numbers() {
        let conn = make_test_conn();
        let content = "alpha\nbeta\ngamma\nalpha again\n";
        insert_handle(&conn, "b_srch01", content, None);
        let results = search(&conn, "b_srch01", "alpha", 20).unwrap();
        assert_eq!(results.len(), 2);
        assert!(results[0].starts_with("1:"), "first match should be line 1, got: {}", results[0]);
        assert!(results[1].starts_with("4:"), "second match should be line 4, got: {}", results[1]);
    }

    #[test]
    fn list_handles_returns_stored() {
        let conn = make_test_conn();
        insert_handle(&conn, "b_list01", "content 1\n", None);
        insert_handle(&conn, "b_list02", "content 2\nline 2\n", None);
        insert_handle(&conn, "b_list03", "content 3\n", None);
        let handles = list_handles(&conn, 10).unwrap();
        assert_eq!(handles.len(), 3);
    }

    #[test]
    fn search_empty_query_returns_error() {
        let conn = make_test_conn();
        insert_handle(&conn, "b_empty1", "some content\n", None);
        let result = search(&conn, "b_empty1", "", 20);
        assert!(result.is_err());
        let result2 = search(&conn, "b_empty1", "   ", 20);
        assert!(result2.is_err());
    }
}
