use std::path::{Path, PathBuf};

use rusqlite::Connection;

#[derive(Debug, serde::Serialize)]
pub struct DoctorReport {
    pub binary_path: Option<PathBuf>,
    pub version: &'static str,
    pub session_id: Option<String>,
    pub db_path: Option<PathBuf>,
    pub db_exists: bool,
    pub schema_version: Option<i32>,
    pub handle_count: Option<i64>,
    pub calls_count: Option<i64>,
    pub intercepted_count: Option<i64>,
    pub retrieved_count: Option<i64>,
    pub retrieval_total: Option<i64>,
    pub hook_enabled: bool,
    pub disabled_file: Option<PathBuf>,
    pub norecord: bool,
    pub thresholds: Thresholds,
    pub jq_available: bool,
    pub cache_root: PathBuf,
    pub liveness: Liveness,
}

/// Warn once this many consecutive recent sessions produced zero intercept handles.
const LIVENESS_QUIET_WARN: usize = 5;
/// Bound the scan — enough history to distinguish "dead hook" from "quiet week".
const LIVENESS_SCAN_CAP: usize = 30;

#[derive(Debug, serde::Serialize)]
pub struct Liveness {
    /// Age of the newest session DB containing intercept handles (mtime proxy).
    pub last_interception_days_ago: Option<u64>,
    /// Consecutive recent sessions (newest first) with zero `b_`/`g_` handles.
    pub recent_quiet_sessions: usize,
    /// Sessions actually examined (readable DBs, excluding current + `test-*`).
    pub scanned_sessions: usize,
    pub warning: Option<String>,
}

/// Scan recent session DBs (newest first, by `store.db` mtime) and report
/// whether interception is producing handles at all.
///
/// The hook chain fails open by design, so a broken link (missing binary,
/// dead registration, env drift) is invisible in-session — it looks exactly
/// like every command staying under threshold. Zero handles across many
/// consecutive real sessions is the observable symptom; this surfaces it.
///
/// `exclude` is the just-started session (legitimately empty). `test-*`
/// sessions (bats/synthetic probes) are skipped. Unreadable DBs are skipped
/// rather than counted as quiet.
pub fn liveness(cache_root: &Path, exclude: Option<&str>) -> Liveness {
    let sessions_root = cache_root.join("ctxl");
    let mut dbs: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&sessions_root) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(sid) = name.to_str() else { continue };
            if sid.starts_with("test-") || Some(sid) == exclude {
                continue;
            }
            let db = entry.path().join("store.db");
            let Ok(meta) = std::fs::metadata(&db) else { continue };
            let Ok(mtime) = meta.modified() else { continue };
            dbs.push((mtime, db));
        }
    }
    dbs.sort_by_key(|e| std::cmp::Reverse(e.0));

    let mut recent_quiet = 0usize;
    let mut scanned = 0usize;
    let mut last_mtime: Option<std::time::SystemTime> = None;

    for (mtime, db) in dbs.into_iter().take(LIVENESS_SCAN_CAP) {
        let Ok(conn) = Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        else {
            continue;
        };
        let _ = conn.pragma_update(None, "busy_timeout", 2000);
        // GLOB, not LIKE — `_` is a single-char wildcard under LIKE.
        let Ok(intercepts) = conn.query_row(
            "SELECT count(*) FROM handles WHERE id GLOB 'b_*' OR id GLOB 'g_*'",
            [],
            |row| row.get::<_, i64>(0),
        ) else {
            continue;
        };
        scanned += 1;
        if intercepts > 0 {
            // Newest intercepting session found — both metrics are settled.
            last_mtime = Some(mtime);
            break;
        }
        recent_quiet += 1;
    }

    let last_interception_days_ago = last_mtime.and_then(|t| {
        std::time::SystemTime::now().duration_since(t).ok().map(|d| d.as_secs() / 86_400)
    });

    let warning = (recent_quiet >= LIVENESS_QUIET_WARN).then(|| {
        let age = match last_interception_days_ago {
            Some(d) => format!("last handle written ~{d}d ago"),
            None => format!("none found in {scanned} recent sessions"),
        };
        format!(
            "no interception in the last {recent_quiet} sessions ({age}) — \
             hook chain may be broken; run `ctxl doctor`"
        )
    });

    Liveness {
        last_interception_days_ago,
        recent_quiet_sessions: recent_quiet,
        scanned_sessions: scanned,
        warning,
    }
}

#[derive(Debug, serde::Serialize)]
pub struct Thresholds {
    pub bash_bytes: usize,
    pub bash_lines: usize,
    pub grep_lines: usize,
}

