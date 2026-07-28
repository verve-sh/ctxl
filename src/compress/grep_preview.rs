//! Mode-aware preview generation for stored Grep handles.
//!
//! Dispatches to the appropriate mode-specific helper based on
//! `tool_response.mode`: `content`, `files_with_matches`, or `count`.
//! Falls back to passthrough head/tail for unknown modes.

/// Maximum lines per side (head/tail) in a content-mode preview.
const MAX_PREVIEW_LINES: usize = 10;

/// Maximum files to show in a file manifest.
const MAX_MANIFEST_FILES: usize = 20;

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Parse per-file match counts from `content` mode grep output.
///
/// Each line has the format `file:line_number:content` or just `file:content`.
/// We take the first colon-delimited segment as the file path.
/// Returns a list of `(file, count)` sorted descending by match count.
fn parse_content_file_counts(content: &str) -> Vec<(String, usize)> {
    let mut counts: std::collections::HashMap<String, usize> = Default::default();
    for line in content.lines() {
        if let Some(file) = extract_content_file_path(line) {
            *counts.entry(file).or_default() += 1;
        }
    }
    let mut result: Vec<(String, usize)> = counts.into_iter().collect();
    result.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    result
}

/// Extract the file path from a content-mode grep line.
///
/// Handles `file:line_number:text` and `file:text` formats.
/// Returns `None` for empty lines.
fn extract_content_file_path(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let end = super::grep_path_colon_pos(trimmed)?;
    let path = &trimmed[..end];
    if path.is_empty() {
        None
    } else {
        Some(path.to_string())
    }
}

/// Parse per-file match counts from `count` mode grep output.
///
/// Each line has the format `file:count`.  Returns a list of `(file, count)`
/// sorted descending by count.
pub(crate) fn parse_count_file_counts(content: &str) -> Vec<(String, usize)> {
    let mut result = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // rfind(':') because count mode is `file:count` — the last colon
        // separates path from count.  Content mode uses find(':') in
        // retrieve.rs::extract_grep_line_path since its format is `file:N:text`.
        if let Some(colon_pos) = trimmed.rfind(':') {
            let file = &trimmed[..colon_pos];
            let count_str = &trimmed[colon_pos + 1..];
            if let Ok(count) = count_str.parse::<usize>() {
                if !file.is_empty() {
                    result.push((file.to_string(), count));
                }
            }
        }
    }
    result.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    result
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Generate a mode-aware block message for a stored Grep handle.
///
/// The returned string is placed in `updatedToolOutput.content`.  It includes:
/// - a header referencing the handle ID and summary stats
/// - a mode-specific preview (file manifest, head/tail matches, or counts)
/// - a `ctxl show` retrieval hint
///
/// `filenames` is the `filenames` array from the original GrepOutput payload.
/// `num_matches` is the `numMatches` field from the original GrepOutput payload.
pub fn grep_preview(
    content: &str,
    mode: &str,
    handle_id: &str,
    num_matches: u64,
    filenames: &[String],
) -> String {
    grep_preview_ex(content, mode, handle_id, num_matches, filenames, None, None)
}

/// Extended grep preview with optional stored/total line counts for truncation transparency.
pub fn grep_preview_ex(
    content: &str,
    mode: &str,
    handle_id: &str,
    num_matches: u64,
    filenames: &[String],
    stored_lines: Option<usize>,
    total_lines: Option<usize>,
) -> String {
    let truncation_note = match (stored_lines, total_lines) {
        (Some(stored), Some(total)) if stored < total => {
            format!(", stored first {stored} of {total} lines")
        }
        _ => String::new(),
    };

    match mode {
        "content" => {
            let preview_body = grep_preview_content(content, MAX_PREVIEW_LINES);
            let num_files = filenames.len();
            format!(
                "[ctxl] Grep output captured ({num_matches} matches, {num_files} files{truncation_note}) → {handle_id}\n\
                 {preview_body}\n\
                 Run: ctxl files {handle_id}  |  ctxl show {handle_id}"
            )
        }
        "files_with_matches" => {
            let non_empty: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
            let total_files = non_empty.len();
            let shown = total_files.min(MAX_MANIFEST_FILES);
            let mut msg = format!(
                "[ctxl] Grep output captured ({total_files} files with matches{truncation_note}) → {handle_id}\n\
                 Files ({shown} shown):\n"
            );
            for f in non_empty.iter().take(shown) {
                msg.push_str(&format!("  {f}\n"));
            }
            if total_files > shown {
                msg.push_str(&format!("  ... [{} more files]\n", total_files - shown));
            }
            msg.push_str(&format!("Run: ctxl files {handle_id}  |  ctxl show {handle_id}"));
            msg
        }
        "count" => {
            let preview_body = grep_preview_count(content, MAX_PREVIEW_LINES);
            format!(
                "[ctxl] Grep output captured (count mode{truncation_note}) → {handle_id}\n\
                 {preview_body}\n\
                 Run: ctxl files {handle_id}  |  ctxl show {handle_id}"
            )
        }
        _ => {
            // Unknown mode: fall back to head/tail passthrough preview.
            let preview = crate::compress::passthrough::preview(content, MAX_PREVIEW_LINES);
            format!(
                "[ctxl] Grep output captured (unknown mode{truncation_note}) → {handle_id}\n\
                 {preview}\n\
                 Run: ctxl show {handle_id}"
            )
        }
    }
}

