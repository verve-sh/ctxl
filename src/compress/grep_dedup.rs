//! SimHash near-duplicate dedup for Grep output.
//!
//! Collapses near-identical lines (Hamming distance ≤ 3) to a representative
//! + count annotation, then groups results by file sorted by match count descending.

use simhash::{hamming_distance, simhash};

/// Hamming distance threshold for treating two lines as near-duplicates.
const HAMMING_THRESHOLD: u32 = 3;

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Extract the file path from a content-mode grep line (`file:line_no:text`).
fn extract_file_path(line: &str) -> Option<String> {
    let end = super::grep_path_colon_pos(line)?;
    let path = &line[..end];
    if path.is_empty() {
        None
    } else {
        Some(path.to_string())
    }
}

/// Extract the match content from a grep line, stripping the `file:lineno:` prefix.
///
/// Handles both `file:lineno:content` and `file:content` formats.
/// The returned slice is used for similarity hashing — line numbers are metadata
/// and should not affect the dedup comparison.
fn extract_match_content(line: &str) -> &str {
    // Skip file path segment (up to first path-separating colon).
    let after_file = match super::grep_path_colon_pos(line) {
        Some(pos) => &line[pos + 1..],
        None => return line,
    };
    // If the next segment is all ASCII digits, it's a line number — skip it too.
    if let Some(colon2) = after_file.find(':') {
        if after_file[..colon2].bytes().all(|b| b.is_ascii_digit()) {
            return &after_file[colon2 + 1..];
        }
    }
    after_file
}

/// Greedy SimHash clustering: collapse near-identical lines into
/// `(representative, cluster_size)` pairs preserving insertion order.
///
/// Similarity is measured on the match-content portion only (file path and line
/// number are stripped before hashing) so lines like `a.rs:1:foo` and `a.rs:2:foo`
/// hash identically.
fn simhash_dedup(lines: &[String]) -> Vec<(String, usize)> {
    // Each cluster: (hash_of_representative_content, representative_line, count)
    let mut clusters: Vec<(u64, String, usize)> = Vec::new();

    for line in lines {
        let content = extract_match_content(line);
        let h = simhash(content);
        let mut found = false;
        for (cluster_hash, _, count) in &mut clusters {
            if hamming_distance(h, *cluster_hash) <= HAMMING_THRESHOLD {
                *count += 1;
                found = true;
                break;
            }
        }
        if !found {
            clusters.push((h, line.clone(), 1));
        }
    }

    clusters.into_iter().map(|(_, rep, count)| (rep, count)).collect()
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Compress grep content-mode output by SimHash near-duplicate deduplication.
///
/// Each line is expected in `file:line_number:content` (or `file:content`) format.
/// Lines are grouped by file, SimHash-deduped within each group (Hamming ≤ 3),
/// then the groups are sorted by match count descending.
///
/// Each group header shows the file name and match count.
/// Collapsed duplicates are annotated with `[×N]`.
///
/// Returns an empty string for empty input.
pub fn compress(input: &str) -> String {
    if input.trim().is_empty() {
        return String::new();
    }

    // Collect lines grouped by file, preserving first-seen file order.
    let mut file_order: Vec<String> = Vec::new();
    let mut file_lines: std::collections::HashMap<String, Vec<String>> = Default::default();

    for line in input.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let file = extract_file_path(trimmed).unwrap_or_else(|| "<unknown>".to_string());
        if !file_lines.contains_key(&file) {
            file_order.push(file.clone());
        }
        file_lines.entry(file).or_default().push(trimmed.to_string());
    }

    if file_order.is_empty() {
        return String::new();
    }

    // Sort file groups by match count descending, then by file name ascending.
    let mut file_counts: Vec<(String, usize)> =
        file_order.iter().map(|f| (f.clone(), file_lines[f].len())).collect();
    file_counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    // Build output.
    let mut out = String::new();
    for (file, count) in &file_counts {
        let plural = if *count == 1 { "match" } else { "matches" };
        out.push_str(&format!("=== {file} ({count} {plural}) ===\n"));

        let lines = &file_lines[file];
        let deduped = simhash_dedup(lines);
        for (rep, cluster_size) in &deduped {
            if *cluster_size > 1 {
                out.push_str(&format!("{rep}  [×{cluster_size}]\n"));
            } else {
                out.push_str(&format!("{rep}\n"));
            }
        }
        // Blank separator between groups.
        out.push('\n');
    }

    out.trim_end().to_string()
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::expect_used)] // test code — intentional panics on assertion failure
mod tests {
    use super::*;

    #[test]
    fn empty_input_returns_empty_string() {
        assert_eq!(compress(""), "");
        assert_eq!(compress("   \n  \n"), "");
    }

    #[test]
    fn identical_lines_collapse_to_one() {
        let lines: Vec<String> =
            (0..10).map(|i| format!("src/a.ts:{}:import React from 'react'", i)).collect();
        let input = lines.join("\n");
        let out = compress(&input);
        assert!(out.contains("[×10]"), "10 identical lines should collapse: {out}");
    }

    #[test]
    fn single_line_no_annotation() {
        let input = "src/main.rs:1:fn main() {}";
        let out = compress(input);
        assert!(!out.contains("[×"), "single line should not be annotated: {out}");
    }

    #[test]
    fn groups_sorted_descending_by_match_count() {
        let lines_a: Vec<String> = (0..3).map(|i| format!("src/a.ts:{}:foo", i)).collect();
        let lines_b: Vec<String> = (0..10).map(|i| format!("src/b.ts:{}:bar", i)).collect();
        let input = [lines_a, lines_b].concat().join("\n");
        let out = compress(&input);
        let pos_b = out.find("src/b.ts").expect("b.ts should appear");
        let pos_a = out.find("src/a.ts").expect("a.ts should appear");
        assert!(pos_b < pos_a, "b.ts (10 matches) should appear before a.ts (3 matches)");
    }
}
