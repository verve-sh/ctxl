use rusqlite::Connection;
use std::io::Write;

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Configuration for the bash post-intercept command.
#[derive(Debug, Clone)]
pub struct InterceptConfig {
    /// Maximum combined stdout+stderr bytes (post-ANSI-strip) before blocking.
    pub threshold: usize,
    /// Maximum combined stdout+stderr lines before blocking.
    /// Output is intercepted when EITHER byte threshold OR line threshold is exceeded.
    pub line_threshold: usize,
    /// When true, record an `insert_calls_row` after interception.
    /// The router sets this to `false` when norecord is active.
    pub record: bool,
    /// The command that was executed (from `tool_input.command`).
    /// Used for content-aware compression routing (diff detection, code language).
    pub command: Option<String>,
    /// Serialized tool_input JSON from the payload.
    pub tool_input: Option<String>,
    /// Working directory from the payload.
    pub cwd: Option<String>,
    /// Tool use ID from the payload.
    pub tool_use_id: Option<String>,
}

impl InterceptConfig {
    /// Build config from environment variables with compiled-in defaults.
    pub fn from_env() -> Self {
        Self {
            threshold: crate::env_parse("CTXL_BASH_THRESHOLD", 8192),
            line_threshold: crate::env_parse("CTXL_BASH_LINE_THRESHOLD", 200),
            record: true,
            command: None,
            tool_input: None,
            cwd: None,
            tool_use_id: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
pub struct ToolResponse {
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub interrupted: Option<bool>,
    #[serde(rename = "isImage")]
    pub is_image: Option<bool>,
    #[serde(rename = "noOutputExpected")]
    pub no_output_expected: Option<bool>,
    #[serde(rename = "exitCode")]
    pub exit_code: Option<i64>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Format the block message placed in `updatedToolOutput.stdout`.
///
/// The returned string contains:
/// - the handle ID (matching `b_[0-9a-f]{6}`)
/// - the line count
/// - the byte count
/// - file count (when diff or grep-like output is detected)
/// - a one-line content preview (last meaningful line, truncated to 120 chars)
/// - retrieve and search hints (includes `ctxl files` when diff or grep-like)
pub fn format_block_message(
    handle_id: &str,
    line_count: usize,
    byte_count: usize,
    content: &str,
) -> String {
    format_block_message_ex(handle_id, line_count, byte_count, content, None)
}

/// Extended block message with optional total line count for truncation transparency.
///
/// When `total_lines` is `Some(total)` and `total > line_count`, the stats
/// include "stored first {line_count} of {total} lines" so the agent knows
/// that search covers stored lines only.
pub fn format_block_message_ex(
    handle_id: &str,
    line_count: usize,
    byte_count: usize,
    content: &str,
    total_lines: Option<usize>,
) -> String {
    let preview = extract_preview(content);
    let preview_line =
        if preview.is_empty() { String::new() } else { format!("\n  → {preview}") };

    let truncation_note = match total_lines {
        Some(total) if total > line_count => {
            format!(", stored first {line_count} of {total} lines")
        }
        _ => String::new(),
    };

    // Check diff first, then fall through to grep analysis.
    if is_unified_diff(content) {
        let diff_info = analyze_diff_files(content);
        let stats = format!(
            "{line_count} lines, {} files, {byte_count} bytes{truncation_note}",
            diff_info.file_count
        );

        let top_files_line = if diff_info.top_files.is_empty() {
            String::new()
        } else {
            let entries: Vec<String> = diff_info
                .top_files
                .iter()
                .map(|(path, count, is_binary)| {
                    if *is_binary {
                        format!("{path} (binary)")
                    } else if *count == 1 {
                        format!("{path} (1 hunk)")
                    } else {
                        format!("{path} ({count} hunks)")
                    }
                })
                .collect();
            format!("\n  Top: {}", entries.join(", "))
        };

        let hints = format!("Run: ctxl files {handle_id}  ·  ctxl show {handle_id}  ·  ctxl search {handle_id} <query>");

        return format!(
            "[ctxl] Output captured ({stats}) → {handle_id}{preview_line}{top_files_line}
{hints}"
        );
    }

    let grep_info = analyze_grep_files(content);
    let stats = if grep_info.file_count > 0 {
        format!(
            "{line_count} lines, {} files, {byte_count} bytes{truncation_note}",
            grep_info.file_count
        )
    } else {
        format!("{line_count} lines, {byte_count} bytes{truncation_note}")
    };

    let top_files_line = if grep_info.top_files.is_empty() {
        String::new()
    } else {
        let entries: Vec<String> =
            grep_info.top_files.iter().map(|(path, count)| format!("{path} ({count})")).collect();
        format!("\n  Top: {}", entries.join(", "))
    };

    let hints = if grep_info.file_count > 0 {
        format!("Run: ctxl files {handle_id}  ·  ctxl show {handle_id}  ·  ctxl search {handle_id} <query>")
    } else {
        format!("Run: ctxl show {handle_id}  ·  ctxl search {handle_id} <query>")
    };

    format!("[ctxl] Output captured ({stats}) → {handle_id}{preview_line}{top_files_line}\n{hints}")
}

struct GrepFileInfo {
    file_count: usize,
    top_files: Vec<(String, usize)>,
}

struct DiffFileInfo {
    file_count: usize,
    /// (path, hunk_count, is_binary) sorted by hunk count descending, top 3.
    top_files: Vec<(String, usize, bool)>,
}

/// Analyze unified diff output for file distribution and hunk counts.
///
/// Scans for `diff --git a/X b/Y` headers to extract file paths (using
/// the `b/` side), and counts `@@` lines between consecutive headers
/// as hunks per file. Detects binary files via `Binary files ... differ`
/// lines between headers. Returns the top 3 files by hunk count.
fn analyze_diff_files(content: &str) -> DiffFileInfo {
    let mut files: Vec<(String, usize, bool)> = Vec::new();
    let mut current_path: Option<String> = None;
    let mut current_hunks: usize = 0;
    let mut current_is_binary: bool = false;

    for line in content.lines() {
        if line.starts_with("diff --git ") {
            // Flush previous file.
            if let Some(path) = current_path.take() {
                files.push((path, current_hunks, current_is_binary));
            }
            // Extract path: take after last ` b/`.
            let path = if let Some(b_pos) = line.rfind(" b/") {
                line[b_pos + 3..].to_owned()
            } else {
                line.get(11..).unwrap_or("").to_owned()
            };
            current_path = Some(path);
            current_hunks = 0;
            current_is_binary = false;
        } else if line.starts_with("@@ ") && current_path.is_some() {
            current_hunks += 1;
        } else if line.starts_with("Binary files ") && line.ends_with(" differ") {
            current_is_binary = true;
        }
    }

    // Flush last file.
    if let Some(path) = current_path.take() {
        files.push((path, current_hunks, current_is_binary));
    }

    let file_count = files.len();
    files.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let top_files: Vec<(String, usize, bool)> = files.into_iter().take(3).collect();

    DiffFileInfo { file_count, top_files }
}

/// Analyze grep-like output for file distribution.
///
/// Returns file count and top-3 files by match count. Returns zero/empty
/// if fewer than 30% of lines match the `file:digits:` pattern.
fn analyze_grep_files(content: &str) -> GrepFileInfo {
    let mut file_counts: std::collections::HashMap<&str, usize> = Default::default();
    let mut total = 0usize;
    let mut matched = 0usize;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        total += 1;

        if let Some(colon1) = trimmed.find(':') {
            let after = &trimmed[colon1 + 1..];
            if let Some(colon2) = after.find(':') {
                let between = &after[..colon2];
                if !between.is_empty() && between.chars().all(|c| c.is_ascii_digit()) {
                    *file_counts.entry(&trimmed[..colon1]).or_default() += 1;
                    matched += 1;
                }
            }
        }
    }

    if total == 0 || matched * 100 / total < 30 {
        return GrepFileInfo { file_count: 0, top_files: vec![] };
    }

    let file_count = file_counts.len();
    let mut sorted: Vec<(&str, usize)> = file_counts.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let top_files: Vec<(String, usize)> =
        sorted.into_iter().take(3).map(|(p, c)| (p.to_string(), c)).collect();

    GrepFileInfo { file_count, top_files }
}

/// File extensions/names considered generated or lockfile content.
/// Preview extraction skips these in diff output to show meaningful files.
const GENERATED_FILE_PATTERNS: &[&str] = &["package-lock.json", "yarn.lock", "Cargo.lock"];

/// Check if a file path looks like a generated/lockfile that should be
/// deprioritized in diff preview extraction.
fn is_generated_file(path: &str) -> bool {
    // Check exact basename matches for lockfiles.
    let basename = std::path::Path::new(path).file_name().and_then(|f| f.to_str()).unwrap_or(path);
    for pattern in GENERATED_FILE_PATTERNS {
        if basename == *pattern {
            return true;
        }
    }
    // Check extension-based patterns: *.lock, *.min.js, *.min.css
    if let Some(ext) = path.rsplit('.').next() {
        if ext == "lock" {
            return true;
        }
    }
    // Two-part extensions: *.min.js, *.min.css
    if path.ends_with(".min.js") || path.ends_with(".min.css") {
        return true;
    }
    false
}

/// Extract a one-line preview from captured content.
///
/// Strategy:
/// - For unified diffs: show the first non-generated `+++ b/` file path.
/// - Otherwise: scan the last 20 lines for common summary patterns (test results,
///   error counts, pass/fail). If none found, use the last non-empty line.
///   Truncates to 120 chars.
///
/// For Rust `test result:` lines, picks the one with the highest `passed` count
/// rather than the last occurrence (which is typically the doc-tests binary
/// with 0 tests).
fn extract_preview(content: &str) -> String {
    // Diff-specific preview: show the first meaningful file path.
    if is_unified_diff(content) {
        let mut first_path: Option<String> = None;
        for line in content.lines() {
            if let Some(path) = line.strip_prefix("+++ b/") {
                if first_path.is_none() {
                    first_path = Some(path.to_owned());
                }
                if !is_generated_file(path) {
                    return truncate_preview(path);
                }
            }
        }
        // All +++ b/ lines were generated -- fall back to first one.
        if let Some(path) = first_path {
            return truncate_preview(&path);
        }
        // No +++ b/ lines found -- fall through to generic logic.
    }

    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return String::new();
    }

    // Scan tail for summary patterns (test runners, build tools)
    let tail_start = lines.len().saturating_sub(20);
    let mut failed_result: Option<&str> = None; // any line with failures > 0
    let mut best_test_result: Option<(&str, usize)> = None; // (line, passed_count)

    for &line in lines[tail_start..].iter().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Rust test runner: "test result: ok. 30 passed; 0 failed"
        // Priority: any line with failures > 0 wins outright.
        // Otherwise pick highest passed count (skips doc-tests 0/0).
        if trimmed.starts_with("test result:") {
            if failed_result.is_none() && extract_failed_count(trimmed) > 0 {
                failed_result = Some(trimmed);
            }
            let count = extract_passed_count(trimmed);
            match best_test_result {
                None => best_test_result = Some((trimmed, count)),
                Some((_, best)) if count > best => best_test_result = Some((trimmed, count)),
                _ => {}
            }
            continue;
        }
        // Node/Vitest: "Tests  30 passed (30)" or "Test Suites: 5 passed"
        if (trimmed.contains("passed") || trimmed.contains("failed"))
            && (trimmed.contains("Tests") || trimmed.contains("Test"))
        {
            return truncate_preview(trimmed);
        }
        // Build: "error[E" or "warning:" summary counts
        if trimmed.starts_with("error[E") || trimmed.starts_with("error:") {
            return truncate_preview(trimmed);
        }
    }

    // Failures always surface — agent must know something broke.
    if let Some(line) = failed_result {
        return truncate_preview(line);
    }
    if let Some((line, _)) = best_test_result {
        return truncate_preview(line);
    }

    // Fallback: last non-empty line
    for &line in lines.iter().rev() {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            return truncate_preview(trimmed);
        }
    }

