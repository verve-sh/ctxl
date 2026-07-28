//! Structural code compression via tree-sitter AST analysis.
//!
//! Extracts imports, type declarations, and function signatures from source
//! files.  Function bodies are replaced with `// ... N lines` placeholders,
//! typically reducing token count to < 30 % of the original.
//!
//! Falls back to [`super::passthrough::preview`] when:
//! - the requested language grammar is not compiled in (feature-gated)
//! - tree-sitter fails to produce any parse tree at all

use tree_sitter::{Language, Parser};

/// Head/tail lines used for passthrough fallback.
const PASSTHROUGH_LINES: usize = 20;

/// Compress `source` to a code skeleton for the given `language`.
///
/// `language` should be a short lower-case identifier: `"rust"`,
/// `"typescript"`, `"tsx"`, `"javascript"`, `"jsx"`, `"python"`, `"go"`,
/// `"java"`, `"c"`, `"cpp"`, `"ruby"`, `"bash"`, `"css"`, or `"json"`.
///
/// Returns a skeleton string with function bodies replaced by
/// `// ... N lines` comments.  Falls back to
/// [`super::passthrough::preview`] for unsupported languages or malformed
/// sources.
pub fn compress(source: &str, language: &str) -> String {
    // AC-1420-05: no grammar compiled → passthrough.
    let Some(ts_language) = grammar_for_language(language) else {
        return super::passthrough::preview(source, PASSTHROUGH_LINES);
    };

    let mut parser = Parser::new();
    if parser.set_language(&ts_language).is_err() {
        return super::passthrough::preview(source, PASSTHROUGH_LINES);
    }

    let Some(tree) = parser.parse(source, None) else {
        return super::passthrough::preview(source, PASSTHROUGH_LINES);
    };

    let body_ranges = collect_function_bodies(&tree.root_node(), language);
    render_skeleton(source, &body_ranges)
}

// ---------------------------------------------------------------------------
// Grammar selection (feature-gated) — public so compress::diff can share it
// ---------------------------------------------------------------------------

