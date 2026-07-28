#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ctxl::compress::grep_dedup;

// ---------------------------------------------------------------------------
// Drive-letter path tests (#1458)
// ---------------------------------------------------------------------------

#[test]
fn drive_letter_paths_group_correctly() {
    // Multiple C:\...\file.rs:N:text lines should group under the full path
    let mut lines: Vec<String> = Vec::new();
    for i in 0..5usize {
        lines.push(format!(r"C:\Users\dev\src\main.rs:{i}:fn func_{i}() {{}}"));
    }
    for i in 0..3usize {
        lines.push(format!(r"C:\Users\dev\src\lib.rs:{i}:fn helper_{i}() {{}}"));
    }

    let input = lines.join("\n");
    let output = grep_dedup::compress(&input);

    assert!(
        output.contains(r"C:\Users\dev\src\main.rs (5 matches)"),
        "main.rs should group under full Windows path with 5 matches: {output}"
    );
    assert!(
        output.contains(r"C:\Users\dev\src\lib.rs (3 matches)"),
        "lib.rs should group under full Windows path with 3 matches: {output}"
    );
    // Must NOT group under "C" (the drive letter alone)
    assert!(!output.contains("=== C ("), "should NOT produce a 'C' group: {output}");
}

#[test]
fn mixed_posix_and_drive_paths() {
    let input = [
        r"C:\project\src\main.rs:1:fn main()",
        r"C:\project\src\main.rs:2:}",
        "src/lib.rs:1:fn helper()",
        "src/lib.rs:2:}",
    ]
    .join("\n");
    let output = grep_dedup::compress(&input);

    assert!(
        output.contains(r"C:\project\src\main.rs (2 matches)"),
        "Windows path should produce correct grouping: {output}"
    );
    assert!(
        output.contains("src/lib.rs (2 matches)"),
        "POSIX path should produce correct grouping: {output}"
    );
}

// @ac AC-1421-01
#[test]
fn simhash_dedup_reduces_near_identical_lines() {
    // Verify: Given 500-line Grep output with 50 near-identical `import React from 'react'` lines across files
    // Verify: `compress::grep_dedup::compress()` returns output where duplicates are collapsed to 1 representative + count annotation
    // Verify: Total output lines < 50% of input

    // Build 500 lines: 10 files × 50 lines each, all near-identical `import React from 'react'`
    // (slight variations to exercise SimHash rather than exact equality)
    let mut input_lines: Vec<String> = Vec::new();
    for file_idx in 0..10usize {
        for line_idx in 0..50usize {
            // Vary quotes/spacing slightly so these are "near-identical" not byte-identical
            let variant = match line_idx % 3 {
                0 => format!("src/comp{file_idx}.tsx:{line_idx}:import React from 'react'"),
                1 => format!("src/comp{file_idx}.tsx:{line_idx}:import React from \"react\""),
                _ => format!("src/comp{file_idx}.tsx:{line_idx}: import React from 'react'"),
            };
            input_lines.push(variant);
        }
    }
    assert_eq!(input_lines.len(), 500, "should start with 500 input lines");

    let input = input_lines.join("\n");
    let output = grep_dedup::compress(&input);

    let output_lines = output.lines().count();
    assert!(output_lines < 250, "output ({output_lines} lines) should be < 50% of 500 input lines");
    // Each file group's near-identical lines should be collapsed to one representative + annotation
    assert!(output.contains("[×"), "output should contain collapse annotations");
}

// @ac AC-1421-02
#[test]
fn grep_dedup_groups_by_file_with_match_counts() {
    // Verify: Given Grep `content` mode output, `compress::grep_dedup::compress()` returns results grouped by file
    // Verify: Sorted by match count descending
    // Verify: Each file group shows the count and representative matches

    // lib.rs: 15 matches, utils.rs: 7 matches, main.rs: 3 matches
    let mut lines: Vec<String> = Vec::new();
    for i in 0..15usize {
        lines.push(format!("src/lib.rs:{i}:fn process_{i}() {{}}"));
    }
    for i in 0..3usize {
        lines.push(format!("src/main.rs:{i}:fn main_{i}() {{}}"));
    }
    for i in 0..7usize {
        lines.push(format!("src/utils.rs:{i}:fn util_{i}() {{}}"));
    }

    let input = lines.join("\n");
    let output = grep_dedup::compress(&input);

    // Each file should appear as a group header
    assert!(output.contains("src/lib.rs"), "lib.rs should appear");
    assert!(output.contains("src/main.rs"), "main.rs should appear");
    assert!(output.contains("src/utils.rs"), "utils.rs should appear");

    // Headers should include match counts
    assert!(
        output.contains("src/lib.rs (15 matches)"),
        "lib.rs header should show 15 matches: {output}"
    );
    assert!(output.contains("src/utils.rs (7 matches)"), "utils.rs header should show 7 matches");
    assert!(output.contains("src/main.rs (3 matches)"), "main.rs header should show 3 matches");

    // lib.rs (highest count) should appear before utils.rs, which before main.rs
    let pos_lib = output.find("src/lib.rs").expect("lib.rs in output");
    let pos_utils = output.find("src/utils.rs").expect("utils.rs in output");
    let pos_main = output.find("src/main.rs").expect("main.rs in output");
    assert!(pos_lib < pos_utils, "lib.rs (15) should come before utils.rs (7)");
    assert!(pos_utils < pos_main, "utils.rs (7) should come before main.rs (3)");
}

// @ac AC-1421-06
#[test]
fn empty_grep_input_returns_empty_string() {
    // Verify: Given empty Grep output (zero lines), `compress::grep_dedup::compress()` returns empty string
    // Verify: No crash, no allocation waste

    assert_eq!(grep_dedup::compress(""), "", "empty string should return empty string");
    assert_eq!(grep_dedup::compress("\n\n\n"), "", "whitespace-only should return empty string");
    assert_eq!(grep_dedup::compress("   "), "", "spaces-only should return empty string");
}
