use rusqlite::Connection;

/// Metadata for an intercepted call, passed to [`insert_calls_row`].
pub struct InterceptCallMeta<'a> {
    pub tool: &'a str,
    pub handle_id: &'a str,
    pub line_count: i64,
    pub token_est: Option<i64>,
    pub tool_input: Option<&'a str>,
    pub cwd: Option<&'a str>,
    pub tool_use_id: Option<&'a str>,
    pub exit_code: Option<i64>,
}

/// Insert an intercepted-call row into the `calls` table.
///
/// Shared by all intercept handlers (Bash, Grep, WebFetch) and `ctxl index`.
/// Gated by `config.record` at each call site — this function always writes.
pub fn insert_calls_row(
    conn: &Connection,
    meta: &InterceptCallMeta<'_>,
) -> Result<(), crate::CtxlError> {
    let created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| std::io::Error::other(format!("system clock precedes Unix epoch: {e}")))?
        .as_secs() as i64;

    conn.execute(
        "INSERT INTO calls \
         (tool, intercepted, handle_id, line_count, token_est, \
          tool_use_id, params, cwd, exit_code, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        rusqlite::params![
            meta.tool,
            true,
            meta.handle_id,
            meta.line_count,
            meta.token_est,
            meta.tool_use_id,
            meta.tool_input,
            meta.cwd,
            meta.exit_code,
            created_at,
        ],
    )?;
    Ok(())
}
