use std::io::Write;

/// Write a diagnostic line to `/tmp/ctxl-debug.log` when `CTXL_DEBUG=1`.
///
/// No-op when the env var is absent or set to any other value.
/// Failures are silently discarded — this is diagnostic-only.
pub fn debug_log(msg: &str) {
    if std::env::var("CTXL_DEBUG").ok().as_deref() == Some("1") {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(
            std::path::Path::new(&std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into()))
                .join("ctxl-debug.log"),
        ) {
            let _ = writeln!(f, "{msg}");
        }
    }
}
