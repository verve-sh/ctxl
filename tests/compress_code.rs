#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

// @ac AC-1420-01
#[test]
#[cfg(feature = "lang-rust")]
fn code_skeleton_extraction_for_rust() {
    // Verify: Given a Rust source file with 5+ functions, compress::code::compress() returns
    //         import statements, type/struct/enum declarations, and function signatures.
    // Verify: Function bodies are replaced with `// ... N lines` placeholders.
    // Verify: Output token count is <30% of input.
    //
    // Source is intentionally body-heavy (10–16 body lines per function) so the
    // compression ratio is well under the 30% AC threshold.
    let source = concat!(
        "use std::collections::HashMap;\n",
        "use std::io;\n",
        "\n",
        "struct Config {\n",
        "    limit: usize,\n",
        "    factor: f64,\n",
        "}\n",
        "\n",
        "enum Status { Ok, Warn, Fail }\n",
        "\n",
        "fn function_one(x: i32, y: i32) -> i32 {\n",
        "    let a = x + y;\n",
        "    let b = a * 2;\n",
        "    let c = b - 1;\n",
        "    let d = c + a;\n",
        "    let e = d * b;\n",
        "    let f = e - c;\n",
        "    let g = f + d;\n",
        "    let h = g * 2;\n",
        "    let i = h + a;\n",
        "    let j = i - b;\n",
        "    j\n",
        "}\n",
        "\n",
        "fn function_two(s: &str) -> usize {\n",
        "    let mut count = 0usize;\n",
        "    for c in s.chars() {\n",
        "        if c.is_alphabetic() { count += 1; }\n",
        "        if c.is_numeric() { count += 2; }\n",
        "        if c.is_whitespace() { count = count.saturating_sub(1); }\n",
        "        if c.is_uppercase() { count += 3; }\n",
        "        if c.is_lowercase() { count += 1; }\n",
        "        if c.is_ascii_punctuation() { count += 2; }\n",
        "        if c.is_control() { break; }\n",
        "        if count > 1000 { count = 1000; }\n",
        "    }\n",
        "    count\n",
        "}\n",
        "\n",
        "fn function_three(data: &[i32]) -> (i32, i32, f64) {\n",
        "    let min = *data.iter().min().unwrap_or(&0);\n",
        "    let max = *data.iter().max().unwrap_or(&0);\n",
        "    let sum: i32 = data.iter().sum();\n",
        "    let n = data.len() as f64;\n",
        "    let mean = if n > 0.0 { sum as f64 / n } else { 0.0 };\n",
        "    let variance: f64 = data.iter()\n",
        "        .map(|&v| { let d = v as f64 - mean; d * d })\n",
        "        .sum::<f64>() / n.max(1.0);\n",
        "    let _std = variance.sqrt();\n",
        "    let _range = max - min;\n",
        "    let _mid = (max + min) / 2;\n",
        "    (min, max, mean)\n",
        "}\n",
        "\n",
        "fn function_four(n: u32) -> Vec<u32> {\n",
        "    let mut result = Vec::new();\n",
        "    for i in 1..=n {\n",
        "        let mut val = i;\n",
        "        let mut steps = 0u32;\n",
        "        while val != 1 && steps < 1000 {\n",
        "            val = if val % 2 == 0 { val / 2 } else { val * 3 + 1 };\n",
        "            steps += 1;\n",
        "        }\n",
        "        result.push(steps);\n",
        "    }\n",
        "    result\n",
        "}\n",
        "\n",
        "fn function_five(rows: usize, cols: usize) -> Vec<Vec<u32>> {\n",
        "    let mut grid = vec![vec![0u32; cols]; rows];\n",
        "    for i in 0..rows {\n",
        "        for j in 0..cols {\n",
        "            grid[i][j] = if i == 0 || j == 0 {\n",
        "                1\n",
        "            } else {\n",
        "                grid[i - 1][j].saturating_add(grid[i][j - 1])\n",
        "            };\n",
        "        }\n",
        "    }\n",
        "    let _total: u32 = grid.iter().flatten().sum();\n",
        "    grid\n",
        "}\n",
    );

    let output = ctxl::compress::code::compress(source, "rust");

    // Imports and type declarations must be preserved.
    assert!(output.contains("use std::collections::HashMap"), "use declaration preserved");
    assert!(output.contains("use std::io"), "second use preserved");
    assert!(output.contains("struct Config"), "struct declaration preserved");
    assert!(output.contains("enum Status"), "enum declaration preserved");

    // Function signatures must appear.
    assert!(output.contains("fn function_one"), "fn function_one signature present");
    assert!(output.contains("fn function_two"), "fn function_two signature present");
    assert!(output.contains("fn function_three"), "fn function_three signature present");
    assert!(output.contains("fn function_four"), "fn function_four signature present");
    assert!(output.contains("fn function_five"), "fn function_five signature present");

    // Function bodies must be suppressed.
    assert!(!output.contains("let a = x + y"), "function_one body suppressed");
    assert!(!output.contains("count += 1"), "function_two body suppressed");
    assert!(!output.contains("let min ="), "function_three body suppressed");

    // Placeholder markers must be present.
    assert!(output.contains("// ..."), "body placeholder present");

    // Token count: output must be < 30% of input (using byte length as proxy).
    let compression_ratio = output.len() as f64 / source.len() as f64;
    assert!(
        compression_ratio < 0.30,
        "output should be < 30% of input length (got {compression_ratio:.2})"
    );
}

