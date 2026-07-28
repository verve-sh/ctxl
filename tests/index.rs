#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used, clippy::panic)] // integration tests

use ctxl::{db, index, retrieve, store};
use rusqlite::Connection;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn in_memory_conn() -> Connection {
    let conn = Connection::open_in_memory().expect("open_in_memory");
    db::apply_schema(&conn).expect("apply_schema");
    conn
}

fn default_opts() -> index::IndexOpts {
    index::IndexOpts { hint: None, content_type: None, source: None }
}

// ---------------------------------------------------------------------------
// Content type detection + conversion
// ---------------------------------------------------------------------------

#[test]
fn html_detection_and_markdown_conversion() {
    let conn = in_memory_conn();
    let html = "<html><body><h1>Title</h1><p>Hello world</p></body></html>";

    let result = index::index_content(&conn, html, &default_opts()).expect("index HTML");
    assert!(
        result.handle_id.starts_with("i_"),
        "handle should have i_ prefix, got: {}",
        result.handle_id
    );

    let content = retrieve::show(&conn, &result.handle_id, retrieve::ShowOpts::default())
        .expect("show stored content");
    assert!(content.contains("Title"), "markdown should contain heading text: {content}");
    assert!(content.contains("Hello world"), "markdown should contain paragraph text: {content}");
    assert!(!content.contains("<html>"), "should not contain raw HTML tags: {content}");
}

#[test]
fn json_detection_and_flattening() {
    let conn = in_memory_conn();
    let json = r#"{"name": "test", "nested": {"key": "value"}}"#;

    let result = index::index_content(&conn, json, &default_opts()).expect("index JSON");
    assert!(result.handle_id.starts_with("i_"));

    let content = retrieve::show(&conn, &result.handle_id, retrieve::ShowOpts::default())
        .expect("show stored content");
    assert!(
        content.contains("name = test"),
        "flattened content should have key = value: {content}"
    );
    assert!(
        content.contains("nested.key = value"),
        "nested keys should be dot-separated: {content}"
    );
}

#[test]
fn plain_text_passthrough() {
    let conn = in_memory_conn();
    let text = "line one\nline two\nline three\n";

    let result = index::index_content(&conn, text, &default_opts()).expect("index text");
    assert!(result.handle_id.starts_with("i_"));

    let content = retrieve::show(&conn, &result.handle_id, retrieve::ShowOpts::default())
        .expect("show stored content");
    assert_eq!(content, text, "text should pass through unchanged");
}

// ---------------------------------------------------------------------------
// --hint
// ---------------------------------------------------------------------------

#[test]
fn hint_produces_inline_results() {
    let conn = in_memory_conn();
    let content = "the quick brown fox\njumps over the lazy dog\nfox trot dance\n";

    let opts = index::IndexOpts { hint: Some("fox".to_string()), ..default_opts() };

    let result = index::index_content(&conn, content, &opts).expect("index with hint");
    assert!(!result.hint_matches.is_empty(), "hint should produce matches for 'fox'");
    for m in &result.hint_matches {
        assert!(m.to_lowercase().contains("fox"), "each hint match should contain 'fox': {m}");
    }
}

#[test]
fn hint_omitted_when_no_matches() {
    let conn = in_memory_conn();
    let content = "alpha beta gamma\ndelta epsilon\n";

    let opts =
        index::IndexOpts { hint: Some("nonexistent_token_xyz".to_string()), ..default_opts() };

    let result = index::index_content(&conn, content, &opts).expect("index with no-match hint");
    assert!(result.hint_matches.is_empty(), "hint with no matches should produce empty list");

    let output = index::format_output(&result);
    assert!(
        !output.contains("--- Hint:"),
        "output should not contain hint section when no matches"
    );
}

// ---------------------------------------------------------------------------
// Edge cases
// ---------------------------------------------------------------------------

#[test]
fn empty_stdin_stores_empty_handle() {
    let conn = in_memory_conn();
    let result = index::index_content(&conn, "", &default_opts()).expect("index empty");
    assert!(result.handle_id.starts_with("i_"));
    assert_eq!(result.line_count, 0);
}

#[test]
fn ten_mb_cap_enforced() {
    let conn = in_memory_conn();
    let huge = "x".repeat(11 * 1024 * 1024);

    let err = index::index_content(&conn, &huge, &default_opts()).expect_err("should exceed cap");
    let msg = err.to_string();
    assert!(msg.contains("10 MB"), "error should mention size limit: {msg}");
}

