//! Entity-level attribution for unified diff output.
//!
//! Given `diff --git` formatted text, [`compress`] returns
//! `(file, entity_name, change_type)` tuples describing *which* function or
//! type was changed and *how* (`"added"` / `"removed"` / `"modified"`).
//!
//! Entity names are resolved in two stages:
//! 1. The `@@` hunk header's trailing context string (git populates this with
//!    the enclosing function name for most languages).
//! 2. Tree-sitter parse of the hunk post-image as fallback.
//! 3. `"<unknown>"` when neither succeeds.

use tree_sitter::Parser;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Analyse `diff_text` and return entity-level attribution tuples.
///
/// Each element is `(file, entity_name, change_type)` where `change_type` is
/// `"added"`, `"removed"`, or `"modified"`.
///
/// Duplicate `(file, entity)` pairs from multiple hunks are merged: conflicting
/// change types collapse to `"modified"`.
pub fn compress(diff_text: &str) -> Vec<(String, String, String)> {
    let sections = parse_diff(diff_text);
    let mut entries: Vec<(String, String, String)> = Vec::new();

    for section in &sections {
        let lang = detect_language(&section.new_path);

        for hunk in &section.hunks {
            let entity = resolve_entity(hunk, lang.as_deref());
            let change_type = classify_change(hunk);
            entries.push((section.new_path.clone(), entity, change_type));
        }
    }

    merge_entries(entries)
}

// ---------------------------------------------------------------------------
// Diff parsing
// ---------------------------------------------------------------------------

pub(crate) struct FileDiff {
    pub(crate) new_path: String,
    pub(crate) hunks: Vec<Hunk>,
}

pub(crate) struct Hunk {
    /// Text after `@@ ... @@ ` — git function context hint.
    pub(crate) header_context: String,
    /// Raw hunk lines including leading ` `, `+`, `-` sigil.
    pub(crate) lines: Vec<String>,
}

#[allow(clippy::manual_strip)] // pre-existing; changing would alter logic flow unnecessarily
pub(crate) fn parse_diff(diff_text: &str) -> Vec<FileDiff> {
    let mut sections: Vec<FileDiff> = Vec::new();
    let mut current: Option<FileDiff> = None;
    let mut current_hunk: Option<Hunk> = None;

    for line in diff_text.lines() {
        if line.starts_with("diff --git ") {
            flush_hunk(&mut current_hunk, &mut current);
            flush_section(&mut current, &mut sections);

            current = Some(FileDiff { new_path: extract_diff_path(line), hunks: Vec::new() });
        } else if line.starts_with("+++ b/") {
            if let Some(sec) = current.as_mut() {
                let path = line[6..].to_owned();
                if path != "/dev/null" && !path.is_empty() {
                    sec.new_path = path;
                }
            }
        } else if line.starts_with("+++ ") {
            if let Some(sec) = current.as_mut() {
                let path = line[4..].to_owned();
                if path != "/dev/null" && !path.is_empty() {
                    sec.new_path = path;
                }
            }
        } else if line.starts_with("@@ ") {
            flush_hunk(&mut current_hunk, &mut current);
            let ctx = parse_hunk_context(line);
            current_hunk = Some(Hunk { header_context: ctx, lines: Vec::new() });
        } else if let Some(hunk) = current_hunk.as_mut() {
            if line.starts_with(' ') || line.starts_with('+') || line.starts_with('-') {
                hunk.lines.push(line.to_owned());
            }
        }
    }

    flush_hunk(&mut current_hunk, &mut current);
    flush_section(&mut current, &mut sections);
    sections
}

fn flush_hunk(hunk: &mut Option<Hunk>, section: &mut Option<FileDiff>) {
    if let Some(h) = hunk.take() {
        if let Some(sec) = section.as_mut() {
            sec.hunks.push(h);
        }
    }
}

fn flush_section(section: &mut Option<FileDiff>, sections: &mut Vec<FileDiff>) {
    if let Some(sec) = section.take() {
        sections.push(sec);
    }
}

pub(crate) fn extract_diff_path(line: &str) -> String {
    // `diff --git a/foo/bar.rs b/foo/bar.rs` — take after last ` b/`
    if let Some(b_pos) = line.rfind(" b/") {
        return line[b_pos + 3..].to_owned();
    }
    line.get(11..).unwrap_or("").to_owned()
}

/// Extract the function context from `@@ -a,b +c,d @@ <context>`.
fn parse_hunk_context(line: &str) -> String {
    // Everything after the second ` @@ `.
    if let Some(pos) = line.find(" @@ ") {
        return line[pos + 4..].trim().to_owned();
    }
    String::new()
}

// ---------------------------------------------------------------------------
// Entity resolution
// ---------------------------------------------------------------------------