pub fn collect(session_id: Option<&str>, cache_root: &Path) -> DoctorReport {
    let binary_path = std::env::current_exe().ok();
    let hook_enabled = std::env::var("CTXL_ENABLED").ok().as_deref() != Some("0");
    let norecord = std::env::var("CTXL_RECORD").ok().as_deref() == Some("0");

    let disabled_file = find_disabled_file();

    let thresholds = Thresholds {
        bash_bytes: crate::env_parse("CTXL_BASH_THRESHOLD", 8192),
        bash_lines: crate::env_parse("CTXL_BASH_LINE_THRESHOLD", 200),
        grep_lines: crate::env_parse("CTXL_GREP_THRESHOLD", 200),
    };

    let jq_available = std::process::Command::new("which")
        .arg("jq")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());

    let (
        db_path,
        db_exists,
        schema_version,
        handle_count,
        calls_count,
        intercepted_count,
        retrieved_count,
        retrieval_total,
    ) = match session_id {
        Some(sid) => {
            let dp = crate::db::session_dir_at(cache_root, sid).join("store.db");
            let exists = dp.exists();
            if exists {
                match Connection::open(&dp) {
                    Ok(conn) => {
                        let _ = conn.pragma_update(None, "busy_timeout", 5000);
                        let sv: Option<i32> =
                            conn.pragma_query_value(None, "user_version", |row| row.get(0)).ok();
                        let hc = conn
                            .query_row("SELECT count(*) FROM handles", [], |row| {
                                row.get::<_, i64>(0)
                            })
                            .ok();
                        let cc = conn
                            .query_row("SELECT count(*) FROM calls", [], |row| row.get::<_, i64>(0))
                            .ok();
                        let ic = conn
                            .query_row(
                                "SELECT count(*) FROM calls WHERE intercepted = 1",
                                [],
                                |row| row.get::<_, i64>(0),
                            )
                            .ok();
                        let rc = conn
                            .query_row(
                                "SELECT count(*) FROM handles WHERE retrieval_count > 0",
                                [],
                                |row| row.get::<_, i64>(0),
                            )
                            .ok();
                        let rt = conn
                            .query_row(
                                "SELECT COALESCE(SUM(retrieval_count), 0) FROM handles",
                                [],
                                |row| row.get::<_, i64>(0),
                            )
                            .ok();
                        (Some(dp), true, sv, hc, cc, ic, rc, rt)
                    }
                    Err(e) => {
                        eprintln!("[ctxl] warn: could not open db: {e}");
                        (Some(dp), true, None, None, None, None, None, None)
                    }
                }
            } else {
                (Some(dp), false, None, None, None, None, None, None)
            }
        }
        None => (None, false, None, None, None, None, None, None),
    };

    DoctorReport {
        binary_path,
        version: env!("CARGO_PKG_VERSION"),
        session_id: session_id.map(String::from),
        db_path,
        db_exists,
        schema_version,
        handle_count,
        calls_count,
        intercepted_count,
        retrieved_count,
        retrieval_total,
        hook_enabled,
        disabled_file,
        norecord,
        thresholds,
        jq_available,
        cache_root: cache_root.to_path_buf(),
        liveness: liveness(cache_root, session_id),
    }
}

fn find_disabled_file() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let candidate = cwd.join(".claude").join("ctxl.disabled");
    candidate.exists().then_some(candidate)
}

pub fn format_human(report: &DoctorReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("ctxl v{}\n", report.version));
    out.push_str(&format!(
        "binary:          {}\n",
        report.binary_path.as_ref().map_or("—".into(), |p| p.display().to_string())
    ));
    out.push_str(&format!("cache_root:      {}\n", report.cache_root.display()));
    out.push_str(&format!("session_id:      {}\n", report.session_id.as_deref().unwrap_or("—")));
    out.push_str(&format!(
        "db_path:         {}\n",
        report.db_path.as_ref().map_or("—".into(), |p| p.display().to_string())
    ));
    out.push_str(&format!("db_exists:       {}\n", report.db_exists));
    out.push_str(&format!(
        "schema_version:  {}\n",
        report.schema_version.map_or("—".into(), |v| v.to_string())
    ));
    out.push_str(&format!(
        "handles:         {}\n",
        report.handle_count.map_or("—".into(), |v| v.to_string())
    ));
    out.push_str(&format!(
        "calls:           {}\n",
        report.calls_count.map_or("—".into(), |v| v.to_string())
    ));
    out.push_str(&format!(
        "intercepted:     {}\n",
        report.intercepted_count.map_or("—".into(), |v| v.to_string())
    ));
    out.push_str(&format!(
        "retrieved:       {}\n",
        report.retrieved_count.map_or("—".into(), |v| v.to_string())
    ));
    out.push_str(&format!(
        "retrieval_total: {}\n",
        report.retrieval_total.map_or("—".into(), |v| v.to_string())
    ));
    out.push_str(&format!("hook_enabled:    {}\n", if report.hook_enabled { "yes" } else { "no" }));
    if let Some(ref df) = report.disabled_file {
        out.push_str(&format!("disabled_file:   {}\n", df.display()));
    }
    out.push_str(&format!("norecord:        {}\n", if report.norecord { "yes" } else { "no" }));
    out.push_str(&format!("jq_available:    {}\n", if report.jq_available { "yes" } else { "no" }));
    out.push_str("\nThresholds:\n");
    out.push_str(&format!("  bash_bytes:    {}\n", report.thresholds.bash_bytes));
    out.push_str(&format!("  bash_lines:    {}\n", report.thresholds.bash_lines));
    out.push_str(&format!("  grep_lines:    {}\n", report.thresholds.grep_lines));
    out.push_str("\nLiveness:\n");
    out.push_str(&format!(
        "  last_interception:     {}\n",
        report.liveness.last_interception_days_ago.map_or_else(
            || format!("none in {} recent sessions", report.liveness.scanned_sessions),
            |d| format!("~{d}d ago"),
        )
    ));
    out.push_str(&format!("  recent_quiet_sessions: {}\n", report.liveness.recent_quiet_sessions));
    if let Some(ref w) = report.liveness.warning {
        out.push_str(&format!("  WARNING: {w}\n"));
    }
    out
}

pub fn format_json(report: &DoctorReport) -> String {
    serde_json::to_string_pretty(report).unwrap_or_else(|_| "{}".into())
}
