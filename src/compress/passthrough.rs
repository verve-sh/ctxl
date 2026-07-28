/// Return a head/tail preview of `input` capped at `max_lines` per side.
///
/// If `input` has more than `2 * max_lines` lines the function returns:
///   - the first `max_lines` lines
///   - an elision marker indicating how many lines were omitted
///   - the last `max_lines` lines
///
/// If `input` has `<= 2 * max_lines` lines, the full content is returned
/// unchanged.
pub fn preview(input: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = input.lines().collect();
    let total = lines.len();
    let threshold = 2 * max_lines;

    if total <= threshold {
        return input.to_owned();
    }

    let omitted = total - threshold;
    let head = lines[..max_lines].join("\n");
    let tail = lines[total - max_lines..].join("\n");

    format!("{head}\n... [{omitted} lines elided] ...\n{tail}")
}
