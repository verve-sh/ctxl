/// Strip ANSI/VT escape sequences from `s`.
///
/// Output is byte-identical to `strip_ansi_escapes::strip(s)`.
/// The stripped byte length is what callers use for threshold decisions —
/// ANSI codes do not count toward content size.
pub fn strip(s: &str) -> Vec<u8> {
    strip_ansi_escapes::strip(s)
}
