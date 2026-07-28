#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ctxl::compress::json;

// @ac AC-1421-03
#[test]
fn json_flattening_produces_path_value_lines() {
    // Verify: Given a 3-level nested JSON object (GitHub API response shape), `compress::json::compress()` returns `path = value` lines
    // Verify: Arrays use `items[0].name` indexing
    // Verify: Null/empty values are omitted

    // GitHub API-shaped 3-level nested JSON with arrays and nulls
    let input = r#"{
        "repository": {
            "name": "verve",
            "owner": {
                "login": "AgentCTO"
            },
            "visibility": "private"
        },
        "items": [
            {"name": "issue-1", "state": "open"},
            {"name": "issue-2", "state": "closed"}
        ],
        "total_count": 2,
        "incomplete_results": false,
        "description": null,
        "empty_field": ""
    }"#;

    let output = json::compress(input);

    // Should produce `path = value` lines
    assert!(output.contains(" = "), "output should contain path = value lines: {output}");

    // 3-level nesting: repository.owner.login
    assert!(
        output.contains("repository.owner.login = AgentCTO"),
        "3-level nesting should be flattened: {output}"
    );

    // 2-level nesting: repository.name
    assert!(
        output.contains("repository.name = verve"),
        "2-level nesting should be flattened: {output}"
    );

    // Array indexing: items[0].name
    assert!(output.contains("items[0].name = issue-1"), "arrays should use [N] indexing: {output}");
    assert!(
        output.contains("items[1].name = issue-2"),
        "second array element should appear: {output}"
    );

    // Scalar fields
    assert!(output.contains("total_count = 2"), "scalar should appear: {output}");
    assert!(output.contains("incomplete_results = false"), "bool should appear: {output}");

    // Null and empty values should be omitted
    assert!(!output.contains("description"), "null value should be omitted: {output}");
    assert!(!output.contains("empty_field"), "empty string value should be omitted: {output}");
}

// @ac AC-1421-05
#[test]
fn invalid_json_falls_back_to_passthrough() {
    // Verify: Given a string that is not valid JSON (starts with `{` but has syntax errors), `compress::json::compress()` returns `compress::passthrough` output instead of an error

    let invalid = "{this is not: valid JSON, missing quotes}";
    let output = json::compress(invalid);

    // Should not panic and should return the passthrough preview
    let expected = ctxl::compress::passthrough::preview(invalid, 20);
    assert_eq!(output, expected, "invalid JSON should fall back to passthrough preview");

    // The output should contain the original input (it's short enough to pass through fully)
    assert!(
        output.contains("this is not"),
        "passthrough output should contain original content: {output}"
    );
}
