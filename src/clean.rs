use std::path::Path;
use std::time::Duration;

// ---------------------------------------------------------------------------
// TTL parsing
// ---------------------------------------------------------------------------

/// Parse a human-readable TTL string such as `"7d"` or `"12h"` into a
/// `Duration`.  Supported suffixes: `d` (days), `h` (hours).
pub fn parse_ttl(s: &str) -> Result<Duration, crate::CtxlError> {
    if let Some(days) = s.strip_suffix('d') {
        let n: u64 = days.parse().map_err(|_| crate::CtxlError::InvalidTtl(s.to_string()))?;
        Ok(Duration::from_secs(n * 86_400))
    } else if let Some(hours) = s.strip_suffix('h') {
        let n: u64 = hours.parse().map_err(|_| crate::CtxlError::InvalidTtl(s.to_string()))?;
        Ok(Duration::from_secs(n * 3_600))
    } else {
        Err(crate::CtxlError::InvalidTtl(s.to_string()))
    }
}

// ---------------------------------------------------------------------------
// sweep
// ---------------------------------------------------------------------------

/// Remove session directories under `cache_root/ctxl/` whose `.last_used`
/// marker file has an mtime older than `ttl`.
///
/// Directories without a `.last_used` marker are skipped.
pub fn sweep(cache_root: &Path, ttl: Duration) -> Result<(), crate::CtxlError> {
    let ctxl_dir = cache_root.join("ctxl");
    if !ctxl_dir.exists() {
        return Ok(());
    }

    let global_conn: Option<rusqlite::Connection> =
        crate::global_db::global_db_path().and_then(|p| crate::global_db::open_global_db(&p).ok());

    let now = std::time::SystemTime::now();

    for entry in std::fs::read_dir(&ctxl_dir)? {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let last_used = path.join(".last_used");
        let mtime = match std::fs::metadata(&last_used) {
            Ok(m) => match m.modified() {
                Ok(t) => t,
                Err(_) => continue,
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => continue,
        };

        if let Ok(age) = now.duration_since(mtime) {
            if age > ttl {
                let store_db = path.join("store.db");
                if store_db.exists() {
                    if let Some(session_id) = path.file_name().and_then(|n| n.to_str()) {
                        if let Some(ref gconn) = global_conn {
                            if let Ok(summary) = crate::global_db::compute_session_stats(&store_db)
                            {
                                let mut summary = summary;
                                summary.session_id = session_id.to_string();
                                let _ = crate::global_db::save_session_summary(gconn, &summary);
                            }
                        }
                    }
                }

                match std::fs::remove_dir_all(&path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
    }

    Ok(())
}
