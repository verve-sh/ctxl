use std::fmt;

/// Typed error enum for the ctxl crate.
///
/// Replaces `Box<dyn std::error::Error>` across the public API surface,
/// enabling callers to match on specific failure modes.
#[derive(Debug)]
pub enum CtxlError {
    /// Wraps a [`crate::db::DbError`] from the database module.
    Db(crate::db::DbError),
    /// Direct SQLite error outside the `db` module.
    Sqlite(rusqlite::Error),
    /// JSON serialization / deserialization failure.
    Json(serde_json::Error),
    /// I/O error (stdin, stdout, filesystem).
    Io(std::io::Error),
    /// Requested handle ID was not found in the store.
    HandleNotFound(String),
    /// Both handle-ID generation attempts collided (UNIQUE constraint).
    HandleCollision,
    /// TTL string could not be parsed.
    InvalidTtl(String),
    /// Error during `ctxl index` processing.
    Index(String),
    /// Session ID contains invalid characters (path traversal defense).
    InvalidSessionId,
    /// Query contains regex metacharacters that FTS5 does not support.
    RegexInQuery(String),
    /// Search query is empty or whitespace-only.
    EmptyQuery,
}

impl fmt::Display for CtxlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CtxlError::Db(e) => write!(f, "{e}"),
            CtxlError::Sqlite(e) => write!(f, "sqlite: {e}"),
            CtxlError::Json(e) => write!(f, "json: {e}"),
            CtxlError::Io(e) => write!(f, "io: {e}"),
            CtxlError::HandleNotFound(id) => {
                write!(f, "handle not found: {id}")?;
                // If the ID doesn't look like a handle (missing prefix), teach usage.
                if !id.starts_with("b_") && !id.starts_with("g_") && !id.starts_with("i_") {
                    write!(
                        f,
                        "\n  hint: handle IDs start with b_, g_, or i_ (e.g. b_69e7a5)\n  usage: ctxl show <handle>  ·  ctxl search <handle> <query>"
                    )?;
                }
                Ok(())
            }
            CtxlError::HandleCollision => {
                write!(f, "handle ID collision after retry")
            }
            CtxlError::InvalidTtl(s) => write!(f, "invalid TTL: {s}"),
            CtxlError::Index(msg) => write!(f, "index: {msg}"),
            CtxlError::InvalidSessionId => {
                write!(f, "invalid session_id (alphanumeric, _, - only)")
            }
            CtxlError::RegexInQuery(msg) => write!(f, "{msg}"),
            CtxlError::EmptyQuery => write!(f, "error: query is empty"),
        }
    }
}

impl std::error::Error for CtxlError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CtxlError::Db(e) => Some(e),
            CtxlError::Sqlite(e) => Some(e),
            CtxlError::Json(e) => Some(e),
            CtxlError::Io(e) => Some(e),
            CtxlError::HandleNotFound(_) => None,
            CtxlError::HandleCollision => None,
            CtxlError::InvalidTtl(_) => None,
            CtxlError::Index(_) => None,
            CtxlError::InvalidSessionId => None,
            CtxlError::RegexInQuery(_) => None,
            CtxlError::EmptyQuery => None,
        }
    }
}

impl From<rusqlite::Error> for CtxlError {
    fn from(e: rusqlite::Error) -> Self {
        CtxlError::Sqlite(e)
    }
}

impl From<serde_json::Error> for CtxlError {
    fn from(e: serde_json::Error) -> Self {
        CtxlError::Json(e)
    }
}

impl From<std::io::Error> for CtxlError {
    fn from(e: std::io::Error) -> Self {
        CtxlError::Io(e)
    }
}

impl From<crate::db::DbError> for CtxlError {
    fn from(e: crate::db::DbError) -> Self {
        CtxlError::Db(e)
    }
}