    String::new()
}

/// Extract the passed count from a Rust `test result:` line.
/// e.g. "test result: ok. 30 passed; 0 failed" → 30
fn extract_passed_count(line: &str) -> usize {
    extract_count_before(line, "passed")
}

/// Extract the failed count from a Rust `test result:` line.
/// e.g. "test result: FAILED. 2 passed; 1 failed" → 1
fn extract_failed_count(line: &str) -> usize {
    extract_count_before(line, "failed")
}

/// Find "N <keyword>" in a test result line and parse N.
fn extract_count_before(line: &str, keyword: &str) -> usize {
    let Some(pos) = line.find(keyword) else {
        return 0;
    };
    let before = line[..pos].trim_end();
    before
        .rsplit_once(' ')
        .or(before.rsplit_once('.'))
        .or(before.rsplit_once(';'))
        .and_then(|(_, n)| n.trim().parse().ok())
        .unwrap_or(0)
}

fn truncate_preview(s: &str) -> String {
    if s.len() <= 120 {
        s.to_string()
    } else {
        let end = s.floor_char_boundary(119);
        format!("{}…", &s[..end])
    }
}

/// Core bash post-intercept logic.
///
/// Processes a deserialized PostToolUse payload.  If passthrough conditions
/// are met, `writer` remains empty (Claude Code treats empty stdout as "no
/// modification").  Otherwise the combined stdout+stderr is stored via
/// [`crate::store::write`] and a `hookSpecificOutput` envelope is written to
/// `writer`.
///
/// # Passthrough conditions (evaluated in order)
///
/// 1. `tool_response.isImage == true` — binary content, never intercept
/// 2. combined ANSI-stripped size <= threshold
/// 3. `tool_response.interrupted == true` AND combined size < 2 × threshold
#[allow(clippy::print_stderr)] // Intentional: CLI warning to stderr for missing tool_name
pub fn run<W: Write>(
    payload: crate::payload::PostToolUsePayload<ToolResponse>,
    writer: &mut W,
    config: &InterceptConfig,
    conn: &Connection,
) -> Result<(), crate::CtxlError> {
    let resp = &payload.tool_response;

    let is_image = resp.is_image.unwrap_or(false);
    let interrupted = resp.interrupted.unwrap_or(false);
    let no_output_expected = resp.no_output_expected.unwrap_or(false);

    // 1. isImage=true — always passthrough regardless of size.
    if is_image {
        crate::debug::debug_log("[intercept] passthrough: is_image");
        return Ok(());
    }

    let stdout_str = resp.stdout.as_deref().unwrap_or("");
    let stderr_str = resp.stderr.as_deref().unwrap_or("");

    // Measure combined ANSI-stripped size (ANSI codes do not count toward threshold).
    let stripped_out = crate::compress::ansi::strip(stdout_str);
    let stripped_err = crate::compress::ansi::strip(stderr_str);
    let combined_bytes = stripped_out.len() + stripped_err.len();

    let combined_lines = stripped_out.iter().filter(|&&b| b == b'\n').count()
        + stripped_err.iter().filter(|&&b| b == b'\n').count();

    crate::debug::debug_log(&format!(
        "[intercept] bytes={combined_bytes} lines={combined_lines} threshold={} line_threshold={}",
        config.threshold, config.line_threshold
    ));

    // 2. Below or at BOTH thresholds — passthrough.
    //    Exceeding either threshold triggers interception.
    if combined_bytes <= config.threshold && combined_lines <= config.line_threshold {
        crate::debug::debug_log("[intercept] passthrough: below_threshold");
        return Ok(());
    }

    // 3. interrupted=true AND size < 2× threshold — passthrough.
    if interrupted && combined_bytes < 2 * config.threshold {
        crate::debug::debug_log("[intercept] passthrough: interrupted_below_2x");
        return Ok(());
    }

    // --- Blocking path ---

    let combined_content =
        if !stdout_str.is_empty() && !stderr_str.is_empty() && !stdout_str.ends_with('\n') {
            format!("{}\n{}", stdout_str, stderr_str)
        } else {
            format!("{}{}", stdout_str, stderr_str)
        };
    let line_count = combined_content.lines().count();

    // ANSI-stripped view for preview/summary extraction. Colored diagnostics
    // (cargo's `error[E0308]`) defeat prefix matching on raw text, and raw
    // previews leak ESC bytes into the handle message. Includes stderr —
    // compiler warnings and test summaries land there even on exit 0.
    let stripped_combined = {
        let out = String::from_utf8_lossy(&stripped_out);
        let err = String::from_utf8_lossy(&stripped_err);
        if !out.is_empty() && !err.is_empty() && !out.ends_with('\n') {
            format!("{out}\n{err}")
        } else {
            format!("{out}{err}")
        }
    };

    let tool = match payload.tool_name.as_deref() {
        Some(t) => t,
        None => {
            eprintln!("[ctxl] warn: tool_name absent, defaulting to Bash");
            "Bash"
        }
    };

    let mut store_payload = serde_json::json!({
        "tool": tool,
        "output_mode": crate::store::OutputMode::Mixed.to_string(),
        "stdout": stdout_str,
        "stderr": stderr_str,
    });
    if let Some(ti) = &config.tool_input {
        crate::payload::set_tool_input(&mut store_payload, ti);
    }

    let handle_id = crate::store::write(conn, store_payload)?;
    crate::debug::debug_log(&format!(
        "[intercept] intercepted tool={tool} bytes={combined_bytes} handle={handle_id}"
    ));

    apply_compression(conn, &handle_id, &combined_content, config.command.as_deref());

    // Cache write-through (fail-open — session handle already stored above).
    // PostToolUse runs AFTER the command, so we always have the output.
    // On cache miss: write to global DB. On cache hit: touch last_used only.
    try_cache_write_through(
        conn,
        &handle_id,
        &combined_content,
        tool,
        "mixed",
        config.tool_input.as_deref(),
    );

    if config.record {
        crate::calls::insert_calls_row(
            conn,
            &crate::calls::InterceptCallMeta {
                tool,
                handle_id: &handle_id,
                line_count: line_count as i64,
                token_est: Some(crate::store::token_estimate(&combined_content)),
                tool_input: config.tool_input.as_deref(),
                cwd: config.cwd.as_deref(),
                tool_use_id: config.tool_use_id.as_deref(),
                exit_code: resp.exit_code,
            },
        )?;
    }
    let block_msg = match resp.exit_code {
        Some(code) if code != 0 => {
            let preview = extract_preview(&stripped_combined);
            let preview_line =
                if preview.is_empty() { String::new() } else { format!("\n  Preview: {preview}") };
            format!(
                "[ctxl] Command failed (exit {code}). Output captured ({line_count} lines, {combined_bytes} bytes) → {handle_id}{preview_line}\n\
                 Run: ctxl show {handle_id} --tail 40  ·  ctxl search {handle_id} \"error\""
            )
        }
        _ => format_block_message(&handle_id, line_count, combined_bytes, &stripped_combined),
    };

    let envelope = serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "updatedToolOutput": {
                "stdout": block_msg,
                "stderr": "",
                "interrupted": interrupted,
                "isImage": is_image,
                "noOutputExpected": no_output_expected,
            }
        }
    });

    write!(writer, "{}", envelope)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Compression routing
