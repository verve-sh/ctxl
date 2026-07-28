use rusqlite::Connection;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum DbError {
    Sqlite(rusqlite::Error),
    Io(std::io::Error),
    NoCacheDir,
    VersionMismatch { found: i32, expected: i32 },
}

impl std::fmt::Display for DbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DbError::Sqlite(e) => write!(f, "sqlite: {e}"),
            DbError::Io(e) => write!(f, "io: {e}"),
            DbError::NoCacheDir => write!(f, "cache directory unavailable"),
            DbError::VersionMismatch { found, expected } => {
                write!(f, "schema version mismatch: found {found}, expected {expected}")
            }
        }
    }
}

impl std::error::Error for DbError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DbError::Sqlite(e) => Some(e),
            DbError::Io(e) => Some(e),
            DbError::NoCacheDir => None,
            DbError::VersionMismatch { .. } => None,
        }
    }
}

impl From<rusqlite::Error> for DbError {
    fn from(e: rusqlite::Error) -> Self {
        DbError::Sqlite(e)
    }
}

impl From<std::io::Error> for DbError {
    fn from(e: std::io::Error) -> Self {
        DbError::Io(e)
    }
}

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

/// Base schema version created from scratch by [`apply_schema`] before
/// incremental migrations run.  A fresh database is first initialised to this
/// version and then immediately upgraded through all incremental migrations
/// up to [`MAX_KNOWN_VERSION`].
///
/// This constant is only meaningful during the creation of a brand-new
/// database.  Callers should never rely on a database stopping at this
/// version; after [`apply_schema`] returns, the version is always
/// `MAX_KNOWN_VERSION`.
const SCHEMA_VERSION: i32 = 2;

/// Highest schema version understood by this binary.  Databases at any
/// version up to and including this value are accepted.  Versions above it
/// are rejected with [`DbError::VersionMismatch`].
///
/// After [`apply_schema`] completes successfully the database is always at
/// exactly this version.
const MAX_KNOWN_VERSION: i32 = 7;
const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS handles (
    id               TEXT    PRIMARY KEY NOT NULL,
    tool             TEXT    NOT NULL,
    output_mode      TEXT    NOT NULL,
    params           TEXT,
    cwd              TEXT,
    summary          TEXT,
    content          TEXT,
    compressed_body  TEXT,
    compressed_method TEXT,
    line_count       INTEGER,
    total_matches    INTEGER,
    truncated        INTEGER NOT NULL DEFAULT 0,
    token_est        INTEGER,
    created_at       INTEGER NOT NULL
);

CREATE VIRTUAL TABLE IF NOT EXISTS content_fts USING fts5(
    content,
    content='handles',
    content_rowid='rowid',
    tokenize='unicode61 tokenchars _'
);

CREATE TRIGGER IF NOT EXISTS handles_ai AFTER INSERT ON handles
    WHEN NEW.content IS NOT NULL
BEGIN
    INSERT INTO content_fts(rowid, content) VALUES (NEW.rowid, NEW.content);
END;

CREATE TRIGGER IF NOT EXISTS handles_ad AFTER DELETE ON handles
    WHEN OLD.content IS NOT NULL
BEGIN
    INSERT INTO content_fts(content_fts, rowid, content) VALUES ('delete', OLD.rowid, OLD.content);
END;

CREATE INDEX IF NOT EXISTS idx_handles_created ON handles(created_at);
"#;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Returns the session directory under an explicit `root`.
///
/// The resulting path is `{root}/ctxl/{session_id}`.
pub fn session_dir_at(root: &Path, session_id: &str) -> PathBuf {
    root.join("ctxl").join(session_id)
}

