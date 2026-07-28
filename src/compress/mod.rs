pub mod ansi;
pub mod code;
pub mod diff;
pub mod grep_dedup;
pub mod grep_preview;
pub mod json;
pub mod passthrough;

/// Find the colon that separates the file path from the rest of a grep line.
///
/// Handles Windows drive-letter paths: if the first colon is at position 1
/// and followed by `\` or `/` (e.g., `C:\`), advance to the next colon.
pub(crate) fn grep_path_colon_pos(line: &str) -> Option<usize> {
    let first = line.find(':')?;
    if first == 1
        && line.as_bytes()[0].is_ascii_alphabetic()
        && line.as_bytes().get(2).is_some_and(|&b| b == b'\\' || b == b'/')
    {
        line[first + 1..].find(':').map(|off| first + 1 + off)
    } else {
        Some(first)
    }
}

/// Map a file extension to a language identifier for tree-sitter.
pub fn language_from_extension(ext: &str) -> Option<&'static str> {
    match ext {
        "rs" => Some("rust"),
        "ts" => Some("typescript"),
        "tsx" => Some("tsx"),
        "js" | "mjs" | "cjs" => Some("javascript"),
        "jsx" => Some("jsx"),
        "py" => Some("python"),
        "go" => Some("go"),
        "java" => Some("java"),
        "c" | "h" => Some("c"),
        "cpp" | "cc" | "cxx" | "hpp" => Some("cpp"),
        "rb" => Some("ruby"),
        "sh" | "bash" => Some("bash"),
        "css" => Some("css"),
        "json" => Some("json"),
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn drive_letter_path_colon_pos() {
        let line = r"C:\Users\dev\src\main.rs:42:fn main()";
        let pos = grep_path_colon_pos(line).unwrap();
        assert_eq!(&line[..pos], r"C:\Users\dev\src\main.rs");
        assert_eq!(&line[pos..pos + 1], ":");
    }

    #[test]
    fn drive_letter_forward_slash() {
        let line = "C:/Users/dev/src/main.rs:42:fn main()";
        let pos = grep_path_colon_pos(line).unwrap();
        assert_eq!(&line[..pos], "C:/Users/dev/src/main.rs");
    }

    #[test]
    fn posix_path_unchanged() {
        let line = "src/main.rs:42:fn main()";
        let pos = grep_path_colon_pos(line).unwrap();
        assert_eq!(&line[..pos], "src/main.rs");
    }

    #[test]
    fn single_letter_dir_not_treated_as_drive() {
        // 'a:42:text' — first colon at pos 1, but byte at pos 2 is '4', not '\' or '/'
        let line = "a:42:text";
        let pos = grep_path_colon_pos(line).unwrap();
        assert_eq!(pos, 1);
        assert_eq!(&line[..pos], "a");
    }

    #[test]
    fn no_colon_returns_none() {
        assert!(grep_path_colon_pos("no-colon-line").is_none());
    }

    #[test]
    fn drive_letter_content_with_colon() {
        let line = r#"C:\src\config.rs:10:let url = "http://example.com";"#;
        let pos = grep_path_colon_pos(line).unwrap();
        assert_eq!(&line[..pos], r"C:\src\config.rs");
    }

    #[test]
    fn posix_path_content_with_colon() {
        let line = r#"src/config.rs:10:let url = "http://example.com";"#;
        let pos = grep_path_colon_pos(line).unwrap();
        assert_eq!(&line[..pos], "src/config.rs");
    }
}