// ---------------------------------------------------------------------------

/// Detect whether `content` is a unified diff (starts with `diff --git`).
pub fn is_unified_diff(content: &str) -> bool {
    let trimmed = content.trim_start();
    trimmed.starts_with("diff --git ")
}

/// Detect the source language from a `cat <file>` command.
///
/// Returns a language identifier suitable for [`crate::compress::code::compress`]
/// when the command is a simple `cat path/to/file.ext` invocation with a
/// recognized extension.
pub fn detect_code_language_from_command(command: &str) -> Option<&'static str> {
    let trimmed = command.trim();
    let trimmed = match trimmed.find(" #") {
        Some(pos) => trimmed[..pos].trim_end(),
        None => trimmed,
    };
    if !trimmed.starts_with("cat ") || trimmed.contains('|') || trimmed.contains('>') {
        return None;
    }
    let args: Vec<&str> = trimmed.split_whitespace().collect();
    // `cat file` or `cat -n file` — take the last non-flag argument
    let path = args.iter().rev().find(|a| !a.starts_with('-'))?;
    let ext = path.rsplit('.').next()?;
    crate::compress::language_from_extension(ext)
}

/// Apply content-aware compression to a stored Bash handle.
///
/// Detection priority:
/// 1. Unified diff → `compress::diff` (entity-level attribution)
/// 2. `cat <file>` with known extension → `compress::code` (tree-sitter skeleton)
///
/// Compression is supplementary — the raw content is always preserved.
/// Errors are logged and swallowed (fail-open).
fn apply_compression(
    conn: &rusqlite::Connection,
    handle_id: &str,
    content: &str,
    command: Option<&str>,
) {
    let (method, body) = if is_unified_diff(content) {
        let tuples = crate::compress::diff::compress(content);
        if tuples.is_empty() {
            return;
        }
        let body: String = tuples
            .iter()
            .map(|(file, entity, change)| format!("{file}\t{entity}\t{change}"))
            .collect::<Vec<_>>()
            .join("\n");
        ("diff", body)
    } else if let Some(lang) = command.and_then(detect_code_language_from_command) {
        let skeleton = crate::compress::code::compress(content, lang);
        // Only store if compression actually reduced content (skeleton != original).
        if skeleton.len() >= content.len() {
            return;
        }
        ("code", skeleton)
    } else {
        return;
    };

    let result = conn.execute(
        "UPDATE handles SET compressed_body = ?1, compressed_method = ?2 WHERE id = ?3",
        rusqlite::params![body, method, handle_id],
    );
    if let Err(e) = result {
        eprintln!("[ctxl] warn: compression update failed: {e}");
    }
}