/// Apply the base schema and all incremental migrations to an open connection.
///
/// This is the single entry point for bringing any database to
/// `MAX_KNOWN_VERSION`.  All incremental migrations (v3 through the current
/// max) are applied in sequence so callers never need to call individual
/// migration functions.
///
/// - `user_version == 0`: fresh database — apply base schema (v1/v2 tables,
///   FTS, triggers, indexes) then run all incremental migrations.
/// - `user_version == 1`: apply v1→v2 column-type migration, then run all
///   incremental migrations.
/// - `user_version >= 2 && < MAX_KNOWN_VERSION`: apply only the pending
///   incremental migrations.
/// - `user_version == MAX_KNOWN_VERSION`: already current — no-op.
/// - `user_version > MAX_KNOWN_VERSION`: database was written by a newer
///   binary — returns `DbError::VersionMismatch`.
///
/// All migrations are idempotent; calling this function on an already-current
/// database is always safe.
///
/// # Errors
///
/// Returns `DbError::VersionMismatch` when the stored version exceeds
/// `MAX_KNOWN_VERSION`, indicating the database was created by a newer build.
/// Returns `DbError::Sqlite` for any underlying SQLite failure.
pub fn apply_schema(conn: &Connection) -> Result<(), DbError> {
    conn.pragma_update(None, "busy_timeout", 5000)?;

    let current: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if current == 0 {
        conn.execute_batch("BEGIN EXCLUSIVE")?;
        let inner_ver: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if inner_ver == 0 {
            match (|| -> Result<(), DbError> {
                conn.execute_batch(SCHEMA_SQL)?;
                conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
                Ok(())
            })() {
                Ok(()) => conn.execute_batch("COMMIT")?,
                Err(e) => {
                    let _ = conn.execute_batch("ROLLBACK");
                    return Err(e);
                }
            }
        } else {
            conn.execute_batch("COMMIT")?;
        }
    } else if current < SCHEMA_VERSION && current == 1 {
        conn.execute_batch("BEGIN EXCLUSIVE")?;
        let inner_ver: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if inner_ver == 1 {
            match (|| -> Result<(), DbError> {
                conn.execute_batch(
                    "UPDATE handles SET created_at = CAST(created_at AS INTEGER) WHERE typeof(created_at) = 'text';",
                )?;
                conn.pragma_update(None, "user_version", 2)?;
                Ok(())
            })() {
                Ok(()) => conn.execute_batch("COMMIT")?,
                Err(e) => {
                    let _ = conn.execute_batch("ROLLBACK");
                    return Err(e);
                }
            }
        } else {
            conn.execute_batch("COMMIT")?;
        }
    }

    let current: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if current < 3 {
        apply_migration_v3(conn)?;
    }
    let current: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if current < 4 {
        apply_migration_v4(conn)?;
    }
    let current: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if current < 5 {
        apply_migration_v5(conn)?;
    }
    let current: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if current < 6 {
        apply_migration_v6(conn)?;
    }
    let current: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if current < 7 {
        apply_migration_v7(conn)?;
    }
    let current: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if current > MAX_KNOWN_VERSION {
        return Err(DbError::VersionMismatch { found: current, expected: MAX_KNOWN_VERSION });
    }
    Ok(())
}

/// Provision a per-session SQLite store under an explicit `root` directory.
///
/// Creates `{root}/ctxl/{session_id}/store.db`, enables WAL mode,
/// applies the schema, and writes a `.last_used` marker file.
///
/// # Errors
///
/// Returns `DbError::Io` if directory creation or marker-file write fails.
/// Returns `DbError::Sqlite` if the database cannot be opened or schema
/// application fails.
pub fn init_session_at(root: &Path, session_id: &str) -> Result<(), DbError> {
    let dir = session_dir_at(root, session_id);
    std::fs::create_dir_all(&dir)?;

    let db_path = dir.join("store.db");
    let conn = Connection::open(&db_path)?;
    conn.pragma_update(None, "busy_timeout", 5000)?;

    // WAL mode for concurrent reader/writer safety
    conn.execute_batch("PRAGMA journal_mode=WAL;")?;

    // Base schema (handles table, FTS, triggers, idx_handles_created)
    // and incremental migrations (v3 calls table) — all idempotent.
    apply_schema(&conn)?;

    // Touch the last-used marker so callers can GC stale sessions
    let last_used = dir.join(".last_used");
    std::fs::write(&last_used, b"")?;

    Ok(())
}

