#![allow(missing_docs)] // Internal CLI tooling — doc coverage deferred

pub mod cache;
pub mod calls;
pub mod clean;
pub mod compress;
pub mod db;
pub mod debug;
pub mod doctor;
pub mod error;
pub mod global_db;
pub mod index;
pub mod intercept;
pub mod intercept_grep;
pub mod payload;
pub mod record;
pub mod retrieve;
pub mod store;

pub use error::CtxlError;

/// Parse an environment variable with a fallback default.
///
/// Returns `default` if the variable is unset or fails to parse.
pub(crate) fn env_parse<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// Sanitize a user query for FTS5 phrase matching.
///
/// Wraps in double quotes for phrase mode, escapes embedded quotes,
/// and strips FTS5 operators that could break out of the phrase literal.
///
/// Supports trailing `*` for prefix matching: `grep*` becomes `"grep"*`
/// which is valid FTS5 prefix syntax.
pub fn sanitize_fts5_query(query: &str) -> String {
    let trimmed = query.trim();
    let (stem, is_prefix) = match trimmed.strip_suffix('*') {
        Some(stripped) => (stripped, true),
        None => (trimmed, false),
    };
    let cleaned: String = stem.replace('"', "\"\"").replace(['+', '^'], "");
    let quoted = format!("\"{}\"", cleaned.trim());
    if is_prefix {
        format!("{quoted}*")
    } else {
        quoted
    }
}

/// Regex metacharacter patterns that FTS5 does not support.
const REGEX_PATTERNS: &[&str] =
    &["\\(", "\\)", "\\|", "\\+", "\\{", "\\}", "[", "]", "\\d", "\\w", "\\s", "\\b"];

/// Detect regex metacharacters in a query string.
///
/// Returns `Some(suggestion)` with a corrected syntax suggestion when regex
/// syntax is detected, or `None` when the query is clean.
pub fn detect_regex_metacharacters(query: &str) -> Option<String> {
    let patterns_found: Vec<&str> =
        REGEX_PATTERNS.iter().filter(|p| query.contains(**p)).copied().collect();

    if patterns_found.is_empty() {
        return None;
    }

    let found_display = patterns_found.join(", ");

    let mut suggestion = query.to_string();
    for pat in &["\\(", "\\)", "\\|", "\\+", "\\{", "\\}", "\\d", "\\w", "\\s", "\\b"] {
        suggestion = suggestion.replace(pat, " ");
    }
    suggestion = suggestion.replace(['[', ']'], "");
    let suggestion = suggestion.split_whitespace().collect::<Vec<_>>().join(" ");

    if query.contains("\\|") {
        let parts: Vec<&str> = query.split("\\|").map(|s| s.trim()).collect();
        let cleaned: Vec<String> = parts
            .iter()
            .map(|p| {
                let mut s = p.to_string();
                for pat in &["\\(", "\\)", "\\+", "\\{", "\\}", "\\d", "\\w", "\\s", "\\b"] {
                    s = s.replace(pat, "");
                }
                s = s.replace(['[', ']'], "");
                s.trim().to_string()
            })
            .filter(|s| !s.is_empty())
            .collect();
        if cleaned.len() > 1 {
            let suggestions: Vec<String> =
                cleaned.iter().map(|c| format!("ctxl search <handle> \"{c}\"")).collect();
            return Some(format!(
                "query contains regex syntax ({found_display}) which FTS5 does not support. Try: {}",
                suggestions.join(" or ")
            ));
        }
    }

    if suggestion.is_empty() {
        Some(format!("query contains regex syntax ({found_display}) which FTS5 does not support"))
    } else {
        Some(format!(
            "query contains regex syntax ({found_display}) which FTS5 does not support. Try: ctxl search <handle> \"{suggestion}\""
        ))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_fts5_plain() {
        assert_eq!(sanitize_fts5_query("hello"), "\"hello\"");
    }

    #[test]
    fn sanitize_fts5_prefix() {
        assert_eq!(sanitize_fts5_query("grep*"), "\"grep\"*");
    }

    #[test]
    fn detect_regex_clean() {
        assert!(detect_regex_metacharacters("normal query").is_none());
    }

    #[test]
    fn detect_regex_brackets() {
        assert!(detect_regex_metacharacters("[a-z]+").is_some());
    }
}
