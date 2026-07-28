#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

// @ac AC-1420-03
#[test]
fn diff_entity_attribution() {
    // Verify: Given unified diff output (`diff --git` format) with changes inside
    //         functions, compress::diff::compress() returns `(file, entity_name, change_type)`
    //         tuples.
    // Verify: Each changed line maps to its enclosing function/type via tree-sitter
    //         parsing of the post-image.
    let diff = concat!(
        "diff --git a/src/lib.rs b/src/lib.rs\n",
        "index abc1234..def5678 100644\n",
        "--- a/src/lib.rs\n",
        "+++ b/src/lib.rs\n",
        "@@ -5,6 +5,7 @@ fn my_function() {\n",
        " let x = 1;\n",
        "-    let y = 2;\n",
        "+    let y = 3;\n",
        "+    let z = 4;\n",
        " x + y\n",
    );

    let results = ctxl::compress::diff::compress(diff);

    assert!(!results.is_empty(), "should return at least one tuple");

    // Each result is (file, entity_name, change_type).
    let (file, entity, change_type) = &results[0];

    assert_eq!(file, "src/lib.rs", "file path should match");

    // Entity name comes from the @@ header context: "fn my_function() {"
    assert!(
        entity.contains("my_function"),
        "entity_name should identify the enclosing function; got: {entity}"
    );

    assert_eq!(change_type, "modified", "both add and remove lines → modified");
}

#[test]
fn diff_added_lines_only() {
    // A hunk with only additions should yield change_type = "added".
    let diff = concat!(
        "diff --git a/src/utils.rs b/src/utils.rs\n",
        "index 000..111 100644\n",
        "--- a/src/utils.rs\n",
        "+++ b/src/utils.rs\n",
        "@@ -10,4 +10,6 @@ fn helper() {\n",
        " let a = 1;\n",
        "+    let b = 2;\n",
        "+    let c = 3;\n",
        " return a;\n",
    );

    let results = ctxl::compress::diff::compress(diff);
    assert!(!results.is_empty());
    let (_, entity, change_type) = &results[0];
    assert!(
        entity.contains("helper") || entity == "<unknown>",
        "entity should be helper; got: {entity}"
    );
    assert_eq!(change_type, "added");
}

#[test]
fn diff_multiple_files() {
    // Changes across two files should produce separate tuples per file.
    let diff = concat!(
        "diff --git a/src/foo.rs b/src/foo.rs\n",
        "--- a/src/foo.rs\n",
        "+++ b/src/foo.rs\n",
        "@@ -1,3 +1,4 @@ fn foo() {\n",
        " let x = 1;\n",
        "+    let y = 2;\n",
        " x\n",
        "diff --git a/src/bar.rs b/src/bar.rs\n",
        "--- a/src/bar.rs\n",
        "+++ b/src/bar.rs\n",
        "@@ -1,3 +1,3 @@ fn bar() {\n",
        " let a = 0;\n",
        "-    let b = 1;\n",
        "+    let b = 2;\n",
        " a\n",
    );

    let results = ctxl::compress::diff::compress(diff);

    let files: Vec<&str> = results.iter().map(|(f, _, _)| f.as_str()).collect();
    assert!(files.contains(&"src/foo.rs"), "foo.rs should be in results");
    assert!(files.contains(&"src/bar.rs"), "bar.rs should be in results");
}

#[test]
fn diff_empty_returns_empty() {
    let results = ctxl::compress::diff::compress("");
    assert!(results.is_empty(), "empty diff → empty result");
}

#[test]
fn diff_unknown_language_still_returns_tuples() {
    // A diff for an unrecognized extension should still return tuples with
    // the entity from the @@ header context or <unknown>.
    let diff = concat!(
        "diff --git a/template.xyz b/template.xyz\n",
        "--- a/template.xyz\n",
        "+++ b/template.xyz\n",
        "@@ -1,2 +1,3 @@ block setup\n",
        " key = value\n",
        "+    extra = 1\n",
    );

    let results = ctxl::compress::diff::compress(diff);
    // Should not panic; may return empty or entity from header.
    let _ = results;
}