fn resolve_entity(hunk: &Hunk, language: Option<&str>) -> String {
    // 1. Git hunk header context.
    if !hunk.header_context.is_empty() {
        let name = extract_entity_name_from_context(&hunk.header_context);
        if !name.is_empty() {
            return name;
        }
    }

    // 2. Tree-sitter parse of the hunk post-image.
    if let Some(lang) = language {
        if let Some(name) = entity_from_hunk_ts(hunk, lang) {
            return name;
        }
    }

    "<unknown>".to_owned()
}

/// Extract a function/type name from a git context string like
/// `fn my_function(x: i32)` or `class Foo {`.
#[allow(clippy::manual_pattern_char_comparison)] // pre-existing; matches! is clearer here
fn extract_entity_name_from_context(ctx: &str) -> String {
    const PREFIXES: &[&str] = &[
        "pub async fn ",
        "async fn ",
        "pub fn ",
        "fn ",
        "async function ",
        "export async function ",
        "export function ",
        "export default function ",
        "export class ",
        "function ",
        "def ",
        "class ",
        "struct ",
        "enum ",
        "impl ",
        "interface ",
        "func ",
        // NOTE: `public `, `private `, `protected ` are intentionally omitted —
        // for `"public String myMethod(int x)"` the next word is the return
        // type, not the method name. Java/C++ signatures fall through to the
        // tree-sitter stage, which correctly picks the enclosing method node.
    ];

    for prefix in PREFIXES {
        if let Some(rest) = ctx.trim().strip_prefix(prefix) {
            let name: String =
                rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            if !name.is_empty() {
                return name;
            }
        }
    }

    // Fallback: trim to first `(`, `{`, or `<`.
    let end = ctx.find(|c| matches!(c, '(' | '{' | '<')).unwrap_or(ctx.len());
    ctx[..end].trim().to_owned()
}

/// Parse the hunk's post-image with tree-sitter and return the enclosing
/// entity name for the first `+` line.
fn entity_from_hunk_ts(hunk: &Hunk, language: &str) -> Option<String> {
    let ts_language = super::code::grammar_for_language(language)?;
    let mut parser = Parser::new();
    parser.set_language(&ts_language).ok()?;

    // Reconstruct the post-image: context lines + added lines, strip sigils.
    let post_lines: Vec<&str> = hunk
        .lines
        .iter()
        .filter(|l| !l.starts_with('-'))
        .map(|l| if l.starts_with('+') || l.starts_with(' ') { &l[1..] } else { l.as_str() })
        .collect();
    let post_source = post_lines.join("\n");

    // Find the 0-indexed post-image row of the first `+` line.
    let mut post_row = 0usize;
    let mut first_added_post_row = None;
    for raw_line in &hunk.lines {
        if raw_line.starts_with('-') {
            continue; // removed lines don't appear in post-image
        }
        if raw_line.starts_with('+') && first_added_post_row.is_none() {
            first_added_post_row = Some(post_row);
        }
        post_row += 1;
    }
    // Deletion-only hunks have no addition to anchor entity lookup. `?` returns
    // `None` here so we don't silently attribute the change to whatever entity
    // happens to live at row 0 of the post-image.
    let target_row = first_added_post_row?;

    let tree = parser.parse(&post_source, None)?;
    // A partial parse can name arbitrary enclosing nodes; mirror the guard in
    // `code::compress` so we never attribute to spurious entities.
    if tree.root_node().has_error() {
        return None;
    }
    let root = tree.root_node();

    find_enclosing_entity(&root, &post_source, target_row, language)
}

/// Find the innermost named entity that spans `target_row` in the tree.
fn find_enclosing_entity(
    root: &tree_sitter::Node<'_>,
    source: &str,
    target_row: usize,
    language: &str,
) -> Option<String> {
    let mut candidates: Vec<(String, usize, usize)> = Vec::new();
    collect_named_nodes(root, source, language, &mut candidates);

    candidates
        .into_iter()
        .filter(|(_, start, end)| *start <= target_row && target_row <= *end)
        .min_by_key(|(_, start, end)| end.saturating_sub(*start))
        .map(|(name, _, _)| name)
}

fn collect_named_nodes(
    node: &tree_sitter::Node<'_>,
    source: &str,
    language: &str,
    out: &mut Vec<(String, usize, usize)>,
) {
    if let Some(name) = entity_name_from_node(node, source, language) {
        out.push((name, node.start_position().row, node.end_position().row));
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_named_nodes(&child, source, language, out);
    }
}