#[test]
fn content_type_override_works() {
    let conn = in_memory_conn();
    // This looks like plain text but we force HTML processing.
    let text = "<p>forced html</p>";

    let opts = index::IndexOpts { content_type: Some(index::ContentType::Html), ..default_opts() };

    let result = index::index_content(&conn, text, &opts).expect("index with override");

    let content = retrieve::show(&conn, &result.handle_id, retrieve::ShowOpts::default())
        .expect("show stored");
    assert!(content.contains("forced html"), "should have processed as HTML markdown: {content}");
    assert!(!content.contains("<p>"), "HTML tags should be stripped: {content}");
}

#[test]
fn handle_prefix_is_i_underscore() {
    let conn = in_memory_conn();
    let result = index::index_content(&conn, "test content", &default_opts()).expect("index");
    assert!(
        result.handle_id.starts_with("i_"),
        "handle should start with i_, got: {}",
        result.handle_id
    );
}

#[test]
fn fts5_index_populated_and_searchable() {
    let conn = in_memory_conn();
    let content = "the Connection API provides open and execute methods\n\
                   use Connection::open() to create a new database\n";

    let result = index::index_content(&conn, content, &default_opts()).expect("index");

    let search_results = retrieve::search(&conn, &result.handle_id, "Connection", 20)
        .expect("search indexed content");
    assert!(!search_results.is_empty(), "FTS5 index should be populated and searchable");
}

// ---------------------------------------------------------------------------
// source metadata
// ---------------------------------------------------------------------------

#[test]
fn source_stored_as_cwd() {
    let conn = in_memory_conn();
    let opts =
        index::IndexOpts { source: Some("https://docs.rs/rusqlite".to_string()), ..default_opts() };

    let result =
        index::index_content(&conn, "some docs content", &opts).expect("index with source");

    let info = retrieve::inspect(&conn, &result.handle_id).expect("inspect");
    assert_eq!(
        info.cwd.as_deref(),
        Some("https://docs.rs/rusqlite"),
        "source should be stored in cwd column"
    );
}

// ---------------------------------------------------------------------------
// format_output
// ---------------------------------------------------------------------------

#[test]
fn format_output_includes_handle_reference() {
    let conn = in_memory_conn();
    let result = index::index_content(&conn, "test data", &default_opts()).expect("index");
    let output = index::format_output(&result);

    assert!(output.contains(&result.handle_id), "output should contain handle ID: {output}");
    assert!(output.contains("[ctxl] Indexed"), "output should have indexed prefix: {output}");
    assert!(output.contains("ctxl show"), "output should suggest ctxl show: {output}");
    assert!(output.contains("ctxl search"), "output should suggest ctxl search: {output}");
}

// ---------------------------------------------------------------------------
// JSON raw preservation
// ---------------------------------------------------------------------------

#[test]
fn json_raw_preserved_in_compressed_body() {
    let conn = in_memory_conn();
    let json = r#"{"hello": "world"}"#;

    let result = index::index_content(&conn, json, &default_opts()).expect("index JSON");

    let info = retrieve::inspect(&conn, &result.handle_id).expect("inspect");
    assert_eq!(
        info.compressed_method.as_deref(),
        Some("json_raw"),
        "compressed_method should be json_raw"
    );
}

// ---------------------------------------------------------------------------
// HTML skip tags
// ---------------------------------------------------------------------------

#[test]
fn html_strips_nav_script_style() {
    let conn = in_memory_conn();
    let html = r#"<html><head><style>body{}</style></head><body>
        <nav><a href="/">Home</a></nav>
        <script>alert('x')</script>
        <main><p>Real content here</p></main>
        <footer>Copyright 2024</footer>
    </body></html>"#;

    let result = index::index_content(&conn, html, &default_opts()).expect("index HTML");
    let content =
        retrieve::show(&conn, &result.handle_id, retrieve::ShowOpts::default()).expect("show");

    assert!(content.contains("Real content"), "main content should be preserved: {content}");
    assert!(!content.contains("alert"), "script content should be stripped: {content}");
    assert!(!content.contains("body{}"), "style content should be stripped: {content}");
}

// ---------------------------------------------------------------------------
// derive_handle_prefix
// ---------------------------------------------------------------------------

#[test]
fn derive_handle_prefix_ctxl_index() {
    let payload = serde_json::json!({ "tool": "ctxl-index" });
    assert_eq!(store::derive_handle_prefix(&payload), "i_");
}

#[test]
fn derive_handle_prefix_other_tools_unchanged() {
    assert_eq!(store::derive_handle_prefix(&serde_json::json!({ "tool": "Bash" })), "b_");
    assert_eq!(store::derive_handle_prefix(&serde_json::json!({ "tool": "Grep" })), "g_");
    assert_eq!(store::derive_handle_prefix(&serde_json::json!({ "tool": "WebFetch" })), "b_");
}