/// Apply the v2 → v3 migration: adds the `calls` table and its indexes.
///
/// Idempotent — a no-op when `PRAGMA user_version` is already 3 or higher.
///
/// Called exclusively by [`apply_schema`] — use that function to initialise
/// or upgrade a database instead of calling this directly.
fn apply_migration_v3(conn: &Connection) -> Result<(), DbError> {
    conn.execute_batch("BEGIN EXCLUSIVE")?;
    let current: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if current >= 3 {
        conn.execute_batch("COMMIT")?;
        return Ok(());
    }
    match (|| -> Result<(), DbError> {
        conn.execute_batch(
            r#"
CREATE TABLE IF NOT EXISTS calls (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    tool        TEXT    NOT NULL,
    tool_use_id TEXT,
    params      TEXT,
    cwd         TEXT,
    intercepted BOOLEAN NOT NULL,
    handle_id   TEXT,
    line_count  INTEGER,
    token_est   INTEGER,
    duration_ms INTEGER,
    exit_code   INTEGER,
    created_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_calls_tool ON calls(tool);
CREATE INDEX IF NOT EXISTS idx_calls_handle ON calls(handle_id);
"#,
        )?;
        conn.pragma_update(None, "user_version", 3)?;
        Ok(())
    })() {
        Ok(()) => conn.execute_batch("COMMIT")?,
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(e);
        }
    }
    Ok(())
}

/// Apply the v3 → v4 migration: rebuild FTS table with underscore-aware tokenizer.
///
/// Drops and recreates `content_fts` with `tokenchars="_"` so identifiers like
/// `test_foo_bar` are indexed as a single token (searchable as a phrase).
/// Rebuilds triggers and re-indexes existing handles.
///
/// Idempotent — a no-op when `PRAGMA user_version` is already 4 or higher.
///
/// Called exclusively by [`apply_schema`] — use that function to initialise
/// or upgrade a database instead of calling this directly.
fn apply_migration_v4(conn: &Connection) -> Result<(), DbError> {
    conn.execute_batch("BEGIN EXCLUSIVE")?;
    let current: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if current >= 4 {
        conn.execute_batch("COMMIT")?;
        return Ok(());
    }
    match (|| -> Result<(), DbError> {
        conn.execute_batch(
            r#"
DROP TRIGGER IF EXISTS handles_ai;
DROP TRIGGER IF EXISTS handles_ad;
DROP TABLE IF EXISTS content_fts;

CREATE VIRTUAL TABLE IF NOT EXISTS content_fts USING fts5(
    content,
    content='handles',
    content_rowid='rowid',
    tokenize='unicode61 tokenchars _'
);

CREATE TRIGGER IF NOT EXISTS handles_ai AFTER INSERT ON handles
    WHEN NEW.content IS NOT NULL
BEGIN
    INSERT INTO content_fts(rowid, content) VALUES (NEW.rowid, NEW.content);
END;

CREATE TRIGGER IF NOT EXISTS handles_ad AFTER DELETE ON handles
    WHEN OLD.content IS NOT NULL
BEGIN
    INSERT INTO content_fts(content_fts, rowid, content) VALUES ('delete', OLD.rowid, OLD.content);
END;

INSERT INTO content_fts(content_fts) VALUES ('rebuild');
"#,
        )?;
        conn.pragma_update(None, "user_version", 4)?;
        Ok(())
    })() {
        Ok(()) => conn.execute_batch("COMMIT")?,
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(e);
        }
    }
    Ok(())
}

/// Apply the v4 → v5 migration: adds `tool_input` column to `handles`.
///
/// Idempotent — a no-op when `PRAGMA user_version` is already 5 or higher.
///
/// Called exclusively by [`apply_schema`] — use that function to initialise
/// or upgrade a database instead of calling this directly.
fn apply_migration_v5(conn: &Connection) -> Result<(), DbError> {
    conn.execute_batch("BEGIN EXCLUSIVE")?;
    let current: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if current >= 5 {
        conn.execute_batch("COMMIT")?;
        return Ok(());
    }
    match (|| -> Result<(), DbError> {
        conn.execute_batch("ALTER TABLE handles ADD COLUMN tool_input TEXT;")?;
        conn.pragma_update(None, "user_version", 5)?;
        Ok(())
    })() {
        Ok(()) => conn.execute_batch("COMMIT")?,
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(e);
        }
    }
    Ok(())
}