/// Return the tree-sitter [`Language`] for `language`, or `None` when the
/// corresponding Cargo feature is not compiled in.
pub fn grammar_for_language(language: &str) -> Option<Language> {
    match language {
        #[cfg(feature = "lang-rust")]
        "rust" => Some(tree_sitter_rust::language()),

        #[cfg(feature = "lang-typescript")]
        "typescript" => Some(tree_sitter_typescript::language_typescript()),

        #[cfg(feature = "lang-typescript")]
        "tsx" => Some(tree_sitter_typescript::language_tsx()),

        #[cfg(feature = "lang-javascript")]
        "javascript" | "jsx" => Some(tree_sitter_javascript::language()),

        #[cfg(feature = "lang-python")]
        "python" => Some(tree_sitter_python::language()),

        #[cfg(feature = "lang-go")]
        "go" => Some(tree_sitter_go::language()),

        #[cfg(feature = "lang-java")]
        "java" => Some(tree_sitter_java::language()),

        #[cfg(feature = "lang-c")]
        "c" => Some(tree_sitter_c::language()),

        #[cfg(feature = "lang-cpp")]
        "cpp" | "c++" => Some(tree_sitter_cpp::language()),

        #[cfg(feature = "lang-ruby")]
        "ruby" => Some(tree_sitter_ruby::language()),

        #[cfg(feature = "lang-bash")]
        "bash" | "sh" => Some(tree_sitter_bash::language()),

        #[cfg(feature = "lang-css")]
        "css" => Some(tree_sitter_css::language()),

        #[cfg(feature = "lang-json")]
        "json" => Some(tree_sitter_json::language()),

        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Body range collection
// ---------------------------------------------------------------------------

/// Row range of a function body to suppress.
#[derive(Debug)]
struct BodyRange {
    /// 0-indexed row of the opening `{` (or Python `:`).
    open_row: usize,
    /// 0-indexed row of the closing `}`.
    close_row: usize,
}

/// Collect all function body ranges that should be suppressed.
///
/// Non-recursive: once a body range is found, its interior is not scanned for
/// nested bodies (they are suppressed along with the outer body).
fn collect_function_bodies(root: &tree_sitter::Node<'_>, language: &str) -> Vec<BodyRange> {
    let mut out = Vec::new();
    collect_bodies_rec(root, language, false, &mut out);
    out.sort_by_key(|r| r.open_row);
    out
}

fn collect_bodies_rec(
    node: &tree_sitter::Node<'_>,
    language: &str,
    inside_body: bool,
    out: &mut Vec<BodyRange>,
) {
    if inside_body {
        // Nested content is already suppressed by the outer range.
        return;
    }

    if is_function_body(node, language) {
        // Python's `block` node starts at the first indented statement, not
        // the `def`. Anchor `open_row` to the enclosing `function_definition`
        // so the placeholder lands on the `def` line (matching brace-language
        // output, where `open_row` is the `{`). This also makes the
        // `close_row > open_row + 1` guard pass for 2-statement bodies that
        // would otherwise be misclassified as "no inner lines".
        let open_row = if language == "python" {
            node.parent().map_or_else(|| node.start_position().row, |p| p.start_position().row)
        } else {
            node.start_position().row
        };
        let close_row = node.end_position().row;
        // Only suppress if there is at least one inner line.
        if close_row > open_row + 1 {
            out.push(BodyRange { open_row, close_row });
        }
        // Do not recurse into the body.
        return;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_bodies_rec(&child, language, false, out);
    }
}

/// Return `true` when `node` is a function body node whose content should be
/// suppressed in the skeleton output.
fn is_function_body(node: &tree_sitter::Node<'_>, language: &str) -> bool {
    let kind = node.kind();
    let parent_kind = node.parent().map(|p| p.kind()).unwrap_or("");

    match language {
        "rust" => kind == "block" && parent_kind == "function_item",

        "typescript" | "tsx" | "javascript" | "jsx" => {
            kind == "statement_block"
                && matches!(
                    parent_kind,
                    "function_declaration"
                        | "method_definition"
                        | "function"
                        | "arrow_function"
                        | "generator_function_declaration"
                        | "generator_function"
                )
        }

        "python" => kind == "block" && parent_kind == "function_definition",

        "go" => {
            kind == "block" && matches!(parent_kind, "function_declaration" | "method_declaration")
        }

        "java" => {
            kind == "block"
                && matches!(parent_kind, "method_declaration" | "constructor_declaration")
        }

        "c" | "cpp" | "c++" => kind == "compound_statement" && parent_kind == "function_definition",

        "ruby" => kind == "body_statement" && parent_kind == "method",

        "bash" | "sh" => kind == "compound_statement" && parent_kind == "function_definition",

        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Skeleton rendering
// ---------------------------------------------------------------------------

/// Render `source` with function bodies replaced by `// ... N lines`.
fn render_skeleton(source: &str, body_ranges: &[BodyRange]) -> String {
    if body_ranges.is_empty() {
        return source.to_owned();
    }

    let lines: Vec<&str> = source.lines().collect();

    // Build suppression sets.
    let mut suppressed: std::collections::HashSet<usize> = Default::default();
    let mut placeholder: std::collections::HashMap<usize, usize> = Default::default();

    for range in body_ranges {
        if range.close_row > range.open_row + 1 {
            let inner_count = range.close_row - range.open_row - 1;
            for row in (range.open_row + 1)..range.close_row {
                suppressed.insert(row);
            }
            placeholder.insert(range.open_row, inner_count);
        }
    }

    let mut out: Vec<String> = Vec::with_capacity(lines.len());

    for (i, &line) in lines.iter().enumerate() {
        if suppressed.contains(&i) {
            continue;
        }
        if let Some(&count) = placeholder.get(&i) {
            out.push(format!("{line} // ... {count} lines"));
        } else {
            out.push(line.to_owned());
        }
    }

    out.join("\n")
}

// ---------------------------------------------------------------------------
// Inline unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "lang-rust")]
    #[test]
    fn rust_skeleton_suppresses_body() {
        let src = "fn add(a: i32, b: i32) -> i32 {\n    a + b\n}";
        let out = compress(src, "rust");
        assert!(out.contains("fn add"), "signature present");
        assert!(!out.contains("a + b"), "body suppressed");
        assert!(out.contains("// ..."), "placeholder present");
    }

    #[cfg(feature = "lang-typescript")]
    #[test]
    fn typescript_skeleton_suppresses_method_body() {
        let src = "class Foo {\n  bar(x: number): number {\n    return x + 1;\n  }\n}";
        let out = compress(src, "typescript");
        assert!(out.contains("bar"), "method signature present");
        assert!(!out.contains("return x + 1"), "body suppressed");
    }

    #[test]
    fn unsupported_language_passthrough() {
        // "cobol" is not any feature → passthrough
        let src = "MOVE 1 TO X.";
        let out = compress(src, "cobol");
        assert_eq!(out, src);
    }

    #[cfg(feature = "lang-rust")]
    #[test]
    fn malformed_rust_no_false_skeleton() {
        let src = "fn foo( {{{ BROKEN {{ SYNTAX";
        let out = compress(src, "rust");
        // No valid function bodies → no skeleton placeholders.
        assert!(!out.contains("// ..."), "malformed source must not produce a skeleton");
        assert!(out.contains("BROKEN"), "source content preserved");
    }
}
