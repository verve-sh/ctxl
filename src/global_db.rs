/// Global cross-session cache database.
///
/// Stores deduplicated tool output blobs and a cache index keyed by
/// (repo_root, tool, output_mode, param_hash, git_head).  The global DB
/// lives at `~/.cache/ctxl/global.db` (or `$CTXL_CACHE_ROOT/ctxl/global.db`).
///
/// All operations are fail-open: every public function returns `Result` but
/// callers are expected to log errors and fall through to session-only storage.
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

const GLOBAL_SCHEMA_VERSION: i32 = 2;

const GLOBAL_SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS blobs (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    content_hash      TEXT    UNIQUE NOT NULL,
    content           TEXT    NOT NULL,
    compressed_body   TEXT,
    compressed_method TEXT,
    line_count        INTEGER,
    token_est         INTEGER,
    created_at        TEXT DEFAULT (datetime('now')),
    last_used         TEXT DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS cache_index (
    repo_root   TEXT    NOT NULL,
    tool        TEXT    NOT NULL,
    output_mode TEXT    NOT NULL DEFAULT '',
    param_hash  TEXT    NOT NULL,
    git_head    TEXT    NOT NULL,
    blob_id     INTEGER NOT NULL,
    created_at  TEXT DEFAULT (datetime('now')),
    PRIMARY KEY (repo_root, tool, output_mode, param_hash, git_head)
);

CREATE INDEX IF NOT EXISTS idx_cache_last_used ON blobs(last_used);

CREATE VIRTUAL TABLE IF NOT EXISTS blobs_fts USING fts5(
    content,
    content='blobs',
    content_rowid='rowid',
    tokenize='unicode61 tokenchars ''_./:-'''
);

CREATE TRIGGER IF NOT EXISTS blobs_ai AFTER INSERT ON blobs BEGIN
    INSERT INTO blobs_fts(rowid, content) VALUES (new.rowid, new.content);
END;

CREATE TRIGGER IF NOT EXISTS blobs_ad AFTER DELETE ON blobs BEGIN
    INSERT INTO blobs_fts(blobs_fts, rowid, content)
        VALUES ('delete', old.rowid, old.content);
END;
"#;

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

/// Return the path to the global DB file.
///
/// Respects `CTXL_CACHE_ROOT` (set by session-start hook for project-local
/// cache) and falls back to `dirs::cache_dir()`.
pub fn global_db_path() -> Option<PathBuf> {
    let root: PathBuf = if let Ok(r) = std::env::var("CTXL_CACHE_ROOT") {
        PathBuf::from(r)
    } else {
        dirs::cache_dir()?
    };
    Some(root.join("ctxl").join("global.db"))
}

// ---------------------------------------------------------------------------
// Initialization
// ---------------------------------------------------------------------------

/// Open (or create) the global DB at `path`, apply WAL mode and schema.
///
/// This function is idempotent — safe to call on an already-current database.
pub fn open_global_db(path: &Path) -> Result<Connection, GlobalDbError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL;")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    apply_global_schema(&conn)?;
    Ok(conn)
}