/// Apply the v5 → v6 migration: adds retrieval tracking columns to `handles`.
///
/// Idempotent — a no-op when `PRAGMA user_version` is already 6 or higher.
///
/// Called exclusively by [`apply_schema`] — use that function to initialise
/// or upgrade a database instead of calling this directly.
fn apply_migration_v6(conn: &Connection) -> Result<(), DbError> {
    conn.execute_batch("BEGIN EXCLUSIVE")?;
    let current: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if current >= 6 {
        conn.execute_batch("COMMIT")?;
        return Ok(());
    }
    match (|| -> Result<(), DbError> {
        conn.execute_batch(
            "ALTER TABLE handles ADD COLUMN retrieval_count INTEGER NOT NULL DEFAULT 0;\
             ALTER TABLE handles ADD COLUMN last_retrieved_at INTEGER;",
        )?;
        conn.pragma_update(None, "user_version", 6)?;
        Ok(())
    })() {
        Ok(()) => conn.execute_batch("COMMIT")?,
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(e);
        }
    }
    Ok(())
}

/// Apply the v6 → v7 migration: adds global-cache columns to `handles`.
///
/// The new columns link a session handle back to its global blob in the
/// cross-session `global.db` store:
/// - `blob_id`   — row ID in `global.db`'s `blobs` table (NULL for session-only handles)
/// - `param_hash` — xxh128 of the normalized params JSON (for cache-check queries)
/// - `git_head`  — `HEAD` SHA at the time the handle was stored
///
/// Idempotent — a no-op when `PRAGMA user_version` is already 7 or higher.
///
/// Called exclusively by [`apply_schema`] — use that function to initialise
/// or upgrade a database instead of calling this directly.
fn apply_migration_v7(conn: &Connection) -> Result<(), DbError> {
    conn.execute_batch("BEGIN EXCLUSIVE")?;
    let current: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if current >= 7 {
        conn.execute_batch("COMMIT")?;
        return Ok(());
    }
    match (|| -> Result<(), DbError> {
        conn.execute_batch(
            "ALTER TABLE handles ADD COLUMN blob_id INTEGER;\
             ALTER TABLE handles ADD COLUMN param_hash TEXT;\
             ALTER TABLE handles ADD COLUMN git_head TEXT;",
        )?;
        conn.pragma_update(None, "user_version", 7)?;
        Ok(())
    })() {
        Ok(()) => conn.execute_batch("COMMIT")?,
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(e);
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod version_tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn fresh_db_gets_latest_version() {
        let conn = Connection::open_in_memory().unwrap();
        apply_schema(&conn).unwrap();
        let ver: i32 = conn.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
        assert_eq!(ver, MAX_KNOWN_VERSION);
    }

    #[test]
    fn idempotent_apply() {
        let conn = Connection::open_in_memory().unwrap();
        apply_schema(&conn).unwrap();
        apply_schema(&conn).unwrap(); // second call is a no-op
        let ver: i32 = conn.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
        assert_eq!(ver, MAX_KNOWN_VERSION);
    }

    #[test]
    fn future_version_rejected() {
        let conn = Connection::open_in_memory().unwrap();
        // Versions up to MAX_KNOWN_VERSION (7) are accepted as no-ops.
        // Only versions strictly above MAX_KNOWN_VERSION trigger VersionMismatch.
        conn.pragma_update(None, "user_version", MAX_KNOWN_VERSION + 1).unwrap();
        let err = apply_schema(&conn).unwrap_err();
        assert!(matches!(err, DbError::VersionMismatch { .. }));
    }

    #[test]
    fn concurrent_migration_neither_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("concurrent.db");
        // Create the file so both threads open the same DB.
        let conn0 = Connection::open(&db_path).unwrap();
        conn0.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
        drop(conn0);

        let p1 = db_path.clone();
        let p2 = db_path.clone();
        let t1 = std::thread::spawn(move || {
            let c = Connection::open(&p1).unwrap();
            apply_schema(&c)
        });
        let t2 = std::thread::spawn(move || {
            let c = Connection::open(&p2).unwrap();
            apply_schema(&c)
        });
        t1.join().unwrap().unwrap();
        t2.join().unwrap().unwrap();

        let conn = Connection::open(&db_path).unwrap();
        let ver: i32 = conn.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
        assert_eq!(ver, MAX_KNOWN_VERSION);
    }

    #[test]
    fn schema_creates_expected_tables() {
        let conn = Connection::open_in_memory().unwrap();
        apply_schema(&conn).unwrap();
        // Verify core tables exist
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(tables.contains(&"handles".to_string()));
        // Check for FTS table (content_fts or similar)
        assert!(tables.iter().any(|t| t.contains("fts")));
    }
}