/// Extract a human-readable name from a function/type definition node using
/// the `name` field and source bytes.
fn entity_name_from_node(
    node: &tree_sitter::Node<'_>,
    source: &str,
    language: &str,
) -> Option<String> {
    let entity_kinds: &[&str] = match language {
        "rust" => &["function_item", "struct_item", "enum_item", "trait_item"],
        "typescript" | "tsx" | "javascript" | "jsx" => {
            &["function_declaration", "method_definition", "class_declaration"]
        }
        "python" => &["function_definition", "class_definition"],
        "go" => &["function_declaration", "method_declaration"],
        "java" => &["method_declaration", "class_declaration"],
        _ => &[],
    };

    if !entity_kinds.contains(&node.kind()) {
        return None;
    }

    let name_node = node.child_by_field_name("name")?;
    let start = name_node.start_byte();
    let end = name_node.end_byte();
    Some(source[start..end].to_owned())
}

// ---------------------------------------------------------------------------
// Change type classification
// ---------------------------------------------------------------------------

fn classify_change(hunk: &Hunk) -> String {
    let has_added = hunk.lines.iter().any(|l| l.starts_with('+'));
    let has_removed = hunk.lines.iter().any(|l| l.starts_with('-'));
    match (has_added, has_removed) {
        (true, true) => "modified".to_owned(),
        (true, false) => "added".to_owned(),
        (false, true) => "removed".to_owned(),
        (false, false) => "modified".to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Entry deduplication
// ---------------------------------------------------------------------------

fn merge_entries(entries: Vec<(String, String, String)>) -> Vec<(String, String, String)> {
    use std::collections::HashMap;
    let mut map: HashMap<(String, String), String> = HashMap::new();

    for (file, entity, change) in entries {
        let key = (file, entity);
        let existing = map.entry(key).or_insert_with(|| change.clone());
        if *existing != change {
            *existing = "modified".to_owned();
        }
    }

    let mut result: Vec<(String, String, String)> =
        map.into_iter().map(|((file, entity), change)| (file, entity, change)).collect();
    result.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    result
}

// ---------------------------------------------------------------------------
// Language detection
// ---------------------------------------------------------------------------

fn detect_language(path: &str) -> Option<String> {
    let ext = path.rsplit('.').next()?;
    super::language_from_extension(ext).map(String::from)
}

// ---------------------------------------------------------------------------
// Inline unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_entity_name_fn_prefix() {
        assert_eq!(extract_entity_name_from_context("fn my_function(x: i32)"), "my_function");
        assert_eq!(extract_entity_name_from_context("function doSomething()"), "doSomething");
        assert_eq!(extract_entity_name_from_context("def compute(a, b):"), "compute");
    }

    #[test]
    fn classify_change_variants() {
        let added_hunk = Hunk {
            header_context: String::new(),
            lines: vec![" ctx".to_owned(), "+new line".to_owned()],
        };
        assert_eq!(classify_change(&added_hunk), "added");

        let removed_hunk = Hunk {
            header_context: String::new(),
            lines: vec![" ctx".to_owned(), "-old line".to_owned()],
        };
        assert_eq!(classify_change(&removed_hunk), "removed");

        let modified_hunk = Hunk {
            header_context: String::new(),
            lines: vec!["-old".to_owned(), "+new".to_owned()],
        };
        assert_eq!(classify_change(&modified_hunk), "modified");
    }

    #[test]
    fn parse_hunk_context_extracts_function_name_hint() {
        let ctx = parse_hunk_context("@@ -5,7 +5,8 @@ fn my_function() {");
        assert_eq!(ctx, "fn my_function() {");
    }

    #[test]
    fn detect_language_from_extension() {
        assert_eq!(detect_language("src/lib.rs"), Some("rust".to_owned()));
        assert_eq!(detect_language("src/index.ts"), Some("typescript".to_owned()));
        assert_eq!(detect_language("main.py"), Some("python".to_owned()));
        assert_eq!(detect_language("README.md"), None);
    }

    #[test]
    fn merge_entries_deduplicates_to_modified() {
        let entries = vec![
            ("file.rs".to_owned(), "foo".to_owned(), "added".to_owned()),
            ("file.rs".to_owned(), "foo".to_owned(), "removed".to_owned()),
        ];
        let merged = merge_entries(entries);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].2, "modified");
    }

    #[test]
    fn full_diff_parse_returns_entity_tuples() {
        let diff = concat!(
            "diff --git a/src/lib.rs b/src/lib.rs\n",
            "index abc..def 100644\n",
            "--- a/src/lib.rs\n",
            "+++ b/src/lib.rs\n",
            "@@ -5,6 +5,7 @@ fn my_function() {\n",
            " let x = 1;\n",
            "-    let y = 2;\n",
            "+    let y = 3;\n",
            "+    let z = 4;\n",
            " x + y\n",
        );
        let results = compress(diff);
        assert!(!results.is_empty(), "should return at least one tuple");
        let (file, entity, _change) = &results[0];
        assert_eq!(file, "src/lib.rs");
        assert!(
            entity.contains("my_function") || entity == "<unknown>",
            "entity should be my_function or unknown, got: {entity}"
        );
    }
}