// @ac AC-1420-02
#[test]
#[cfg(feature = "lang-typescript")]
fn code_skeleton_extraction_for_typescript() {
    // Verify: Given a TypeScript file with classes, interfaces, and exported functions,
    //         compress::code::compress() returns declarations and signatures only.
    // Verify: Body suppression applies to function/method bodies.
    // Verify: Arrow functions and class methods handled.
    let source = r#"import { readFile } from 'fs';
import type { EventEmitter } from 'events';

interface Shape {
    area(): number;
    perimeter(): number;
}

class Circle implements Shape {
    constructor(private radius: number) {}

    area(): number {
        const pi = Math.PI;
        return pi * this.radius * this.radius;
    }

    perimeter(): number {
        return 2 * Math.PI * this.radius;
    }
}

function formatShape(s: Shape): string {
    const a = s.area();
    const p = s.perimeter();
    return `area=${a} perimeter=${p}`;
}

export const doubleArea = (s: Shape): number => {
    const base = s.area();
    return base * 2;
};

export function describe(s: Shape): string {
    const label = formatShape(s);
    return `Shape: ${label}`;
}
"#;

    let output = ctxl::compress::code::compress(source, "typescript");

    // Imports and interfaces preserved.
    assert!(output.contains("import"), "import preserved");
    assert!(output.contains("interface Shape"), "interface preserved");
    assert!(output.contains("class Circle"), "class declaration preserved");

    // Method signatures present.
    assert!(output.contains("area()"), "area method signature present");
    assert!(output.contains("perimeter()"), "perimeter method signature present");

    // Function signature present.
    assert!(output.contains("formatShape"), "function signature present");

    // Bodies suppressed.
    assert!(!output.contains("Math.PI"), "area body suppressed");
    assert!(!output.contains("return 2 * Math.PI"), "perimeter body suppressed");
    assert!(!output.contains("return `area="), "formatShape body suppressed");

    // Placeholder markers present.
    assert!(output.contains("// ..."), "body placeholder present");

    // Arrow functions handled.
    assert!(
        output.contains("doubleArea") || output.contains("const doubleArea"),
        "arrow function present"
    );

    // Arrow function body must be suppressed (AC-1420-02: bodies replaced by
    // placeholders, including arrow-function bodies — not just declared
    // functions / methods).
    assert!(!output.contains("const base = s.area()"), "arrow function body should be suppressed");
    assert!(!output.contains("return base * 2"), "arrow function return should be suppressed");
}

// @ac AC-1420-05
#[test]
fn unsupported_language_fallback() {
    // Verify: Given a source file in a language without a compiled grammar
    //         (e.g., Ruby when only default features), compress::code::compress()
    //         returns compress::passthrough output (head/tail preview) instead of an error.
    let source = "puts 'hello world'\ndef greet(name)\n  puts \"Hello, #{name}\"\nend\n";

    // Ruby is NOT in the default feature set, so this must fall back to passthrough.
    // (If someone runs with --features lang-ruby, that feature is tested elsewhere.)
    #[cfg(not(feature = "lang-ruby"))]
    {
        let output = ctxl::compress::code::compress(source, "ruby");
        // Passthrough returns the content (possibly head/tail trimmed for large files).
        // For a short file like this, passthrough returns the full content.
        assert!(output.contains("puts"), "passthrough should preserve source content");
        assert!(output.contains("greet"), "passthrough should preserve source content");
    }

    // For any language not in compiled grammars, no panic.
    let _out = ctxl::compress::code::compress(source, "cobol");
    let _out = ctxl::compress::code::compress(source, "fortran");
    let _out = ctxl::compress::code::compress(source, "");
}

// @ac AC-1420-06
#[test]
#[cfg(feature = "lang-rust")]
fn malformed_source_no_panic() {
    // Verify: No panic or error propagation on invalid source.
    // With has_error() gate removed, malformed source still produces no
    // false skeletons because is_function_body() requires valid
    // function_item > block pairs that don't exist in garbage input.
    let invalid_sources = [
        "fn broken( {{{ INVALID {{ SYNTAX {{{",
        "@#$%^&*()!!!",
        "fn foo() { let x = (1 + [2 * {3}]; }",
        "",
    ];

    for source in &invalid_sources {
        let output = ctxl::compress::code::compress(source, "rust");
        assert!(
            !output.contains("// ..."),
            "malformed input must not produce skeleton (source: {source:?})"
        );
    }

    let malformed = "fn { BROKEN }}}";
    let output = ctxl::compress::code::compress(malformed, "rust");
    assert!(!output.contains("// ..."), "no false skeleton from garbage");
    assert!(output.contains("BROKEN"), "source content preserved");
}
