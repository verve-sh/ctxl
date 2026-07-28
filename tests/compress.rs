#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// @ac AC-1111-03

#[test]
fn passthrough_preview_returns_head_tail_snippet_for_oversized_content() {
    // --- oversized: 100 lines, max_lines = 20 ---
    let input_100: String = (1..=100).map(|i| format!("line {i}\n")).collect();
    let input_100 = input_100.trim_end_matches('\n'); // strip trailing newline
    let result = ctxl::compress::passthrough::preview(input_100, 20);

    let out_lines: Vec<&str> = result.lines().collect();

    // first 20 lines must match input
    for (i, expected) in (1..=20).enumerate() {
        assert_eq!(out_lines[i], format!("line {expected}"), "head line {i} mismatch");
    }

    // elision marker in the middle (index 20)
    let elision = out_lines[20];
    assert!(
        elision.contains("elided") || elision.contains("..."),
        "expected elision marker at index 20, got: {elision}"
    );

    // last 20 lines must match input lines 81–100
    for (i, expected) in (81..=100).enumerate() {
        assert_eq!(out_lines[21 + i], format!("line {expected}"), "tail line {i} mismatch");
    }

    // total output lines: 20 head + 1 elision + 20 tail = 41
    assert_eq!(out_lines.len(), 41, "output should have 41 lines");

    // --- at-threshold: exactly 40 lines → full content unchanged ---
    let input_40: String = (1..=40).map(|i| format!("line {i}\n")).collect();
    let input_40 = input_40.trim_end_matches('\n');
    let result_40 = ctxl::compress::passthrough::preview(input_40, 20);
    assert_eq!(result_40, input_40, "40-line input should be returned unchanged");

    // --- under threshold: 30 lines → full content unchanged ---
    let input_30: String = (1..=30).map(|i| format!("line {i}\n")).collect();
    let input_30 = input_30.trim_end_matches('\n');
    let result_30 = ctxl::compress::passthrough::preview(input_30, 20);
    assert_eq!(result_30, input_30, "30-line input should be returned unchanged");
}

// @ac AC-1111-04
#[test]
fn ansi_strip_is_byte_accurate_with_underlying_crate() {
    let cases = [
        "\x1b[31mred text\x1b[0m",
        "\x1b[1;32mgreen bold\x1b[0m plain",
        "\x1b[2J\x1b[H",             // cursor move / clear
        "\x1b[38;5;200mpink\x1b[0m", // 256-color
        "no escapes here",
        "\x1b[0m\x1b[1m\x1b[4m\x1b[7m", // multiple SGR params
    ];

    for s in &cases {
        let expected = strip_ansi_escapes::strip(s);
        let actual = ctxl::compress::ansi::strip(s);
        assert_eq!(actual, expected, "strip({s:?}): actual {actual:?} != expected {expected:?}");
    }
}

// @ac AC-1111-05
#[test]
fn threshold_measurement_is_post_ansi_strip() {
    // Strings containing ANSI color codes: stripped length must be < raw length
    let ansi_strings =
        ["\x1b[31mHello\x1b[0m", "\x1b[1;32mbold green\x1b[0m", "\x1b[0m\x1b[1m\x1b[4m"];

    for s in &ansi_strings {
        let stripped = ctxl::compress::ansi::strip(s);
        assert!(
            stripped.len() < s.len(),
            "stripped.len()={} should be < raw.len()={} for input {s:?}",
            stripped.len(),
            s.len()
        );
    }

    // Plain string: stripped length equals raw byte length
    let plain = "hello world";
    let stripped_plain = ctxl::compress::ansi::strip(plain);
    assert_eq!(stripped_plain.len(), plain.len(), "plain string should be unchanged by stripping");
}
