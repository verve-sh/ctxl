//! `ctxl index` — stdin pipe command that stores + indexes arbitrary content.
//!
//! Reads stdin, auto-detects content type (HTML / JSON / text), converts if
//! needed, stores in session DB with FTS5 indexing, and returns handle +
//! optional inline search results via `--hint`.

use rusqlite::Connection;

/// 10 MB cap on stdin input.
const MAX_INPUT_BYTES: usize = 10 * 1024 * 1024;

/// Maximum inline hint results to display.
const MAX_HINT_RESULTS: usize = 5;

/// Content type as detected or overridden via `--content-type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentType {
    Html,
    Json,
    Text,
}

/// Options for `index_stdin`.
#[derive(Debug, Default)]
pub struct IndexOpts {
    /// FTS5 search query to run against the just-stored content.
    pub hint: Option<String>,
    /// Override auto-detection: "html", "json", "text".
    pub content_type: Option<ContentType>,
    /// Provenance URL stored as metadata.
    pub source: Option<String>,
}

/// Result of `index_stdin` — handle ID + optional hint matches.
#[derive(Debug)]
pub struct IndexResult {
    pub handle_id: String,
    pub line_count: usize,
    pub byte_count: usize,
    pub hint_matches: Vec<String>,
    pub hint_query: Option<String>,
}

// ---------------------------------------------------------------------------
// Content detection
// ---------------------------------------------------------------------------

/// Auto-detect content type from the raw input.
fn detect_content_type(input: &str) -> ContentType {
    let trimmed = input.trim_start();
    if trimmed.starts_with('<') || trimmed.starts_with("<!") {
        return ContentType::Html;
    }
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return ContentType::Json;
    }
    ContentType::Text
}

// ---------------------------------------------------------------------------
// HTML -> Markdown
// ---------------------------------------------------------------------------

/// Tags to skip when converting HTML to markdown.
const SKIP_TAGS: &[&str] = &["script", "style", "nav", "footer", "aside", "header", "noscript"];

fn html_to_markdown(html: &str) -> String {
    let converter = htmd::HtmlToMarkdown::builder().skip_tags(SKIP_TAGS.to_vec()).build();
    match converter.convert(html) {
        Ok(md) => md,
        Err(_) => html.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Read `input` content, detect type, convert, store, and optionally run hint search.
///
/// This is the core logic for `ctxl index`. The caller is responsible for
/// reading stdin and passing the content string.
pub fn index_content(
    conn: &Connection,
    input: &str,
    opts: &IndexOpts,
) -> Result<IndexResult, crate::CtxlError> {
    if input.len() > MAX_INPUT_BYTES {
        return Err(crate::CtxlError::Index(format!(
            "input exceeds {} MB limit ({} bytes)",
            MAX_INPUT_BYTES / (1024 * 1024),
            input.len()
        )));
    }

    let content_type = opts.content_type.unwrap_or_else(|| detect_content_type(input));

    // Convert content based on detected/overridden type.
    let (stored_content, is_json) = match content_type {
        ContentType::Html => (html_to_markdown(input), false),
        ContentType::Json => {
            let flattened = crate::compress::json::compress(input);
            (flattened, true)
        }
        ContentType::Text => (input.to_string(), false),
    };

    let line_count = stored_content.lines().count();
    let byte_count = stored_content.len();

    // Build store payload.
    let store_payload = serde_json::json!({
        "tool": "ctxl-index",
        "output_mode": "stdout",
        "content": stored_content,
        "cwd": opts.source.as_deref().unwrap_or(""),
    });

    let handle_id = crate::store::write(conn, store_payload)?;

    // For JSON: store raw JSON in compressed_body for lossless retrieval.
    if is_json {
        conn.execute(
            "UPDATE handles SET compressed_body = ?1, compressed_method = ?2 WHERE id = ?3",
            rusqlite::params![input, "json_raw", &handle_id],
        )?;
    }

    // Record in calls table.
    crate::calls::insert_calls_row(
        conn,
        &crate::calls::InterceptCallMeta {
            tool: "ctxl-index",
            handle_id: &handle_id,
            line_count: line_count as i64,
            token_est: None,
            tool_input: None,
            cwd: None,
            tool_use_id: None,
            exit_code: None,
        },
    )?;

    // Hint search if requested.
    let mut hint_matches = Vec::new();
    let hint_query = opts.hint.clone();
    if let Some(ref query) = hint_query {
        if let Ok(lines) = crate::retrieve::search(conn, &handle_id, query, MAX_HINT_RESULTS) {
            hint_matches = lines;
        }
    }

    Ok(IndexResult { handle_id, line_count, byte_count, hint_matches, hint_query })
}

/// Format the output for `ctxl index` — handle reference + optional hint results.
pub fn format_output(result: &IndexResult) -> String {
    let mut out = format!(
        "[ctxl] Indexed ({} lines, {} bytes) \u{2192} {}\n\
         Run: ctxl show {}  \u{00b7}  ctxl search {} <query>",
        result.line_count, result.byte_count, result.handle_id, result.handle_id, result.handle_id,
    );

    if let Some(ref query) = result.hint_query {
        if !result.hint_matches.is_empty() {
            out.push_str(&format!(
                "\n\n--- Hint: \"{}\" ({} matches) ---",
                query,
                result.hint_matches.len()
            ));
            for line in &result.hint_matches {
                out.push_str(&format!("\n{line}"));
            }
        }
    }

    out
}
