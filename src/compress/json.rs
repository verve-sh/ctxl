//! JSON flattening compressor via flatten-json-object.
//!
//! Converts nested JSON objects to `path = value` lines.
//! Arrays use `items[0].name` indexing. Null/empty values are omitted.
//! Falls back to passthrough on invalid JSON.

use flatten_json_object::{ArrayFormatting, Flattener};

/// Maximum lines shown when falling back to passthrough preview.
const PASSTHROUGH_MAX_LINES: usize = 20;

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Format a scalar JSON value for display.
fn format_scalar(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => "null".to_string(),
        // Arrays / objects should not survive flattening — stringify as fallback.
        other => other.to_string(),
    }
}

/// Return `true` when a value should be omitted (null or empty string).
fn should_omit(v: &serde_json::Value) -> bool {
    if v.is_null() {
        return true;
    }
    if let Some(s) = v.as_str() {
        return s.is_empty();
    }
    false
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Flatten a JSON string into `path = value` lines.
///
/// - Keys are joined with `.` (nested objects) or `[N]` (arrays).
/// - Null and empty-string values are omitted.
/// - On parse failure or non-object input, falls back to passthrough preview.
pub fn compress(input: &str) -> String {
    let value: serde_json::Value = match serde_json::from_str(input) {
        Ok(v) => v,
        Err(_) => {
            return crate::compress::passthrough::preview(input, PASSTHROUGH_MAX_LINES);
        }
    };

    if !value.is_object() {
        return crate::compress::passthrough::preview(input, PASSTHROUGH_MAX_LINES);
    }

    let flattener = Flattener::new()
        .set_key_separator(".")
        .set_array_formatting(ArrayFormatting::Surrounded {
            start: "[".to_string(),
            end: "]".to_string(),
        })
        .set_preserve_empty_arrays(false)
        .set_preserve_empty_objects(false);

    let flat = match flattener.flatten(&value) {
        Ok(f) => f,
        Err(_) => {
            return crate::compress::passthrough::preview(input, PASSTHROUGH_MAX_LINES);
        }
    };

    let obj = match flat.as_object() {
        Some(o) => o,
        None => {
            return crate::compress::passthrough::preview(input, PASSTHROUGH_MAX_LINES);
        }
    };

    let mut lines: Vec<String> = obj
        .iter()
        .filter(|(_, v)| !should_omit(v))
        .map(|(k, v)| format!("{k} = {}", format_scalar(v)))
        .collect();

    // Sort for stable output.
    lines.sort();
    lines.join("\n")
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flattens_nested_object() {
        let json = r#"{"a": {"b": "c"}}"#;
        let out = compress(json);
        assert_eq!(out, "a.b = c");
    }

    #[test]
    fn array_uses_bracket_indexing() {
        let json = r#"{"items": [{"name": "foo"}]}"#;
        let out = compress(json);
        assert!(out.contains("items[0].name = foo"), "got: {out}");
    }

    #[test]
    fn null_values_omitted() {
        let json = r#"{"a": null, "b": "keep"}"#;
        let out = compress(json);
        assert!(!out.contains("null"), "null should be omitted: {out}");
        assert!(out.contains("b = keep"), "non-null should be present: {out}");
    }

    #[test]
    fn empty_string_values_omitted() {
        let json = r#"{"a": "", "b": "value"}"#;
        let out = compress(json);
        assert!(!out.contains("a ="), "empty string should be omitted: {out}");
        assert!(out.contains("b = value"));
    }

    #[test]
    fn invalid_json_returns_passthrough() {
        let bad = "{invalid json}";
        let out = compress(bad);
        let expected = crate::compress::passthrough::preview(bad, 20);
        assert_eq!(out, expected);
    }
}