// ---------------------------------------------------------------------------
// Cache write-through
// ---------------------------------------------------------------------------

/// Attempt to write the handle content to the global cross-session cache.
///
/// Fail-open: any error is silently swallowed — the session handle was already
/// stored before this is called.  The algorithm:
///
/// 1. Build a [`crate::cache::CacheContext`] — resolves `repo_root` and checks
///    git availability.
/// 2. If the worktree is dirty (uncommitted changes), skip — results are not
///    reproducible, so caching would be misleading.
/// 3. Hash the normalized params → `param_hash`.
/// 4. Look up the global cache.
///    - **Hit:** touch `last_used` on the global blob.  Skip writing new content
///      (it's already there). Update session handle's `blob_id`/`param_hash`/
///      `git_head` to link it back to the global entry.
///    - **Miss:** store content in the global DB, then update the session handle.
///
/// `params_json` is the raw tool_input JSON string from the PostToolUse payload.
/// When `None`, `param_hash` is derived from the content hash as a fallback.
pub fn try_cache_write_through(
    conn: &rusqlite::Connection,
    handle_id: &str,
    content: &str,
    tool: &str,
    output_mode: &str,
    params_json: Option<&str>,
) {
    // Build context (resolves repo_root once).
    let ctx = crate::cache::CacheContext::new();
    let repo_root = match &ctx.repo_root {
        Some(r) => r.clone(),
        None => return, // not in a git repo → skip
    };

    // Dirty worktree → skip (results are not reproducible).
    if ctx.is_dirty() {
        return;
    }

    let git_head = match ctx.git_head() {
        Some(h) => h,
        None => return,
    };

    // Derive param_hash consistently with cache_check(): normalize the raw
    // params JSON, then hash.  When params_json is unavailable, fall back to
    // a content-derived hash (won't match cache_check, but still deduplicates
    // identical output within write-through).
    let param_hash = match params_json {
        Some(pj) => {
            let normalized = crate::cache::normalize_params(pj);
            crate::cache::xxh128_hex(&normalized)
        }
        None => {
            let content_hash = crate::cache::xxh128_hex(content);
            crate::cache::xxh128_hex(&format!("{tool}:{output_mode}:{content_hash}"))
        }
    };
    let content_hash = crate::cache::xxh128_hex(content);

    let db_path = match crate::global_db::global_db_path() {
        Some(p) => p,
        None => return,
    };
    let gconn = match crate::global_db::open_global_db(&db_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[ctxl] global db open failed: {e}");
            return;
        }
    };

    // Check for an existing entry.
    match crate::global_db::lookup(&gconn, &repo_root, tool, output_mode, &param_hash, &git_head) {
        Ok(Some(hit)) => {
            // Cache hit — touch last_used, link session handle.
            let _ = crate::global_db::touch_blob(&gconn, hit.blob_id);
            let _ = conn.execute(
                "UPDATE handles SET blob_id=?1, param_hash=?2, git_head=?3 WHERE id=?4",
                rusqlite::params![hit.blob_id, param_hash, git_head, handle_id],
            );
        }
        Ok(None) => {
            // Cache miss — write to global DB.
            let line_count = content.lines().count() as i64;
            let tok_est = crate::store::token_estimate(content);
            let params = crate::global_db::StoreBlobParams {
                content_hash: &content_hash,
                content,
                compressed_body: None,
                compressed_method: None,
                line_count: Some(line_count),
                token_est: Some(tok_est),
            };
            if let Ok(blob_id) = crate::global_db::store_blob(
                &gconn,
                params,
                &repo_root,
                tool,
                output_mode,
                &param_hash,
                &git_head,
            ) {
                // silently swallow errors — session handle is already stored
                let _ = conn.execute(
                    "UPDATE handles SET blob_id=?1, param_hash=?2, git_head=?3 WHERE id=?4",
                    rusqlite::params![blob_id, param_hash, git_head, handle_id],
                );
            }
        }
        Err(e) => {
            eprintln!("[ctxl] global cache lookup failed: {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::db;

    fn make_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        db::apply_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn blocking_path_inserts_calls_row() {
        let conn = make_conn();
        let config = InterceptConfig {
            threshold: 8192,
            line_threshold: 200,
            record: true,
            command: None,
            tool_input: None,
            cwd: None,
            tool_use_id: None,
        };

        // Build a payload whose stdout exceeds the 8192-byte threshold.
        let big_output = "x".repeat(9000);
        let payload = crate::payload::PostToolUsePayload {
            tool_name: Some("Bash".into()),
            tool_response: ToolResponse {
                stdout: Some(big_output),
                stderr: Some(String::new()),
                interrupted: Some(false),
                is_image: Some(false),
                no_output_expected: Some(false),
                exit_code: None,
            },
        };
        let mut output = Vec::new();

        run(payload, &mut output, &config, &conn).unwrap();

        // A calls row with intercepted=true and a non-empty handle_id must exist.
        let (intercepted, handle_id): (bool, String) = conn
            .query_row("SELECT intercepted, handle_id FROM calls", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert!(intercepted);
        assert!(!handle_id.is_empty());
    }

    #[test]
    fn below_threshold_no_calls_row() {
        let conn = make_conn();
        let config = InterceptConfig {
            threshold: 8192,
            line_threshold: 200,
            record: true,
            command: None,
            tool_input: None,
            cwd: None,
            tool_use_id: None,
        };

        let payload = crate::payload::PostToolUsePayload {
            tool_name: Some("Bash".into()),
            tool_response: ToolResponse {
                stdout: Some("small output".into()),
                stderr: Some(String::new()),
                interrupted: Some(false),
                is_image: Some(false),
                no_output_expected: Some(false),
                exit_code: None,
            },
        };
        let mut output = Vec::new();

        run(payload, &mut output, &config, &conn).unwrap();

        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM calls", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn block_message_includes_search_hint() {
        let msg = format_block_message("b_abc123", 42, 1024, "some content");
        assert!(msg.contains("ctxl search b_abc123 <query>"), "missing search hint in: {msg}");
        assert!(msg.contains("ctxl show b_abc123"), "missing show hint in: {msg}");
    }

    #[test]
    fn block_message_includes_preview() {
        let content = "line 1\nline 2\ntest result: ok. 30 passed; 0 failed\n";
        let msg = format_block_message("b_abc123", 3, 100, content);
        assert!(msg.contains("test result: ok. 30 passed; 0 failed"), "missing preview in: {msg}");
    }

    #[test]
    fn block_message_fallback_preview() {
        let content = "first line\nsecond line\nlast line\n";
        let msg = format_block_message("b_abc123", 3, 100, content);
        assert!(msg.contains("→ last line"), "missing fallback preview in: {msg}");
    }

    #[test]
    fn block_message_empty_content_no_preview() {
        let msg = format_block_message("b_abc123", 0, 0, "");
        // The message contains "→ b_abc123" (handle ID), but should NOT contain
        // the preview prefix "  → " (indented arrow for preview line).
        assert!(!msg.contains("  → "), "unexpected preview in empty content: {msg}");
    }

    #[test]
    fn extract_preview_rust_test_result() {
        let content = "running 30 tests\ntest foo ... ok\ntest bar ... ok\n\ntest result: ok. 30 passed; 0 failed; 0 ignored\n";
        assert_eq!(extract_preview(content), "test result: ok. 30 passed; 0 failed; 0 ignored");
    }

    #[test]
    fn extract_preview_picks_highest_passed_count() {
        // Simulates multi-crate cargo test: lib tests (30 passed), then doc-tests (0 passed)
        let content = "\
test result: ok. 30 passed; 0 failed; 0 ignored
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored
";
        assert_eq!(extract_preview(content), "test result: ok. 30 passed; 0 failed; 0 ignored");
    }

    #[test]
    fn extract_passed_count_parses() {
        assert_eq!(extract_passed_count("test result: ok. 30 passed; 0 failed"), 30);
        assert_eq!(extract_passed_count("test result: ok. 0 passed; 0 failed"), 0);
        assert_eq!(extract_passed_count("test result: FAILED. 2 passed; 1 failed"), 2);
    }

    #[test]
    fn extract_failed_count_parses() {
        assert_eq!(extract_failed_count("test result: FAILED. 2 passed; 1 failed"), 1);
        assert_eq!(extract_failed_count("test result: ok. 30 passed; 0 failed"), 0);
    }

    #[test]
    fn extract_preview_failures_win_over_higher_passed() {
        // Crate A passes 50, crate B fails 2, doc-tests 0 — failure must surface
        let content = "\
test result: ok. 50 passed; 0 failed; 0 ignored
test result: FAILED. 8 passed; 2 failed; 0 ignored
test result: ok. 0 passed; 0 failed; 0 ignored
";
        assert_eq!(extract_preview(content), "test result: FAILED. 8 passed; 2 failed; 0 ignored");
    }

    #[test]
    fn block_message_grep_like_includes_files_hint() {
        let lines: Vec<String> = (1..=20).map(|i| format!("src/lib.rs:{i}:match_{i}")).collect();
        let content = lines.join("\n");
        let msg = format_block_message("b_abc123", 20, 500, &content);
        assert!(
            msg.contains("ctxl files b_abc123"),
            "grep-like output should include files hint: {msg}"
        );
        assert!(msg.contains("1 files"), "should report file count: {msg}");
        assert!(msg.contains("Top: src/lib.rs (20)"), "should show top file: {msg}");
    }

    #[test]
    fn block_message_grep_like_shows_top_3() {
        let mut lines: Vec<String> = Vec::new();
        for i in 1..=30 {
            lines.push(format!("src/a.rs:{i}:match"));
        }
        for i in 1..=20 {
            lines.push(format!("src/b.rs:{i}:match"));
        }
        for i in 1..=10 {
            lines.push(format!("src/c.rs:{i}:match"));
        }
        for i in 1..=5 {
            lines.push(format!("src/d.rs:{i}:match"));
        }
        let content = lines.join("\n");
        let msg = format_block_message("b_abc123", 65, 2000, &content);
        assert!(msg.contains("4 files"), "should report 4 files: {msg}");
        assert!(msg.contains("src/a.rs (30)"), "should show top file: {msg}");
        assert!(msg.contains("src/b.rs (20)"), "should show second file: {msg}");
        assert!(msg.contains("src/c.rs (10)"), "should show third file: {msg}");
        assert!(!msg.contains("src/d.rs (5)"), "should not show fourth file in top list: {msg}");
    }

    #[test]
    fn block_message_non_grep_no_files_hint() {
        let content = "just some regular output\nnothing grep-like here\n";
        let msg = format_block_message("b_abc123", 2, 50, content);
        assert!(
            !msg.contains("ctxl files"),
            "non-grep output should not include files hint: {msg}"
        );
        assert!(!msg.contains("Top:"), "non-grep should not show top files: {msg}");
    }

    #[test]
    fn analyze_grep_files_detects_pattern() {
        let lines: Vec<String> = (1..=10)
            .map(|i| format!("src/lib.rs:{i}:fn foo()"))
            .chain((1..=5).map(|i| format!("src/main.rs:{i}:fn bar()")))
            .collect();
        let content = lines.join("\n");
        let info = analyze_grep_files(&content);
        assert_eq!(info.file_count, 2);
        assert_eq!(info.top_files.len(), 2);
        assert_eq!(info.top_files[0], ("src/lib.rs".to_string(), 10));
        assert_eq!(info.top_files[1], ("src/main.rs".to_string(), 5));
    }

    #[test]
    fn analyze_grep_files_rejects_low_ratio() {
        let mut lines: Vec<String> = (1..=9).map(|i| format!("regular line {i}")).collect();
        lines.push("src/lib.rs:1:match".to_string());
        let content = lines.join("\n");
        let info = analyze_grep_files(&content);
        assert_eq!(info.file_count, 0);
        assert!(info.top_files.is_empty());
    }

    #[test]
    fn extract_preview_truncates_long_lines() {
        let long_line = "x".repeat(200);
        let content = format!("short\n{long_line}\n");
        let preview = extract_preview(&content);
        assert!(preview.chars().count() <= 121); // 119 chars + ellipsis
        assert!(preview.ends_with('…'));
    }

    #[test]
    fn truncate_preview_utf8_boundary() {
        // 4-byte emoji repeated — boundary at 119 bytes falls mid-char
        let s = "🦀".repeat(40); // 160 bytes
        let result = truncate_preview(&s);
        assert!(result.is_char_boundary(result.len()));
        assert!(result.ends_with('…'));
        assert!(result.len() <= 123); // 29 emojis (116 bytes) + 3-byte ellipsis

        // CJK 3-byte chars at boundary
        let s = "漢".repeat(50); // 150 bytes
        let result = truncate_preview(&s);
        assert!(result.is_char_boundary(result.len()));
        assert!(result.ends_with('…'));
    }

    #[test]
    fn block_message_binary_diff_shows_binary_label() {
        // A diff with a binary file produces a `diff --git` header and a
        // `Binary files ... differ` line but no `@@` hunks.
        // A mode-change-only file also has zero `@@` hunks but is NOT binary.
        let diff = concat!(
            "diff --git a/assets/icon.png b/assets/icon.png\n",
            "index abc1234..def5678 100644\n",
            "Binary files a/assets/icon.png and b/assets/icon.png differ\n",
            "diff --git a/src/lib.rs b/src/lib.rs\n",
            "old mode 100644\n",
            "new mode 100755\n",
            "diff --git a/src/main.rs b/src/main.rs\n",
            "--- a/src/main.rs\n",
            "+++ b/src/main.rs\n",
            "@@ -1,3 +1,4 @@\n",
            " context\n",
            "+added\n",
        );
        let msg = format_block_message("b_abc123", 10, 400, diff);
        assert!(
            msg.contains("assets/icon.png (binary)"),
            "binary file should be labeled (binary), got: {msg}"
        );
        assert!(
            !msg.contains("assets/icon.png (0 hunks)"),
            "should not show '(0 hunks)' for binary files, got: {msg}"
        );
        assert!(
            msg.contains("src/main.rs (1 hunk)"),
            "non-binary file with 1 hunk should show singular '(1 hunk)', got: {msg}"
        );
        // Mode-change-only file must NOT be labeled "(binary)".
        assert!(
            !msg.contains("src/lib.rs (binary)"),
            "mode-change-only file should not be labeled (binary), got: {msg}"
        );
        assert!(
            msg.contains("src/lib.rs (0 hunks)"),
            "mode-change-only file should show (0 hunks), got: {msg}"
        );
    }

    #[test]
    fn analyze_diff_files_counts_hunks() {
        let diff = concat!(
            "diff --git a/src/intercept.rs b/src/intercept.rs\n",
            "--- a/src/intercept.rs\n",
            "+++ b/src/intercept.rs\n",
            "@@ -5,6 +5,7 @@ fn foo() {\n",
            " context\n",
            "+added\n",
            "@@ -20,3 +21,4 @@ fn bar() {\n",
            " context\n",
            "+added\n",
            "diff --git a/src/main.rs b/src/main.rs\n",
            "--- a/src/main.rs\n",
            "+++ b/src/main.rs\n",
            "@@ -1,3 +1,4 @@\n",
            " context\n",
            "+added\n",
        );
        let info = analyze_diff_files(diff);
        assert_eq!(info.file_count, 2);
        assert_eq!(info.top_files[0], ("src/intercept.rs".to_string(), 2, false));
        assert_eq!(info.top_files[1], ("src/main.rs".to_string(), 1, false));
    }

    #[test]
    fn extract_preview_diff_skips_lockfile() {
        let diff = concat!(
            "diff --git a/package-lock.json b/package-lock.json\n",
            "--- a/package-lock.json\n",
            "+++ b/package-lock.json\n",
            "@@ -1,3 +1,4 @@\n",
            " context\n",
            "+added\n",
            "diff --git a/src/main.rs b/src/main.rs\n",
            "--- a/src/main.rs\n",
            "+++ b/src/main.rs\n",
            "@@ -1,3 +1,4 @@\n",
            " context\n",
            "+added\n",
        );
        let preview = extract_preview(diff);
        assert_eq!(preview, "src/main.rs");
    }

    #[test]
    fn is_generated_file_detects_patterns() {
        assert!(is_generated_file("package-lock.json"));
        assert!(is_generated_file("yarn.lock"));
        assert!(is_generated_file("Cargo.lock"));
        assert!(is_generated_file("something.lock"));
        assert!(is_generated_file("vendor/bundle.min.js"));
        assert!(is_generated_file("styles.min.css"));
        assert!(!is_generated_file("src/main.rs"));
        assert!(!is_generated_file("src/lock_manager.rs"));
        // Basename-anchored: suffix substrings must NOT match GENERATED_FILE_PATTERNS.
        // Note: my-yarn.lock still matches via the *.lock extension check (correct).
        assert!(
            !is_generated_file("vendor/not-package-lock.json"),
            "not-package-lock.json is not package-lock.json"
        );
        assert!(
            !is_generated_file("my-package-lock.json"),
            "my-package-lock.json is not package-lock.json"
        );
        // But subdirectory paths with exact basenames still match.
        assert!(is_generated_file("some/dir/package-lock.json"));
        assert!(is_generated_file("deep/nested/yarn.lock"));
    }

    #[test]
    fn format_block_message_ex_truncation_note() {
        // total_lines > line_count → message includes truncation note.
        let msg = format_block_message_ex("b_abc123", 50, 2000, "some content", Some(200));
        assert!(msg.contains("stored first 50 of 200 lines"), "truncation note missing in: {msg}");

        // total_lines == line_count → no truncation note.
        let msg_eq = format_block_message_ex("b_abc123", 50, 2000, "some content", Some(50));
        assert!(
            !msg_eq.contains("stored first"),
            "should not show truncation note when total == stored: {msg_eq}"
        );

        // total_lines is None → no truncation note (same as format_block_message).
        let msg_none = format_block_message_ex("b_abc123", 50, 2000, "some content", None);
        assert!(
            !msg_none.contains("stored first"),
            "should not show truncation note when total_lines is None: {msg_none}"
        );
    }

    #[test]
    fn failed_command_includes_preview() {
        let conn = make_conn();
        let config = InterceptConfig {
            threshold: 8192,
            line_threshold: 200,
            record: true,
            command: None,
            tool_input: None,
            cwd: None,
            tool_use_id: None,
        };

        // Build a payload whose stdout exceeds threshold and has exit_code != 0.
        let big_output =
            format!("{}\ntest result: FAILED. 8 passed; 2 failed; 0 ignored\n", "x\n".repeat(300));
        let payload = crate::payload::PostToolUsePayload {
            tool_name: Some("Bash".into()),
            tool_response: ToolResponse {
                stdout: Some(big_output),
                stderr: Some(String::new()),
                interrupted: Some(false),
                is_image: Some(false),
                no_output_expected: Some(false),
                exit_code: Some(1),
            },
        };
        let mut output = Vec::new();

        run(payload, &mut output, &config, &conn).unwrap();

        let output_str = String::from_utf8(output).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output_str).unwrap();
        let stdout = parsed["hookSpecificOutput"]["updatedToolOutput"]["stdout"].as_str().unwrap();
        assert!(
            stdout.contains("Command failed (exit 1)"),
            "should contain failure message, got: {stdout}"
        );
        assert!(
            stdout.contains("Preview: test result: FAILED. 8 passed; 2 failed"),
            "should contain preview with test result, got: {stdout}"
        );
    }

    #[test]
    fn failed_command_empty_content_no_preview() {
        let conn = make_conn();
        let config = InterceptConfig {
            threshold: 10,
            line_threshold: 5,
            record: true,
            command: None,
            tool_input: None,
            cwd: None,
            tool_use_id: None,
        };

        let payload = crate::payload::PostToolUsePayload {
            tool_name: Some("Bash".into()),
            tool_response: ToolResponse {
                stdout: Some(" ".repeat(20)),
                stderr: Some(String::new()),
                interrupted: Some(false),
                is_image: Some(false),
                no_output_expected: Some(false),
                exit_code: Some(127),
            },
        };
        let mut output = Vec::new();

        run(payload, &mut output, &config, &conn).unwrap();

        let output_str = String::from_utf8(output).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output_str).unwrap();
        let stdout = parsed["hookSpecificOutput"]["updatedToolOutput"]["stdout"].as_str().unwrap();
        assert!(
            stdout.contains("Command failed (exit 127)"),
            "should contain failure message, got: {stdout}"
        );
        // Whitespace-only content → preview is the last non-empty line (empty string),
        // so "Preview:" should not appear.
        assert!(
            !stdout.contains("Preview:"),
            "whitespace-only content should not show Preview:, got: {stdout}"
        );
    }

    #[test]
    fn detect_code_language_strips_trailing_comment() {
        assert_eq!(
            detect_code_language_from_command("cat file.rs # guard-override: reason"),
            Some("rust")
        );
        assert_eq!(detect_code_language_from_command("cat file.rs"), Some("rust"));
        assert_eq!(detect_code_language_from_command("cat file.rs | head"), None);
    }
}