/// Preview for `content` mode (default `rg` output: `file:line_number:text`).
///
/// Returns a formatted string with:
/// - a file manifest sorted descending by match count
/// - a head/tail preview of matching lines (capped at `threshold` lines per side)
pub fn grep_preview_content(content: &str, threshold: usize) -> String {
    let total_lines = content.lines().count();
    let preview = crate::compress::passthrough::preview(content, threshold);

    let file_counts = parse_content_file_counts(content);
    let shown = file_counts.len().min(MAX_MANIFEST_FILES);
    let mut manifest = String::from("Top files by match count:\n");
    for (file, count) in file_counts.iter().take(shown) {
        let plural = if *count == 1 { "" } else { "es" };
        manifest.push_str(&format!("  {file} ({count} match{plural})\n"));
    }
    if file_counts.len() > shown {
        manifest.push_str(&format!("  ... [{} more files]\n", file_counts.len() - shown));
    }

    format!("{manifest}Sample output ({total_lines} lines total):\n{preview}")
}

/// Preview for `count` mode (`rg -c`: `file:count` per line).
///
/// Returns a formatted string with a file manifest derived from `file:N` rows.
/// `threshold` bounds the number of files shown (minimum `MAX_MANIFEST_FILES`).
pub fn grep_preview_count(content: &str, threshold: usize) -> String {
    let file_counts = parse_count_file_counts(content);
    let total_files = file_counts.len();
    let shown = total_files.min(threshold.max(MAX_MANIFEST_FILES));
    let mut manifest = String::from("Files with match counts:\n");
    for (file, count) in file_counts.iter().take(shown) {
        manifest.push_str(&format!("  {file}: {count}\n"));
    }
    if total_files > shown {
        manifest.push_str(&format!("  ... [{} more files]\n", total_files - shown));
    }
    manifest.trim_end().to_string()
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_mode_preview_includes_file_manifest_and_head_tail() {
        let lines: Vec<String> = (0..50)
            .map(|i| format!("src/lib.rs:{}:match_{}", i, i))
            .chain((0..10).map(|i| format!("src/main.rs:{}:other_{}", i, i)))
            .collect();
        let content = lines.join("\n");

        let preview = grep_preview_content(&content, 5);
        assert!(preview.contains("src/lib.rs"), "should mention top file");
        assert!(preview.contains("50 match"), "should show lib.rs count");
        assert!(preview.contains("Sample output"), "should include sample header");
    }

    #[test]
    fn count_mode_preview_shows_sorted_file_counts() {
        let content = "src/main.rs:3\nsrc/lib.rs:15\nsrc/utils.rs:7\n";
        let preview = grep_preview_count(content, 10);
        assert!(preview.contains("src/lib.rs: 15"), "lib.rs should appear (highest count)");
        assert!(preview.contains("src/utils.rs: 7"));
        assert!(preview.contains("src/main.rs: 3"));
    }

    #[test]
    fn files_with_matches_mode_shows_file_manifest() {
        let content = "src/lib.rs\nsrc/main.rs\nsrc/utils.rs\n";
        let msg = grep_preview(content, "files_with_matches", "g_abc123", 0, &[]);
        assert!(msg.contains("src/lib.rs"));
        assert!(msg.contains("src/main.rs"));
        assert!(msg.contains("3 files with matches"));
    }

    #[test]
    fn unknown_mode_falls_back_to_head_tail() {
        let lines: Vec<String> = (0..10).map(|i| format!("line {i}")).collect();
        let content = lines.join("\n");
        let msg = grep_preview(&content, "mystery_mode", "g_abc123", 0, &[]);
        assert!(msg.contains("unknown mode"));
        assert!(msg.contains("g_abc123"));
    }

    #[test]
    fn parse_count_file_counts_handles_rfind_colon() {
        let content = "path/to/file.rs:5\nanother/file.rs:2\n";
        let counts = parse_count_file_counts(content);
        assert_eq!(counts.len(), 2);
        // sorted descending by count
        assert_eq!(counts[0].1, 5);
        assert_eq!(counts[1].1, 2);
    }

    #[test]
    fn content_mode_includes_files_hint() {
        let content = "src/lib.rs:1:fn main() {}\nsrc/lib.rs:2:}\n";
        let msg = grep_preview(content, "content", "g_abc123", 2, &["src/lib.rs".to_string()]);
        assert!(
            msg.contains("ctxl files"),
            "content mode should include 'ctxl files' hint, got: {msg}"
        );
    }

    #[test]
    fn count_mode_includes_files_hint() {
        let content = "src/lib.rs:5\nsrc/main.rs:3\n";
        let msg = grep_preview(content, "count", "g_def456", 8, &[]);
        assert!(
            msg.contains("ctxl files"),
            "count mode should include 'ctxl files' hint, got: {msg}"
        );
    }
}