/// Apply the global DB schema.
///
/// Uses the DB's own `user_version` pragma (separate from the session DB's
/// pragma) to track its schema state.
pub fn apply_global_schema(conn: &Connection) -> Result<(), GlobalDbError> {
    let current: i32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if current == 0 {
        conn.execute_batch(GLOBAL_SCHEMA_SQL)?;
        conn.pragma_update(None, "user_version", 1)?;
    }
    let current: i32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if current < 2 {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS session_summaries (
                session_id         TEXT    PRIMARY KEY,
                handles_count      INTEGER NOT NULL DEFAULT 0,
                tokens_intercepted INTEGER NOT NULL DEFAULT 0,
                bytes_intercepted  INTEGER NOT NULL DEFAULT 0,
                calls_count        INTEGER NOT NULL DEFAULT 0,
                calls_intercepted  INTEGER NOT NULL DEFAULT 0,
                retrieval_calls    INTEGER NOT NULL DEFAULT 0,
                retrieval_tokens   INTEGER NOT NULL DEFAULT 0,
                handles_retrieved  INTEGER NOT NULL DEFAULT 0,
                total_retrievals   INTEGER NOT NULL DEFAULT 0,
                cleaned_at         TEXT    DEFAULT (datetime('now'))
            );",
        )?;
        conn.pragma_update(None, "user_version", GLOBAL_SCHEMA_VERSION)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Cache lookup
// ---------------------------------------------------------------------------

/// Result of a cache lookup.
pub struct CacheLookup {
    /// The blob ID in the global `blobs` table.
    pub blob_id: i64,
    /// The `created_at` timestamp from the blobs row (for diagnostic output).
    pub created_at: String,
}

/// Look up a cache entry in the global DB.
///
/// Returns `Ok(Some(...))` on hit, `Ok(None)` on miss, `Err(...)` on failure.
pub fn lookup(
    conn: &Connection,
    repo_root: &str,
    tool: &str,
    output_mode: &str,
    param_hash: &str,
    git_head: &str,
) -> Result<Option<CacheLookup>, GlobalDbError> {
    let result: Option<(i64, String)> = conn
        .query_row(
            "SELECT ci.blob_id, b.created_at \
             FROM cache_index ci \
             JOIN blobs b ON b.id = ci.blob_id \
             WHERE ci.repo_root=?1 AND ci.tool=?2 AND ci.output_mode=?3 \
               AND ci.param_hash=?4 AND ci.git_head=?5",
            rusqlite::params![repo_root, tool, output_mode, param_hash, git_head],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;

    Ok(result.map(|(blob_id, created_at)| CacheLookup { blob_id, created_at }))
}

/// Look up a cache entry ignoring `output_mode`.
///
/// Returns the first match (ordered by `created_at` DESC so the most recent
/// mode wins when the same params produced multiple modes).
pub fn lookup_any_mode(
    conn: &Connection,
    repo_root: &str,
    tool: &str,
    param_hash: &str,
    git_head: &str,
) -> Result<Option<CacheLookup>, GlobalDbError> {
    let result: Option<(i64, String)> = conn
        .query_row(
            "SELECT ci.blob_id, b.created_at \
             FROM cache_index ci \
             JOIN blobs b ON b.id = ci.blob_id \
             WHERE ci.repo_root=?1 AND ci.tool=?2 \
               AND ci.param_hash=?3 AND ci.git_head=?4 \
             ORDER BY b.created_at DESC LIMIT 1",
            rusqlite::params![repo_root, tool, param_hash, git_head],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;

    Ok(result.map(|(blob_id, created_at)| CacheLookup { blob_id, created_at }))
}

/// Update `last_used` on a blob after a cache hit.
pub fn touch_blob(conn: &Connection, blob_id: i64) -> Result<(), GlobalDbError> {
    conn.execute(
        "UPDATE blobs SET last_used = datetime('now') WHERE id = ?1",
        rusqlite::params![blob_id],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Cache store
// ---------------------------------------------------------------------------

/// Parameters for storing a new blob in the global cache.
pub struct StoreBlobParams<'a> {
    pub content_hash: &'a str,
    pub content: &'a str,
    pub compressed_body: Option<&'a str>,
    pub compressed_method: Option<&'a str>,
    pub line_count: Option<i64>,
    pub token_est: Option<i64>,
}

/// Store a blob in the global DB and associate it with the cache index.
///
/// Returns the blob ID (newly inserted or pre-existing for the same hash).
pub fn store_blob(
    conn: &Connection,
    params: StoreBlobParams<'_>,
    repo_root: &str,
    tool: &str,
    output_mode: &str,
    param_hash: &str,
    git_head: &str,
) -> Result<i64, GlobalDbError> {
    // Upsert the blob (content_hash is UNIQUE — ignore on conflict).
    conn.execute(
        "INSERT OR IGNORE INTO blobs \
         (content_hash, content, compressed_body, compressed_method, line_count, token_est) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            params.content_hash,
            params.content,
            params.compressed_body,
            params.compressed_method,
            params.line_count,
            params.token_est,
        ],
    )?;

    // Retrieve the blob_id (either just inserted or pre-existing).
    let blob_id: i64 = conn.query_row(
        "SELECT id FROM blobs WHERE content_hash = ?1",
        rusqlite::params![params.content_hash],
        |row| row.get(0),
    )?;

    // Upsert the cache_index entry.
    conn.execute(
        "INSERT OR REPLACE INTO cache_index \
         (repo_root, tool, output_mode, param_hash, git_head, blob_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![repo_root, tool, output_mode, param_hash, git_head, blob_id],
    )?;

    Ok(blob_id)
}

/// Fetch blob content from the global DB by blob ID.
pub fn fetch_blob_content(
    conn: &Connection,
    blob_id: i64,
) -> Result<Option<String>, GlobalDbError> {
    let result: Option<String> = conn
        .query_row("SELECT content FROM blobs WHERE id = ?1", rusqlite::params![blob_id], |row| {
            row.get(0)
        })
        .optional()?;
    Ok(result)
}

// ---------------------------------------------------------------------------
// FTS search
// ---------------------------------------------------------------------------

/// A single search result from the global FTS index.
pub struct GlobalSearchResult {
    pub blob_id: i64,
    pub snippet: String,
    pub repo_root: String,
    pub tool: String,
    pub created_at: String,
}

/// Search the global `blobs_fts` index.
///
/// Scoped to `repo_root` when provided. Returns at most `limit` results.
pub fn search_global(
    conn: &Connection,
    query: &str,
    repo_root: Option<&str>,
    limit: usize,
) -> Result<Vec<GlobalSearchResult>, GlobalDbError> {
    if let Some(msg) = crate::detect_regex_metacharacters(query) {
        return Err(GlobalDbError::RegexInQuery(format!("error: {msg}")));
    }

    let sanitized = sanitize_fts5_query(query);

    // Deduplicate results when a single blob has multiple cache_index entries
    // (e.g., different git_head values).  Without dedup, duplicate blob content
    // displaces relevant results when limit is small.  We use a subquery to
    // pick one representative cache_index row per blob_id (MIN(rowid) is
    // deterministic and cheap), then join FTS results against it.
    // highlight() requires FTS row context, so GROUP BY on the outer query
    // would break it — the subquery approach preserves FTS context.
    if let Some(root) = repo_root {
        let mut stmt = conn.prepare(
            "SELECT b.id, \
             highlight(blobs_fts, 0, '[', ']'), \
             ci.repo_root, ci.tool, b.created_at \
             FROM blobs_fts \
             JOIN blobs b ON b.rowid = blobs_fts.rowid \
             JOIN ( \
               SELECT blob_id, repo_root, tool, MIN(rowid) AS rn \
               FROM cache_index \
               GROUP BY blob_id \
             ) ci ON ci.blob_id = b.id \
             WHERE blobs_fts MATCH ?1 \
               AND ci.repo_root = ?2 \
             ORDER BY rank \
             LIMIT ?3",
        )?;
        let results = stmt
            .query_map(rusqlite::params![sanitized, root, limit as i64], |row| {
                Ok(GlobalSearchResult {
                    blob_id: row.get(0)?,
                    snippet: row.get(1)?,
                    repo_root: row.get(2)?,
                    tool: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(results)
    } else {
        let mut stmt = conn.prepare(
            "SELECT b.id, \
             highlight(blobs_fts, 0, '[', ']'), \
             ci.repo_root, ci.tool, b.created_at \
             FROM blobs_fts \
             JOIN blobs b ON b.rowid = blobs_fts.rowid \
             JOIN ( \
               SELECT blob_id, repo_root, tool, MIN(rowid) AS rn \
               FROM cache_index \
               GROUP BY blob_id \
             ) ci ON ci.blob_id = b.id \
             WHERE blobs_fts MATCH ?1 \
             ORDER BY rank \
             LIMIT ?2",
        )?;
        let results = stmt
            .query_map(rusqlite::params![sanitized, limit as i64], |row| {
                Ok(GlobalSearchResult {
                    blob_id: row.get(0)?,
                    snippet: row.get(1)?,
                    repo_root: row.get(2)?,
                    tool: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(results)
    }
}

// ---------------------------------------------------------------------------
// Session stats aggregation
// ---------------------------------------------------------------------------

/// Stats for a single cleaned session.
#[derive(Debug, Clone, Serialize)]
pub struct SessionSummary {
    pub session_id: String,
    pub handles_count: i64,
    pub tokens_intercepted: i64,
    pub bytes_intercepted: i64,
    pub calls_count: i64,
    pub calls_intercepted: i64,
    pub retrieval_calls: i64,
    pub retrieval_tokens: i64,
    pub handles_retrieved: i64,
    pub total_retrievals: i64,
    pub cleaned_at: Option<String>,
}

/// Aggregated totals across all cleaned sessions.
#[derive(Debug, Clone, Serialize)]
pub struct CumulativeStats {
    pub sessions_count: i64,
    pub handles_count: i64,
    pub tokens_intercepted: i64,
    pub bytes_intercepted: i64,
    pub calls_count: i64,
    pub calls_intercepted: i64,
    pub retrieval_calls: i64,
    pub retrieval_tokens: i64,
    pub handles_retrieved: i64,
    pub total_retrievals: i64,
    pub earliest_cleaned: Option<String>,
    pub latest_cleaned: Option<String>,
}

/// Extract stats from a session's `store.db` (read-only).
///
/// Handles missing `calls` table (pre-v3) and missing `retrieval_count`
/// column (pre-v6) by defaulting to 0.
pub fn compute_session_stats(session_db: &Path) -> Result<SessionSummary, GlobalDbError> {
    let conn = Connection::open_with_flags(session_db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;

    let handles_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM handles", [], |r| r.get(0)).unwrap_or(0);

    let tokens_intercepted: i64 = conn
        .query_row("SELECT COALESCE(SUM(token_est), 0) FROM handles", [], |r| r.get(0))
        .unwrap_or(0);

    let bytes_intercepted: i64 = conn
        .query_row("SELECT COALESCE(SUM(LENGTH(content)), 0) FROM handles", [], |r| r.get(0))
        .unwrap_or(0);

    // calls table may not exist (pre-v3 sessions)
    let has_calls = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='calls'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
        > 0;

    let (calls_count, calls_intercepted, retrieval_calls, retrieval_tokens) = if has_calls {
        let total: i64 =
            conn.query_row("SELECT COUNT(*) FROM calls", [], |r| r.get(0)).unwrap_or(0);
        let intercepted: i64 = conn
            .query_row("SELECT COUNT(*) FROM calls WHERE intercepted = 1", [], |r| r.get(0))
            .unwrap_or(0);
        let ret_calls: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM calls WHERE intercepted = 0 AND tool LIKE 'ctxl-%'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let ret_tokens: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(token_est), 0) FROM calls WHERE intercepted = 0 AND tool LIKE 'ctxl-%'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        (total, intercepted, ret_calls, ret_tokens)
    } else {
        (0, 0, 0, 0)
    };

    // retrieval_count column may not exist (pre-v6)
    let has_retrieval_count = conn.prepare("SELECT retrieval_count FROM handles LIMIT 0").is_ok();

    let (handles_retrieved, total_retrievals) = if has_retrieval_count {
        let retrieved: i64 = conn
            .query_row("SELECT COUNT(*) FROM handles WHERE retrieval_count > 0", [], |r| r.get(0))
            .unwrap_or(0);
        let total: i64 = conn
            .query_row("SELECT COALESCE(SUM(retrieval_count), 0) FROM handles", [], |r| r.get(0))
            .unwrap_or(0);
        (retrieved, total)
    } else {
        (0, 0)
    };

    Ok(SessionSummary {
        session_id: String::new(),
        handles_count,
        tokens_intercepted,
        bytes_intercepted,
        calls_count,
        calls_intercepted,
        retrieval_calls,
        retrieval_tokens,
        handles_retrieved,
        total_retrievals,
        cleaned_at: None,
    })
}

/// Persist a session summary into the global DB (idempotent via `INSERT OR REPLACE`).
pub fn save_session_summary(
    conn: &Connection,
    summary: &SessionSummary,
) -> Result<(), GlobalDbError> {
    conn.execute(
        "INSERT OR REPLACE INTO session_summaries \
         (session_id, handles_count, tokens_intercepted, bytes_intercepted, \
          calls_count, calls_intercepted, retrieval_calls, retrieval_tokens, \
          handles_retrieved, total_retrievals) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        rusqlite::params![
            summary.session_id,
            summary.handles_count,
            summary.tokens_intercepted,
            summary.bytes_intercepted,
            summary.calls_count,
            summary.calls_intercepted,
            summary.retrieval_calls,
            summary.retrieval_tokens,
            summary.handles_retrieved,
            summary.total_retrievals,
        ],
    )?;
    Ok(())
}

/// Query aggregated totals across all cleaned sessions.
pub fn query_cumulative_stats(conn: &Connection) -> Result<CumulativeStats, GlobalDbError> {
    let row = conn.query_row(
        "SELECT \
            COUNT(*), \
            COALESCE(SUM(handles_count), 0), \
            COALESCE(SUM(tokens_intercepted), 0), \
            COALESCE(SUM(bytes_intercepted), 0), \
            COALESCE(SUM(calls_count), 0), \
            COALESCE(SUM(calls_intercepted), 0), \
            COALESCE(SUM(retrieval_calls), 0), \
            COALESCE(SUM(retrieval_tokens), 0), \
            COALESCE(SUM(handles_retrieved), 0), \
            COALESCE(SUM(total_retrievals), 0), \
            MIN(cleaned_at), \
            MAX(cleaned_at) \
         FROM session_summaries",
        [],
        |r| {
            Ok(CumulativeStats {
                sessions_count: r.get(0)?,
                handles_count: r.get(1)?,
                tokens_intercepted: r.get(2)?,
                bytes_intercepted: r.get(3)?,
                calls_count: r.get(4)?,
                calls_intercepted: r.get(5)?,
                retrieval_calls: r.get(6)?,
                retrieval_tokens: r.get(7)?,
                handles_retrieved: r.get(8)?,
                total_retrievals: r.get(9)?,
                earliest_cleaned: r.get(10)?,
                latest_cleaned: r.get(11)?,
            })
        },
    )?;
    Ok(row)
}

/// Query individual session summaries, most recent first.
pub fn query_session_summaries(
    conn: &Connection,
    limit: usize,
) -> Result<Vec<SessionSummary>, GlobalDbError> {
    let mut stmt = conn.prepare(
        "SELECT session_id, handles_count, tokens_intercepted, bytes_intercepted, \
                calls_count, calls_intercepted, retrieval_calls, retrieval_tokens, \
                handles_retrieved, total_retrievals, cleaned_at \
         FROM session_summaries ORDER BY cleaned_at DESC LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![limit as i64], |r| {
            Ok(SessionSummary {
                session_id: r.get(0)?,
                handles_count: r.get(1)?,
                tokens_intercepted: r.get(2)?,
                bytes_intercepted: r.get(3)?,
                calls_count: r.get(4)?,
                calls_intercepted: r.get(5)?,
                retrieval_calls: r.get(6)?,
                retrieval_tokens: r.get(7)?,
                handles_retrieved: r.get(8)?,
                total_retrievals: r.get(9)?,
                cleaned_at: r.get(10)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ---------------------------------------------------------------------------
// Clean
// ---------------------------------------------------------------------------

/// Remove blobs whose `last_used` is older than `days_threshold` days.
///
/// Also removes orphaned `cache_index` rows that point to deleted blobs.
/// Returns `(blobs_deleted, bytes_freed)`.
pub fn clean_old_blobs(
    conn: &Connection,
    days_threshold: u32,
) -> Result<(usize, usize), GlobalDbError> {
    let threshold_param = format!("-{days_threshold} days");

    // Collect IDs and content lengths before deletion.
    let mut stmt = conn.prepare(
        "SELECT id, LENGTH(content) FROM blobs \
         WHERE last_used < datetime('now', ?1)",
    )?;
    let to_delete: Vec<(i64, usize)> = stmt
        .query_map(rusqlite::params![threshold_param], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;

    let count = to_delete.len();
    let bytes: usize = to_delete.iter().map(|(_, b)| b).sum();

    // Wrap deletions in a transaction so cache_index and blob rows are removed
    // atomically.  If interrupted between the two DELETEs without a transaction,
    // cache_index would be gone but the blob would remain as an unreachable orphan.
    conn.execute_batch("BEGIN")?;
    let result = (|| -> Result<(), GlobalDbError> {
        for (id, _) in &to_delete {
            conn.execute("DELETE FROM cache_index WHERE blob_id = ?1", rusqlite::params![id])?;
            conn.execute("DELETE FROM blobs WHERE id = ?1", rusqlite::params![id])?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => conn.execute_batch("COMMIT")?,
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(e);
        }
    }

    Ok((count, bytes))
}

use crate::sanitize_fts5_query;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors from global DB operations.
#[derive(Debug)]
pub enum GlobalDbError {
    Sqlite(rusqlite::Error),
    Io(std::io::Error),
    RegexInQuery(String),
}

impl std::fmt::Display for GlobalDbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GlobalDbError::Sqlite(e) => write!(f, "global db sqlite: {e}"),
            GlobalDbError::Io(e) => write!(f, "global db io: {e}"),
            GlobalDbError::RegexInQuery(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for GlobalDbError {}

impl From<rusqlite::Error> for GlobalDbError {
    fn from(e: rusqlite::Error) -> Self {
        GlobalDbError::Sqlite(e)
    }
}

impl From<std::io::Error> for GlobalDbError {
    fn from(e: std::io::Error) -> Self {
        GlobalDbError::Io(e)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn fresh_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
        apply_global_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn global_schema_creates_tables() {
        let conn = fresh_db();
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(tables.contains(&"blobs".to_string()), "blobs table missing");
        assert!(tables.contains(&"cache_index".to_string()), "cache_index table missing");
        // FTS tables appear as regular tables in sqlite_master
        assert!(tables.iter().any(|t| t.contains("fts")), "blobs_fts table missing");
    }

    #[test]
    fn global_schema_idempotent() {
        let conn = fresh_db();
        // Calling again must not fail
        apply_global_schema(&conn).unwrap();
        let ver: i32 = conn.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
        assert_eq!(ver, GLOBAL_SCHEMA_VERSION);
    }

    #[test]
    fn v2_migration_creates_session_summaries() {
        let conn = fresh_db();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='session_summaries'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "session_summaries table must exist after v2 migration");
    }

    #[test]
    fn v1_to_v2_migration() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
        // Simulate a v1 DB
        conn.execute_batch(GLOBAL_SCHEMA_SQL).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        let ver: i32 = conn.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
        assert_eq!(ver, 1);

        apply_global_schema(&conn).unwrap();
        let ver: i32 = conn.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
        assert_eq!(ver, 2);

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='session_summaries'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn save_and_query_session_summary() {
        let conn = fresh_db();
        let summary = SessionSummary {
            session_id: "test-session-1".to_string(),
            handles_count: 10,
            tokens_intercepted: 5000,
            bytes_intercepted: 20000,
            calls_count: 50,
            calls_intercepted: 10,
            retrieval_calls: 5,
            retrieval_tokens: 200,
            handles_retrieved: 3,
            total_retrievals: 8,
            cleaned_at: None,
        };
        save_session_summary(&conn, &summary).unwrap();

        let rows = query_session_summaries(&conn, 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].session_id, "test-session-1");
        assert_eq!(rows[0].handles_count, 10);
        assert_eq!(rows[0].tokens_intercepted, 5000);
        assert_eq!(rows[0].bytes_intercepted, 20000);
        assert_eq!(rows[0].calls_count, 50);
        assert_eq!(rows[0].calls_intercepted, 10);
        assert_eq!(rows[0].retrieval_calls, 5);
        assert_eq!(rows[0].retrieval_tokens, 200);
        assert_eq!(rows[0].handles_retrieved, 3);
        assert_eq!(rows[0].total_retrievals, 8);
    }

    #[test]
    fn save_session_summary_idempotent() {
        let conn = fresh_db();
        let summary = SessionSummary {
            session_id: "idempotent-test".to_string(),
            handles_count: 5,
            tokens_intercepted: 1000,
            bytes_intercepted: 4000,
            calls_count: 20,
            calls_intercepted: 5,
            retrieval_calls: 3,
            retrieval_tokens: 100,
            handles_retrieved: 2,
            total_retrievals: 4,
            cleaned_at: None,
        };
        save_session_summary(&conn, &summary).unwrap();
        save_session_summary(&conn, &summary).unwrap();

        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM session_summaries", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 1, "duplicate insert should produce single row");
    }

    #[test]
    fn query_cumulative_stats_sums_correctly() {
        let conn = fresh_db();
        for i in 1..=3 {
            let summary = SessionSummary {
                session_id: format!("session-{i}"),
                handles_count: 10,
                tokens_intercepted: 1000,
                bytes_intercepted: 5000,
                calls_count: 20,
                calls_intercepted: 5,
                retrieval_calls: 3,
                retrieval_tokens: 100,
                handles_retrieved: 2,
                total_retrievals: 4,
                cleaned_at: None,
            };
            save_session_summary(&conn, &summary).unwrap();
        }

        let stats = query_cumulative_stats(&conn).unwrap();
        assert_eq!(stats.sessions_count, 3);
        assert_eq!(stats.handles_count, 30);
        assert_eq!(stats.tokens_intercepted, 3000);
        assert_eq!(stats.bytes_intercepted, 15000);
        assert_eq!(stats.calls_count, 60);
        assert_eq!(stats.calls_intercepted, 15);
        assert_eq!(stats.retrieval_calls, 9);
        assert_eq!(stats.retrieval_tokens, 300);
        assert_eq!(stats.handles_retrieved, 6);
        assert_eq!(stats.total_retrievals, 12);
    }

    #[test]
    fn query_cumulative_stats_empty() {
        let conn = fresh_db();
        let stats = query_cumulative_stats(&conn).unwrap();
        assert_eq!(stats.sessions_count, 0);
        assert_eq!(stats.handles_count, 0);
        assert_eq!(stats.tokens_intercepted, 0);
        assert_eq!(stats.bytes_intercepted, 0);
        assert_eq!(stats.calls_count, 0);
        assert_eq!(stats.calls_intercepted, 0);
        assert!(stats.earliest_cleaned.is_none());
        assert!(stats.latest_cleaned.is_none());
    }

    #[test]
    fn query_session_summaries_ordered() {
        let conn = fresh_db();
        // Insert with explicit cleaned_at to control ordering
        conn.execute(
            "INSERT INTO session_summaries (session_id, cleaned_at) VALUES ('older', '2024-01-01 00:00:00')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO session_summaries (session_id, cleaned_at) VALUES ('newer', '2024-06-01 00:00:00')",
            [],
        ).unwrap();

        let rows = query_session_summaries(&conn, 10).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].session_id, "newer", "most recent should be first");
        assert_eq!(rows[1].session_id, "older");
    }

    #[test]
    fn store_and_lookup_hit() {
        let conn = fresh_db();
        let params = StoreBlobParams {
            content_hash: "abc123",
            content: "hello world",
            compressed_body: None,
            compressed_method: None,
            line_count: Some(1),
            token_est: Some(2),
        };
        let blob_id =
            store_blob(&conn, params, "repo_root", "Bash", "stdout", "param_hash_x", "deadbeef")
                .unwrap();

        let hit = lookup(&conn, "repo_root", "Bash", "stdout", "param_hash_x", "deadbeef")
            .unwrap()
            .expect("cache hit expected");
        assert_eq!(hit.blob_id, blob_id);
    }

    #[test]
    fn lookup_miss_on_different_git_head() {
        let conn = fresh_db();
        let params = StoreBlobParams {
            content_hash: "abc456",
            content: "content",
            compressed_body: None,
            compressed_method: None,
            line_count: Some(1),
            token_est: Some(1),
        };
        store_blob(&conn, params, "repo_root", "Bash", "stdout", "ph", "head1").unwrap();

        let miss = lookup(&conn, "repo_root", "Bash", "stdout", "ph", "head2").unwrap();
        assert!(miss.is_none(), "different git_head should miss");
    }

    #[test]
    fn fetch_blob_content_roundtrip() {
        let conn = fresh_db();
        let params = StoreBlobParams {
            content_hash: "hash789",
            content: "the content",
            compressed_body: None,
            compressed_method: None,
            line_count: Some(1),
            token_est: Some(3),
        };
        let blob_id = store_blob(&conn, params, "r", "Grep", "", "ph", "hd").unwrap();
        let content = fetch_blob_content(&conn, blob_id).unwrap().expect("content present");
        assert_eq!(content, "the content");
    }

    #[test]
    fn fts_search_global_finds_match() {
        let conn = fresh_db();
        let params = StoreBlobParams {
            content_hash: "searchhash",
            content: "fn hello_world() { println!(\"hello\"); }",
            compressed_body: None,
            compressed_method: None,
            line_count: Some(1),
            token_est: Some(10),
        };
        store_blob(&conn, params, "/my/repo", "Bash", "stdout", "ph", "hd").unwrap();

        let results = search_global(&conn, "hello_world", Some("/my/repo"), 10).unwrap();
        assert!(!results.is_empty(), "FTS search should find match");
        assert_eq!(results[0].tool, "Bash");
    }

    #[test]
    fn clean_old_blobs_removes_stale() {
        let conn = fresh_db();
        // Insert a blob with artificially old last_used
        conn.execute(
            "INSERT INTO blobs (content_hash, content, last_used) \
             VALUES ('old_hash', 'old content', datetime('now', '-31 days'))",
            [],
        )
        .unwrap();
        let old_id: i64 = conn
            .query_row("SELECT id FROM blobs WHERE content_hash='old_hash'", [], |r| r.get(0))
            .unwrap();
        conn.execute(
            "INSERT INTO cache_index \
             (repo_root, tool, output_mode, param_hash, git_head, blob_id) \
             VALUES ('r', 'Bash', '', 'ph', 'hd', ?1)",
            rusqlite::params![old_id],
        )
        .unwrap();

        let (count, _bytes) = clean_old_blobs(&conn, 30).unwrap();
        assert_eq!(count, 1, "one stale blob should be removed");

        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM blobs WHERE content_hash='old_hash'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining, 0);
    }

    #[test]
    fn clean_preserves_recent_blobs() {
        let conn = fresh_db();
        conn.execute(
            "INSERT INTO blobs (content_hash, content) VALUES ('new_hash', 'new content')",
            [],
        )
        .unwrap();

        let (count, _) = clean_old_blobs(&conn, 30).unwrap();
        assert_eq!(count, 0, "recent blob should not be removed");
    }

    #[test]
    fn compute_session_stats_counts_only_ctxl_retrievals() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("session").join("store.db");
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        let conn = Connection::open(&db_path).unwrap();
        crate::db::apply_schema(&conn).unwrap();

        // Insert recording-tier call (Read, intercepted=0) — should NOT count as retrieval
        conn.execute(
            "INSERT INTO calls (tool, intercepted, line_count, token_est, created_at) \
             VALUES ('Read', 0, 10, 30, 1000)",
            [],
        )
        .unwrap();
        // Insert ctxl retrieval call (ctxl-show, intercepted=0) — should count
        conn.execute(
            "INSERT INTO calls (tool, intercepted, handle_id, line_count, token_est, created_at) \
             VALUES ('ctxl-show', 0, 'b_aabbcc', 50, 150, 1001)",
            [],
        )
        .unwrap();
        // Insert interception call (Bash, intercepted=1) — should NOT count as retrieval
        conn.execute(
            "INSERT INTO calls (tool, intercepted, handle_id, line_count, token_est, created_at) \
             VALUES ('Bash', 1, 'b_112233', 200, 600, 1002)",
            [],
        )
        .unwrap();

        let stats = compute_session_stats(&db_path).unwrap();
        assert_eq!(stats.retrieval_calls, 1, "only ctxl-show should count");
        assert_eq!(stats.retrieval_tokens, 150, "only ctxl-show tokens should count");
        assert_eq!(stats.calls_count, 3, "total calls should be 3");
        assert_eq!(stats.calls_intercepted, 1, "intercepted calls should be 1");
    }

    #[test]
    fn search_global_rejects_regex() {
        let conn = fresh_db();
        let result = search_global(&conn, "[a-z]+", None, 10);
        assert!(result.is_err());
    }
}
