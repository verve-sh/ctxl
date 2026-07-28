#![allow(missing_docs, clippy::print_stdout, clippy::print_stderr)] // CLI binary — stdout/stderr output is intentional

use clap::{Parser, Subcommand};

/// Context ledger CLI — per-session SQLite store for tool output.
#[derive(Parser)]
#[command(name = "ctxl")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Provision a per-session SQLite store.
    Init {
        /// UUID identifying this session.  May also be supplied via the
        /// `CTXL_SESSION_ID` environment variable.
        #[arg(long, env = "CTXL_SESSION_ID")]
        session_id: Option<String>,
    },
    /// Display stored output for a handle (bounded).
    Show {
        /// Handle ID (e.g. b_1a2b3c or g_1a2b3c).
        handle: String,
        /// Number of lines to skip before applying head/tail bounds.
        #[arg(long, default_value = "0")]
        offset: usize,
        /// Maximum number of lines to return (from the start).
        #[arg(long, default_value = "80", conflicts_with = "tail")]
        head: usize,
        /// Return the last N lines instead of the first.
        /// Cannot be combined with --file, --glob, or --exclude (which use head-only bounds).
        #[arg(long, conflicts_with_all = ["head", "file", "glob", "exclude"])]
        tail: Option<usize>,
        /// For Grep and diff handles: return only matches from this exact file path.
        #[arg(long)]
        file: Option<String>,
        /// For Grep and diff handles: return matches from files matching this glob (e.g. *.rs).
        #[arg(long)]
        glob: Option<String>,
        /// Exclude files matching these glob patterns (repeatable). Works with grep and diff handles.
        #[arg(long)]
        exclude: Vec<String>,
        /// Show the compressed/structural view instead of raw content.
        #[arg(long, conflicts_with_all = ["file", "glob", "exclude"])]
        compressed: bool,
        /// Show raw content (default). Explicit flag for clarity when compressed is available.
        #[arg(long, conflicts_with = "compressed")]
        raw: bool,
        /// Session ID.  May also be supplied via `CTXL_SESSION_ID`.
        #[arg(long, env = "CTXL_SESSION_ID")]
        session_id: Option<String>,
    },
    /// Show metadata for a stored handle without retrieving content.
    Inspect {
        /// Handle ID (e.g. b_1a2b3c or g_1a2b3c).
        handle: String,
        /// Human-readable key-value output instead of JSON.
        #[arg(long)]
        human: bool,
        /// Session ID.  May also be supplied via `CTXL_SESSION_ID`.
        #[arg(long, env = "CTXL_SESSION_ID")]
        session_id: Option<String>,
    },
    /// Search stored output using FTS5.  Per-handle, cross-handle (--all), or global (--global).
    ///
    /// Positional: `ctxl search <handle> <query>` (handle-first).
    /// Named flags: `ctxl search --handle <id> --query <text>` (order-independent).
    /// Global: `ctxl search --global <query>` — searches blobs_fts in global.db.
    Search {
        /// Handle ID (named flag, order-independent).
        #[arg(long = "handle")]
        handle_flag: Option<String>,
        /// Handle ID (first positional in positional mode).
        /// When --all or --global is set, this positional becomes the query string.
        #[arg(required_unless_present_any = ["all", "handle_flag", "global"])]
        handle: Option<String>,
        /// Search query (named flag, order-independent).
        #[arg(long = "query", short = 'q')]
        query_flag: Option<String>,
        /// Query string (second positional for per-handle search).
        query_pos: Option<String>,
        /// Search across all handles in the session.
        #[arg(long)]
        all: bool,
        /// Search the project cache across all sessions (blobs_fts in global.db).
        #[arg(long)]
        global: bool,
        /// Scope global search to this repo root (default: current repo via git).
        #[arg(long)]
        repo: Option<String>,
        /// Maximum number of results.
        #[arg(long, default_value = "20")]
        limit: usize,
        /// Session ID.  May also be supplied via `CTXL_SESSION_ID`.
        #[arg(long, env = "CTXL_SESSION_ID")]
        session_id: Option<String>,
    },
    /// Index stdin content — store, FTS5-index, and optionally search inline.
    ///
    /// Reads all of stdin, auto-detects content type (HTML → markdown,
    /// JSON → flattened, text → passthrough), stores in the session DB,
    /// and prints a handle reference.  Use `--hint` to get inline search
    /// results from the just-stored content.
    Index {
        /// Search stored content for this query, print inline results.
        #[arg(long)]
        hint: Option<String>,
        /// Override auto-detection: "html", "json", "text".
        #[arg(long)]
        content_type: Option<String>,
        /// Provenance URL stored as metadata (no HTTP fetch).
        #[arg(long)]
        source: Option<String>,
        /// Session ID.  May also be supplied via `CTXL_SESSION_ID`.
        #[arg(long, env = "CTXL_SESSION_ID")]
        session_id: Option<String>,
    },
    /// Remove stale session directories, or old global cache blobs (--global).
    Clean {
        /// Time-to-live for session dirs (e.g. 7d).
        #[arg(long, env = "CTXL_TTL", default_value = "7d")]
        ttl: String,
        /// Clean the global cache: remove blobs not used in the last 30 days.
        #[arg(long)]
        global: bool,
    },
    /// Check whether a cache hit exists for the given tool and params.
    ///
    /// Prints "cache hit: blob_id=N, stored <timestamp>" on hit, or a miss
    /// reason (git_head changed, no match, unavailable).
    CacheCheck {
        /// Tool name (e.g. Bash, Grep).
        #[arg(long)]
        tool: String,
        /// Output mode (stdout, stderr, mixed).  Omit to try all modes.
        #[arg(long)]
        output_mode: Option<String>,
        /// JSON params for the tool invocation.
        #[arg(long)]
        params: String,
    },
    /// Unified PostToolUse intercept — routes by `tool_name` in the JSON payload.
    ///
    /// Reads a PostToolUse JSON payload from stdin.  Dispatches to the
    /// appropriate handler based on `tool_name`:
    ///   - `Bash` → bash intercept (byte + line threshold)
    ///   - `Grep` → grep intercept (line threshold)
    ///   - `Read|Edit|Write|Glob` → record-only (calls table)
    ///   - unknown → exit 0 (passthrough)
    ///
    /// Thresholds are compiled-in defaults with env var overrides
    /// (`CTXL_BASH_THRESHOLD`, `CTXL_GREP_THRESHOLD`).
    Intercept {
        /// Session ID.  May also be supplied via `CTXL_SESSION_ID`.
        #[arg(long, env = "CTXL_SESSION_ID")]
        session_id: Option<String>,
    },
    /// List all files with match/hunk counts for a handle.
    ///
    /// Auto-detects content type:
    /// - Diff handles: counts `@@` hunks per file (uses "Hunks" header).
    /// - Grep handles: counts lines/matches per file (uses "Matches" header).
    ///
    /// Output: tabular display with file paths and counts.
    Files {
        /// Handle ID (e.g. g_1a2b3c or b_1a2b3c).
        handle: String,
        /// Session ID.  May also be supplied via `CTXL_SESSION_ID`.
        #[arg(long, env = "CTXL_SESSION_ID")]
        session_id: Option<String>,
    },
    /// Diagnostic report: binary, session DB, thresholds, counts.
    Doctor {
        /// Session ID.  May also be supplied via `CTXL_SESSION_ID`.
        #[arg(long, env = "CTXL_SESSION_ID")]
        session_id: Option<String>,
        /// Output as JSON instead of human-readable.
        #[arg(long)]
        json: bool,
    },
    /// Return bounded call history for a session.
    Calls {
        /// Session ID.  May also be supplied via `CTXL_SESSION_ID`.
        #[arg(long, env = "CTXL_SESSION_ID")]
        session_id: Option<String>,
        /// Maximum number of rows to return (default 20).
        #[arg(long, default_value = "20")]
        last: usize,
        /// Filter rows by tool name (e.g. Read, Grep, Glob).
        #[arg(long)]
        tool: Option<String>,
        /// Show only calls that produced handles.
        #[arg(long)]
        intercepted: bool,
    },
    /// Most recent call (shorthand for `calls --last 1`).
    Last {
        /// Session ID.  May also be supplied via `CTXL_SESSION_ID`.
        #[arg(long, env = "CTXL_SESSION_ID")]
        session_id: Option<String>,
        /// Show only the last intercepted call.
        #[arg(long)]
        intercepted: bool,
    },
    /// List stored handles in the current session.
    Handles {
        /// Session ID.  May also be supplied via `CTXL_SESSION_ID`.
        #[arg(long, env = "CTXL_SESSION_ID")]
        session_id: Option<String>,
        /// Maximum number of handles to return.
        #[arg(long, default_value = "20")]
        limit: usize,
        /// Output as JSON array.
        #[arg(long)]
        json: bool,
    },
    /// Show cumulative token savings across cleaned sessions.
    Stats {
        /// Show per-session breakdown.
        #[arg(long)]
        sessions: bool,
        /// Include active (uncleaned) sessions.
        #[arg(long)]
        live: bool,
        /// Maximum per-session rows to display.
        #[arg(long, default_value = "20")]
        limit: usize,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Hook entry points — called by shell hook stubs.
    Hook {
        #[command(subcommand)]
        action: HookAction,
    },
}

#[derive(Subcommand)]
enum HookAction {
    /// SessionStart — provision DB, run background clean.
    SessionStart {
        #[arg(long)]
        session_id: String,
    },
    /// PostToolUse — read stdin JSON, extract session_id, route to intercept.
    PostToolUse,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Open the SQLite connection for a session.
///
/// Respects `CTXL_CACHE_ROOT` env var so tests can redirect to a temp dir
/// without modifying the real `~/Library/Caches/ctxl/` tree.
fn open_session_db(session_id: &str) -> Result<rusqlite::Connection, ctxl::CtxlError> {
    if !is_valid_session_id(session_id) {
        return Err(ctxl::CtxlError::InvalidSessionId);
    }
    let root = resolve_cache_root();
    let dir = ctxl::db::session_dir_at(&root, session_id);
    // SQLite creates the file but not the directory — without this, a session
    // whose SessionStart init never ran can never be intercepted (fail-open
    // swallows the "unable to open database file" error every call).
    std::fs::create_dir_all(&dir)?;
    let conn = rusqlite::Connection::open(dir.join("store.db"))?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    ctxl::db::apply_schema(&conn)?;
    Ok(conn)
}

/// Record a retrieval call (ctxl show/search/files) into the session ledger.
///
/// Non-fatal — errors are silently ignored since recording failures must
/// never block retrieval.
fn record_retrieval(conn: &rusqlite::Connection, tool: &str, handle_id: &str, line_count: i64) {
    // Safety: duration_since(UNIX_EPOCH) only fails if the system clock is
    // before 1970 — a broken-system invariant, not a recoverable condition.
    // This function is intentionally non-fatal (returns ()), so ? is not an option.
    #[allow(clippy::expect_used)]
    let created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock precedes Unix epoch")
        .as_secs() as i64;

    let token_est = (line_count * 3).max(1); // rough estimate: ~3 tokens per line retrieved

    let _ = conn.execute(
        "INSERT INTO calls \
         (tool, intercepted, handle_id, line_count, token_est, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![tool, false, handle_id, line_count, token_est, created_at],
    );

    let _ = conn.execute(
        "UPDATE handles SET retrieval_count = retrieval_count + 1, \
         last_retrieved_at = ?1 WHERE id = ?2",
        rusqlite::params![created_at, handle_id],
    );
}

/// Refresh the `.last_used` marker for a session directory.
///
/// Called after successful retrieval (show, search, files, inspect) so that
/// the session TTL sweep considers the session recently active. Non-fatal —
/// write failures are silently ignored.
fn touch_session_last_used(session_id: &str) {
    let root = resolve_cache_root();
    let marker = ctxl::db::session_dir_at(&root, session_id).join(".last_used");
    let _ = std::fs::write(&marker, b"");
}

/// Format a Unix epoch timestamp as a human-readable UTC string.
fn chrono_format(epoch: i64) -> String {
    if epoch < 0 {
        return format!("{epoch}");
    }
    format_epoch(epoch)
}

fn format_epoch(epoch: i64) -> String {
    let total_secs = epoch as u64;
    let secs_in_day: u64 = 86400;
    let mut days = total_secs / secs_in_day;
    let rem = total_secs % secs_in_day;
    let hours = rem / 3600;
    let minutes = (rem % 3600) / 60;
    let seconds = rem % 60;

    // Days since 1970-01-01 → year/month/day
    let mut year: u64 = 1970;
    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }
    let month_days: [u64; 12] = if is_leap(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut month: u64 = 1;
    for &md in &month_days {
        if days < md {
            break;
        }
        days -= md;
        month += 1;
    }
    let day = days + 1;
    format!("{year:04}-{month:02}-{day:02} {hours:02}:{minutes:02}:{seconds:02} UTC")
}

fn is_leap(y: u64) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}

/// Return the cache root, respecting `CTXL_CACHE_ROOT` (set by session-start hook).
fn resolve_cache_root() -> std::path::PathBuf {
    if let Ok(root) = std::env::var("CTXL_CACHE_ROOT") {
        return std::path::PathBuf::from(root);
    }
    dirs::cache_dir().unwrap_or_else(|| {
        eprintln!("error: cache directory unavailable");
        std::process::exit(1);
    })
}

fn format_tokens(n: i64) -> String {
    let abs = n.unsigned_abs();
    let sign = if n < 0 { "-" } else { "" };
    if abs >= 1_000_000 {
        format!("{sign}{:.1}M", abs as f64 / 1_000_000.0)
    } else if abs >= 10_000 {
        format!("{sign}{}K", abs / 1_000)
    } else {
        format!("{sign}{}", separate_thousands(abs))
    }
}

fn separate_thousands(n: u64) -> String {
    let s = n.to_string();
    if s.len() <= 3 {
        return s;
    }
    let mut result = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            result.push(',');
        }
        result.push(ch);
    }
    result
}

fn is_valid_session_id(sid: &str) -> bool {
    !sid.is_empty() && sid.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Shared intercept core — processes a PostToolUse JSON payload against
/// a pre-opened connection, writing output to the provided writer.
///
/// Core intercept dispatch — routes by `tool_name` in the JSON payload.
///
/// Errors are written to stderr (never to `out`) — the caller sees an
/// empty response on failure, which is the fail-open contract.
/// Check whether recording is disabled for this session.
///
/// Evaluated per-request so that mid-session `CTXL_RECORD=0` or
/// sentinel file creation is respected immediately.
fn check_norecord(sid: &str) -> bool {
    std::env::var("CTXL_RECORD").ok().as_deref() == Some("0") || {
        let tmpdir = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into());
        std::path::Path::new(&tmpdir).join(format!("ctxl-norecord-{sid}")).exists()
    }
}

fn intercept_core<W: std::io::Write>(
    conn: &rusqlite::Connection,
    sid: &str,
    input: &str,
    out: &mut W,
) {
    let norecord = check_norecord(sid);
    let peeked = serde_json::from_str::<serde_json::Value>(input).ok();
    let tool_name = peeked
        .as_ref()
        .and_then(|v| v.get("tool_name")?.as_str().map(String::from))
        .unwrap_or_default();

    if !matches!(tool_name.as_str(), "Bash" | "Grep" | "Read" | "Edit" | "Write" | "Glob") {
        return;
    }

    if tool_name == "Bash" {
        let is_ctxl_cmd = peeked
            .as_ref()
            .and_then(|v| v.get("tool_input")?.get("command")?.as_str())
            .is_some_and(|cmd| {
                let trimmed = cmd.trim_start();
                if trimmed == "ctxl" || trimmed.starts_with("ctxl ") {
                    return true;
                }
                let first_word = trimmed.split_whitespace().next().unwrap_or("");
                let basename = std::path::Path::new(first_word)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("");
                if basename == "ctxl" {
                    return true;
                }
                if let Ok(ctxl_bin) = std::env::var("CTXL_BIN") {
                    if !ctxl_bin.is_empty() && trimmed.starts_with(&ctxl_bin) {
                        return true;
                    }
                }
                false
            });
        if is_ctxl_cmd {
            return;
        }
    }

    // Record-only tools (Read/Edit/Write/Glob) — no interception output
    if matches!(tool_name.as_str(), "Read" | "Edit" | "Write" | "Glob") {
        if norecord {
            return;
        }
        // record::record needs &mut Connection but we only have &Connection.
        // Open a temporary connection for the recording path.  This is the
        // same cost as the pre-refactor code (which opened a fresh conn).
        let root = resolve_cache_root();
        let db_path = ctxl::db::session_dir_at(&root, sid).join("store.db");
        let mut record_conn = match rusqlite::Connection::open(db_path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[ctxl] error opening session for record: {e}");
                return;
            }
        };
        if let Err(e) = ctxl::record::record(&mut record_conn, input) {
            eprintln!("[ctxl] error: {e}");
        }
        return;
    }

    let peeked_tool_input =
        peeked.as_ref().and_then(|v| v.get("tool_input")).map(|v| v.to_string());
    let peeked_cwd = peeked
        .as_ref()
        .and_then(|v| v.get("tool_input")?.get("cwd")?.as_str().map(String::from))
        .or_else(|| peeked.as_ref().and_then(|v| v.get("cwd")?.as_str().map(String::from)));
    let peeked_tool_use_id =
        peeked.as_ref().and_then(|v| v.get("tool_use_id")?.as_str().map(String::from));

    let result = match tool_name.as_str() {
        "Bash" => {
            let mut config = ctxl::intercept::InterceptConfig::from_env();
            config.record = !norecord;
            config.command = peeked
                .as_ref()
                .and_then(|v| v.get("tool_input")?.get("command")?.as_str())
                .map(String::from);
            config.tool_input = peeked_tool_input.clone();
            config.cwd = peeked_cwd.clone();
            config.tool_use_id = peeked_tool_use_id.clone();
            serde_json::from_str(input)
                .map_err(ctxl::CtxlError::from)
                .and_then(|payload| ctxl::intercept::run(payload, out, &config, conn))
        }
        "Grep" => {
            let mut config = ctxl::intercept_grep::GrepInterceptConfig::from_env();
            config.record = !norecord;
            config.tool_input = peeked_tool_input.clone();
            config.cwd = peeked_cwd.clone();
            config.tool_use_id = peeked_tool_use_id.clone();
            serde_json::from_str(input)
                .map_err(ctxl::CtxlError::from)
                .and_then(|payload| ctxl::intercept_grep::run(payload, out, &config, conn))
        }
        _ => Ok(()),
    };

    if let Err(e) = result {
        eprintln!("[ctxl] error: {e}");
    }
}

/// Shared intercept dispatch — used by both `Commands::Intercept` and `HookAction::PostToolUse`.
///
/// `session_id`: from CLI arg or peeked from JSON.
/// `prefetched_input`: when `Some`, stdin has already been read (hook post-tool-use path).
fn run_intercept(session_id: Option<String>, prefetched_input: Option<String>) {
    if std::env::var("CTXL_ENABLED").ok().as_deref() == Some("0") {
        return;
    }

    let input = match prefetched_input {
        Some(s) => s,
        None => {
            let mut buf = String::new();
            use std::io::Read as _;
            if let Err(e) = std::io::stdin().read_to_string(&mut buf) {
                eprintln!("[ctxl] error reading stdin: {e}");
                return;
            }
            buf
        }
    };

    let sid = match session_id {
        Some(id) => id,
        None => {
            eprintln!("[ctxl] error: session_id required (use --session-id or CTXL_SESSION_ID)");
            return;
        }
    };

    if !is_valid_session_id(&sid) {
        eprintln!("[ctxl] error: invalid session_id (alphanumeric, _, - only)");
        return;
    }

    // Pre-open gate: decide from the peeked tool_name whether this request
    // needs a session DB at all. CTXL_RECORD=0 with record-only tools must
    // produce ZERO filesystem I/O (#1476) — open_session_db now creates the
    // session dir, so the short-circuit has to happen before it. Unknown
    // tools likewise must not create session dirs as a side effect.
    let peeked_tool = serde_json::from_str::<serde_json::Value>(&input)
        .ok()
        .and_then(|v| v.get("tool_name")?.as_str().map(String::from))
        .unwrap_or_default();
    match peeked_tool.as_str() {
        "Bash" | "Grep" => {}
        "Read" | "Edit" | "Write" | "Glob" => {
            if check_norecord(&sid) {
                return;
            }
        }
        _ => return,
    }

    let conn = match open_session_db(&sid) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[ctxl] error opening session: {e}");
            return;
        }
    };

    intercept_core(&conn, &sid, &input, &mut std::io::stdout());
}

/// Check whether a string looks like a ctxl handle ID.
///
/// Handles are `<prefix>_<hex>` where prefix is one of `b`, `g`, or `i`
/// and hex is 6-7 digits (total 8-9 chars). We accept 6-8 hex digits
/// (total 8-10 chars) to tolerate common test fixtures like `b_deadbeef`.
fn looks_like_handle(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() < 8 || bytes.len() > 10 {
        return false;
    }
    matches!(bytes[0], b'b' | b'g' | b'i')
        && bytes[1] == b'_'
        && bytes[2..].iter().all(|b| b.is_ascii_hexdigit())
}
fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Init { session_id } => {
            let id = match session_id {
                Some(id) => id,
                None => {
                    eprintln!("session_id required");
                    std::process::exit(1);
                }
            };

            let root = resolve_cache_root();
            if let Err(e) = ctxl::db::init_session_at(&root, &id) {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }

        Commands::Show {
            handle,
            offset,
            head,
            tail,
            file,
            glob,
            exclude,
            compressed,
            raw: _,
            session_id,
        } => {
            let sid = match session_id {
                Some(id) => id,
                None => {
                    eprintln!("error: session_id required (use --session-id or CTXL_SESSION_ID)");
                    std::process::exit(1);
                }
            };

            let conn = match open_session_db(&sid) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            };

            // Use filtered retrieval when --file, --glob, or --exclude is requested.
            let result = if file.is_some() || glob.is_some() || !exclude.is_empty() {
                ctxl::retrieve::show_filtered(
                    &conn,
                    &handle,
                    file.as_deref(),
                    glob.as_deref(),
                    &exclude,
                    head,
                    offset,
                )
            } else {
                ctxl::retrieve::show(
                    &conn,
                    &handle,
                    ctxl::retrieve::ShowOpts { head, tail, offset, compressed },
                )
            };

            match result {
                Ok(content) => {
                    let line_count = content.lines().count() as i64;
                    record_retrieval(&conn, "ctxl-show", &handle, line_count);
                    touch_session_last_used(&sid);
                    if content.is_empty() {
                        if let Some(ref f) = file {
                            eprintln!(
                                "[ctxl] No matches for --file \"{f}\". Run: ctxl files {handle}"
                            );
                        } else if let Some(ref g) = glob {
                            eprintln!(
                                "[ctxl] No matches for --glob \"{g}\". Run: ctxl files {handle}"
                            );
                        }
                    }
                    print!("{content}");
                }
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
        }

        Commands::Inspect { handle, human, session_id } => {
            let sid = match session_id {
                Some(id) => id,
                None => {
                    eprintln!("error: session_id required (use --session-id or CTXL_SESSION_ID)");
                    std::process::exit(1);
                }
            };

            let conn = match open_session_db(&sid) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            };

            match ctxl::retrieve::inspect(&conn, &handle) {
                Ok(info) => {
                    record_retrieval(&conn, "ctxl-inspect", &handle, 0);
                    touch_session_last_used(&sid);

                    let mut suggested = vec![
                        format!("ctxl show {}", info.id),
                        format!("ctxl search {} \"query\"", info.id),
                    ];
                    if info.tool == "Grep" {
                        suggested.insert(1, format!("ctxl files {}", info.id));
                    }

                    if human {
                        let compressed_display = info.compressed_method.as_deref().unwrap_or("—");
                        let ts = chrono_format(info.created_at);
                        println!("handle:      {}", info.id);
                        println!("tool:        {}", info.tool);
                        println!("mode:        {}", info.output_mode);
                        println!(
                            "lines:       {}",
                            info.line_count.map_or("—".to_string(), |v| v.to_string())
                        );
                        println!(
                            "tokens:      {}",
                            info.token_est.map_or("—".to_string(), |v| format!("~{v}"))
                        );
                        println!("truncated:   {}", if info.truncated { "yes" } else { "no" });
                        println!("compressed:  {compressed_display}");
                        println!("cwd:         {}", info.cwd.as_deref().unwrap_or("—"));
                        if let Some(ref ti) = info.tool_input {
                            println!("tool_input:  {ti}");
                        }
                        println!("created:     {ts}");
                        println!("retrievals:  {}", info.retrieval_count);
                        println!(
                            "last_retrieved: {}",
                            info.last_retrieved_at.map_or("—".to_string(), chrono_format)
                        );
                        if info.truncated {
                            println!();
                            println!("Note: content is partial — search covers stored lines only");
                        }
                        println!();
                        println!("Suggested:");
                        for cmd in &suggested {
                            println!("  {cmd}");
                        }
                    } else {
                        let mut json = serde_json::json!({
                            "id": info.id,
                            "tool": info.tool,
                            "mode": info.output_mode,
                            "lines": info.line_count,
                            "tokens": info.token_est,
                            "truncated": info.truncated,
                            "compressed": info.compressed_method,
                            "cwd": info.cwd,
                            "tool_input": info.tool_input,
                            "created_at": info.created_at,
                            "retrieval_count": info.retrieval_count,
                            "last_retrieved_at": info.last_retrieved_at,
                            "suggested": suggested,
                        });
                        if info.truncated {
                            json["note"] = serde_json::json!(
                                "content is partial — search covers stored lines only"
                            );
                        }
                        println!("{json}");
                    }
                }
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
        }

        Commands::Search {
            handle_flag,
            handle,
            query_flag,
            query_pos,
            all,
            global,
            repo,
            limit,
            session_id,
        } => {
            if global {
                // --global mode: search blobs_fts in global.db.
                let q = match query_flag.or(handle).or(query_pos) {
                    Some(q) => q,
                    None => {
                        eprintln!("error: query required with --global");
                        std::process::exit(1);
                    }
                };

                // Determine repo_root scope.
                let repo_root = repo.or_else(|| {
                    std::process::Command::new("git")
                        .args(["rev-parse", "--show-toplevel"])
                        .output()
                        .ok()
                        .filter(|o| o.status.success())
                        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                });

                let db_path = match ctxl::global_db::global_db_path() {
                    Some(p) => p,
                    None => {
                        eprintln!("error: cache directory unavailable");
                        std::process::exit(1);
                    }
                };
                let gconn = match ctxl::global_db::open_global_db(&db_path) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("error opening global cache: {e}");
                        std::process::exit(1);
                    }
                };

                match ctxl::global_db::search_global(&gconn, &q, repo_root.as_deref(), limit) {
                    Ok(results) if results.is_empty() => {
                        eprintln!("no global results for \"{q}\"");
                    }
                    Ok(results) => {
                        for r in results {
                            println!(
                                "[blob_id={} tool={} repo={} at={}] {}",
                                r.blob_id, r.tool, r.repo_root, r.created_at, r.snippet
                            );
                        }
                    }
                    Err(e) => {
                        eprintln!("error: {e}");
                        std::process::exit(1);
                    }
                }
                return;
            }

            let sid = match session_id {
                Some(id) => id,
                None => {
                    eprintln!("error: session_id required (use --session-id or CTXL_SESSION_ID)");
                    std::process::exit(1);
                }
            };

            let conn = match open_session_db(&sid) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            };

            if all {
                // --all mode: query comes from --query flag or first positional.
                let q = match query_flag.or(handle) {
                    Some(q) => q,
                    None => {
                        eprintln!("error: query required with --all");
                        std::process::exit(1);
                    }
                };
                match ctxl::retrieve::search_all(&conn, &q, limit) {
                    Ok(matches) if matches.is_empty() => {
                        record_retrieval(&conn, "ctxl-search-all", "--all", 0);
                        touch_session_last_used(&sid);
                        eprintln!("no results for \"{q}\" across session handles");
                    }
                    Ok(matches) => {
                        let line_count = matches.len() as i64;
                        record_retrieval(&conn, "ctxl-search-all", "--all", line_count);
                        touch_session_last_used(&sid);
                        print!("{}", ctxl::retrieve::format_search_all(&matches));
                    }
                    Err(e) => {
                        eprintln!("{e}");
                        std::process::exit(1);
                    }
                }
            } else {
                // Per-handle mode: resolve from named flags or positionals.
                let h = match handle_flag.or(handle) {
                    Some(h) => h,
                    None => {
                        eprintln!("error: handle required (or use --all for cross-handle search)");
                        std::process::exit(1);
                    }
                };
                let q = match query_flag.or(query_pos) {
                    Some(q) => q,
                    None => {
                        eprintln!("error: query required");
                        std::process::exit(1);
                    }
                };

                // Detect reversed positional arguments (query first, handle second).
                if !looks_like_handle(&h) && looks_like_handle(&q) {
                    eprintln!(
                        "error: arguments appear reversed \
                         — expected: ctxl search <handle> <query>\n\
                         hint: use --handle and --query flags for order-independent invocation"
                    );
                    std::process::exit(1);
                }

                match ctxl::retrieve::search(&conn, &h, &q, limit) {
                    Ok(lines) if lines.is_empty() => {
                        record_retrieval(&conn, "ctxl-search", &h, 0);
                        touch_session_last_used(&sid);
                        if q.contains('-') {
                            eprintln!(
                                "no results for \"{q}\" in {h} \
                                 (FTS5 treats - as a token boundary; \
                                 try the exact compound token instead)"
                            );
                        } else {
                            eprintln!("no results for \"{q}\" in {h}");
                        }
                    }
                    Ok(lines) => {
                        let line_count = lines.len() as i64;
                        record_retrieval(&conn, "ctxl-search", &h, line_count);
                        touch_session_last_used(&sid);
                        for line in lines {
                            println!("{line}");
                        }
                    }
                    Err(e) => {
                        eprintln!("{e}");
                        std::process::exit(1);
                    }
                }
            }
        }

        Commands::Index { hint, content_type, source, session_id } => {
            let sid = match session_id {
                Some(id) => id,
                None => {
                    eprintln!("error: session_id required (use --session-id or CTXL_SESSION_ID)");
                    std::process::exit(1);
                }
            };

            let conn = match open_session_db(&sid) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            };

            // Read all of stdin.
            let mut input = String::new();
            use std::io::Read as _;
            if let Err(e) = std::io::stdin().read_to_string(&mut input) {
                eprintln!("error reading stdin: {e}");
                std::process::exit(1);
            }

            let ct = content_type.as_deref().and_then(|s| match s {
                "html" => Some(ctxl::index::ContentType::Html),
                "json" => Some(ctxl::index::ContentType::Json),
                "text" => Some(ctxl::index::ContentType::Text),
                other => {
                    eprintln!("warning: unknown content-type \"{other}\", using auto-detection");
                    None
                }
            });

            let opts = ctxl::index::IndexOpts { hint, content_type: ct, source };

            match ctxl::index::index_content(&conn, &input, &opts) {
                Ok(result) => {
                    println!("{}", ctxl::index::format_output(&result));
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
        }

        Commands::Intercept { session_id } => {
            run_intercept(session_id, None);
        }

        Commands::Hook { action } => match action {
            HookAction::SessionStart { session_id } => {
                if !is_valid_session_id(&session_id) {
                    eprintln!("[ctxl] error: invalid session_id");
                    std::process::exit(1);
                }
                let root = resolve_cache_root();
                if let Err(e) = ctxl::db::init_session_at(&root, &session_id) {
                    eprintln!("[ctxl] error: {e}");
                    std::process::exit(1);
                }
                // Inline clean — SessionStart fires once per session; sweep is ~100ms I/O.
                let _ = ctxl::clean::sweep(&root, std::time::Duration::from_secs(7 * 24 * 3600));
                eprintln!("[ctxl] session initialized: {session_id}");
                // Liveness: the fail-open hook chain breaks silently — surface
                // "interception stopped producing handles" at the one moment
                // the user sees ctxl output.
                let live = ctxl::doctor::liveness(&root, Some(&session_id));
                if let Some(w) = live.warning {
                    eprintln!("[ctxl] warn: {w}");
                }
            }
            HookAction::PostToolUse => {
                // Read stdin, peek session_id from JSON, delegate to shared dispatch.
                let mut input = String::new();
                use std::io::Read as _;
                if let Err(e) = std::io::stdin().read_to_string(&mut input) {
                    eprintln!("[ctxl] error reading stdin: {e}");
                    return;
                }

                // Prefer CTXL_SESSION_ID (set by ctxl-init.sh at session start) over
                // the JSON payload's session_id.  Retrieval commands (show, search,
                // files) resolve via CTXL_SESSION_ID, so interception must write to
                // the same DB.  After `/clear`, Claude Code emits a new session_id
                // in the payload while CTXL_SESSION_ID retains the init value —
                // using the payload first causes write/read DB divergence.
                let peeked_sid = serde_json::from_str::<serde_json::Value>(&input)
                    .ok()
                    .and_then(|v| v.get("session_id")?.as_str().map(String::from));

                let sid = std::env::var("CTXL_SESSION_ID").ok().or(peeked_sid);

                run_intercept(sid, Some(input));
            }
        },

        Commands::Stats { sessions, live, limit, json } => {
            let db_path = match ctxl::global_db::global_db_path() {
                Some(p) if p.exists() => Some(p),
                _ => None,
            };

            let gconn = db_path.and_then(|p| ctxl::global_db::open_global_db(&p).ok());

            let cumulative =
                gconn.as_ref().and_then(|c| ctxl::global_db::query_cumulative_stats(c).ok());

            let mut session_rows: Vec<ctxl::global_db::SessionSummary> = if sessions {
                gconn
                    .as_ref()
                    .and_then(|c| ctxl::global_db::query_session_summaries(c, limit).ok())
                    .unwrap_or_default()
            } else {
                Vec::new()
            };

            // --live: scan active session dirs
            let mut live_summaries: Vec<ctxl::global_db::SessionSummary> = Vec::new();
            if live {
                let cache_root = resolve_cache_root();
                let ctxl_dir = cache_root.join("ctxl");
                if let Ok(entries) = std::fs::read_dir(&ctxl_dir) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if !path.is_dir() {
                            continue;
                        }
                        let store_db = path.join("store.db");
                        if !store_db.exists() {
                            continue;
                        }
                        if let Some(sid) = path.file_name().and_then(|n| n.to_str()) {
                            if sid == "global.db" {
                                continue;
                            }
                            if let Ok(mut summary) =
                                ctxl::global_db::compute_session_stats(&store_db)
                            {
                                summary.session_id = sid.to_string();
                                summary.cleaned_at = Some("[live]".to_string());
                                live_summaries.push(summary);
                            }
                        }
                    }
                }
            }

            // Merge live stats into cumulative for display
            let mut merged = cumulative.unwrap_or(ctxl::global_db::CumulativeStats {
                sessions_count: 0,
                handles_count: 0,
                tokens_intercepted: 0,
                bytes_intercepted: 0,
                calls_count: 0,
                calls_intercepted: 0,
                retrieval_calls: 0,
                retrieval_tokens: 0,
                handles_retrieved: 0,
                total_retrievals: 0,
                earliest_cleaned: None,
                latest_cleaned: None,
            });

            for ls in &live_summaries {
                merged.sessions_count += 1;
                merged.handles_count += ls.handles_count;
                merged.tokens_intercepted += ls.tokens_intercepted;
                merged.bytes_intercepted += ls.bytes_intercepted;
                merged.calls_count += ls.calls_count;
                merged.calls_intercepted += ls.calls_intercepted;
                merged.retrieval_calls += ls.retrieval_calls;
                merged.retrieval_tokens += ls.retrieval_tokens;
                merged.handles_retrieved += ls.handles_retrieved;
                merged.total_retrievals += ls.total_retrievals;
            }

            if sessions {
                session_rows.extend(live_summaries);
            }

            if json {
                let out = serde_json::json!({
                    "cumulative": merged,
                    "sessions": session_rows,
                });
                println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
            } else if merged.sessions_count == 0 {
                println!("No sessions tracked yet. Stats accumulate as sessions are cleaned.");
            } else {
                let net_saved = merged.tokens_intercepted - merged.retrieval_tokens;
                let efficiency = if merged.tokens_intercepted > 0 {
                    (net_saved as f64 / merged.tokens_intercepted as f64 * 100.0) as u64
                } else {
                    0
                };
                let per_session =
                    if merged.sessions_count > 0 { net_saved / merged.sessions_count } else { 0 };
                let utilization = if merged.handles_count > 0 {
                    (merged.handles_retrieved as f64 / merged.handles_count as f64 * 100.0) as u64
                } else {
                    0
                };
                let capture_rate = if merged.calls_count > 0 {
                    (merged.calls_intercepted as f64 / merged.calls_count as f64 * 100.0) as u64
                } else {
                    0
                };

                let date_range = match (&merged.earliest_cleaned, &merged.latest_cleaned) {
                    (Some(e), Some(l)) => {
                        let e_short = &e[..10.min(e.len())];
                        let l_short = &l[..10.min(l.len())];
                        format!(" ({e_short} \u{2013} {l_short})")
                    }
                    _ => String::new(),
                };

                let session_word = if merged.sessions_count == 1 { "session" } else { "sessions" };

                println!(
                    "ctxl has saved ~{} tokens across {} {}{}.{}",
                    format_tokens(net_saved),
                    merged.sessions_count,
                    session_word,
                    date_range,
                    if efficiency > 0 {
                        format!(
                            "\nThat's {}% of intercepted content you never had to read.",
                            efficiency
                        )
                    } else {
                        String::new()
                    }
                );
                println!();
                println!(
                    "  Saved    {:>12} tokens net  ({} intercepted \u{2212} {} retrieval)",
                    format_tokens(net_saved),
                    format_tokens(merged.tokens_intercepted),
                    format_tokens(merged.retrieval_tokens)
                );
                println!(
                    "  Efficiency    {:>4}%             only {}% of captured content was retrieved",
                    efficiency,
                    100u64.saturating_sub(efficiency)
                );
                println!(
                    "  Per session {:>6} tokens       average net savings",
                    format_tokens(per_session)
                );
                println!(
                    "  Handles    {:>7} created      {} retrieved ({}% utilization)",
                    format_tokens(merged.handles_count),
                    format_tokens(merged.handles_retrieved),
                    utilization
                );
                println!(
                    "  Calls      {:>7} recorded     {} intercepted ({}% capture rate)",
                    format_tokens(merged.calls_count),
                    format_tokens(merged.calls_intercepted),
                    capture_rate
                );

                if sessions && !session_rows.is_empty() {
                    println!();
                    println!(
                        "  {:>36}  {:>8}  {:>8}  {:>6}  {:>6}",
                        "SESSION", "TOKENS", "HANDLES", "CALLS", "STATUS"
                    );
                    for row in &session_rows {
                        let net = row.tokens_intercepted - row.retrieval_tokens;
                        let status = row.cleaned_at.as_deref().unwrap_or("cleaned");
                        let sid = if row.session_id.len() > 36 {
                            &row.session_id[..36]
                        } else {
                            &row.session_id
                        };
                        println!(
                            "  {:>36}  {:>8}  {:>8}  {:>6}  {:>6}",
                            sid,
                            format_tokens(net),
                            row.handles_count,
                            row.calls_count,
                            status
                        );
                    }
                }
            }
        }

        Commands::Clean { ttl, global } => {
            if global {
                // Global cache clean — remove blobs older than 30 days by last_used.
                const GLOBAL_TTL_DAYS: u32 = 30;
                let db_path = match ctxl::global_db::global_db_path() {
                    Some(p) => p,
                    None => {
                        eprintln!("error: cache directory unavailable");
                        std::process::exit(1);
                    }
                };
                let conn = match ctxl::global_db::open_global_db(&db_path) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("error opening global cache: {e}");
                        std::process::exit(1);
                    }
                };
                match ctxl::global_db::clean_old_blobs(&conn, GLOBAL_TTL_DAYS) {
                    Ok((count, bytes)) => {
                        println!("Cleaned {count} blobs ({bytes} bytes freed)");
                    }
                    Err(e) => {
                        eprintln!("error: {e}");
                        std::process::exit(1);
                    }
                }
            } else {
                let duration = match ctxl::clean::parse_ttl(&ttl) {
                    Ok(d) => d,
                    Err(e) => {
                        eprintln!("error: {e}");
                        std::process::exit(1);
                    }
                };

                let cache_root = resolve_cache_root();

                if let Err(e) = ctxl::clean::sweep(&cache_root, duration) {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
        }

        Commands::CacheCheck { tool, output_mode, params } => {
            let ctx = ctxl::cache::CacheContext::new();
            let result = ctxl::cache::cache_check(&ctx, &tool, output_mode.as_deref(), &params);
            println!("{result}");
        }

        Commands::Files { handle, session_id } => {
            let sid = match session_id {
                Some(id) => id,
                None => {
                    eprintln!("error: session_id required (use --session-id or CTXL_SESSION_ID)");
                    std::process::exit(1);
                }
            };

            let conn = match open_session_db(&sid) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            };

            match ctxl::retrieve::files(&conn, &handle) {
                Ok(rows) => {
                    let line_count = rows.len() as i64;
                    record_retrieval(&conn, "ctxl-files", &handle, line_count);
                    touch_session_last_used(&sid);
                    for row in rows {
                        println!("{row}");
                    }
                }
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
        }

        Commands::Doctor { session_id, json } => {
            let root = resolve_cache_root();
            let report = ctxl::doctor::collect(session_id.as_deref(), &root);
            if json {
                println!("{}", ctxl::doctor::format_json(&report));
            } else {
                print!("{}", ctxl::doctor::format_human(&report));
            }
        }

        Commands::Calls { session_id, last, tool, intercepted } => {
            let sid = match session_id {
                Some(id) => id,
                None => {
                    eprintln!("error: session_id required (use --session-id or CTXL_SESSION_ID)");
                    std::process::exit(1);
                }
            };

            let conn = match open_session_db(&sid) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            };

            let opts = ctxl::retrieve::CallsOpts {
                last,
                tool,
                intercepted: if intercepted { Some(true) } else { None },
            };
            match ctxl::retrieve::calls(&conn, opts) {
                Ok(rows) => {
                    for row in rows {
                        println!("{row}");
                    }
                }
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
        }

        Commands::Last { session_id, intercepted } => {
            let sid = match session_id {
                Some(id) => id,
                None => {
                    eprintln!("error: session_id required (use --session-id or CTXL_SESSION_ID)");
                    std::process::exit(1);
                }
            };

            let conn = match open_session_db(&sid) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            };

            let opts = ctxl::retrieve::CallsOpts {
                last: 1,
                tool: None,
                intercepted: if intercepted { Some(true) } else { None },
            };
            match ctxl::retrieve::calls(&conn, opts) {
                Ok(rows) => {
                    for row in rows {
                        println!("{row}");
                    }
                }
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
        }

        Commands::Handles { session_id, limit, json } => {
            let sid = match session_id {
                Some(id) => id,
                None => {
                    eprintln!("error: session_id required (use --session-id or CTXL_SESSION_ID)");
                    std::process::exit(1);
                }
            };

            let conn = match open_session_db(&sid) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            };

            match ctxl::retrieve::list_handles(&conn, limit) {
                Ok(handles) => {
                    if json {
                        let arr: Vec<serde_json::Value> = handles
                            .iter()
                            .map(|h| {
                                serde_json::json!({
                                    "id": h.id,
                                    "tool": h.tool,
                                    "line_count": h.line_count,
                                    "token_est": h.token_est,
                                    "created_at": h.created_at,
                                    "retrieval_count": h.retrieval_count,
                                })
                            })
                            .collect();
                        println!("{}", serde_json::to_string_pretty(&arr).unwrap_or_default());
                    } else if handles.is_empty() {
                        println!("No handles in this session.");
                    } else {
                        println!(
                            "{:<12} {:<8} {:>6} {:>8} {:>5} CREATED",
                            "HANDLE", "TOOL", "LINES", "TOKENS", "RETR"
                        );
                        for h in &handles {
                            println!(
                                "{:<12} {:<8} {:>6} {:>8} {:>5} {}",
                                h.id,
                                h.tool,
                                h.line_count.map_or("\u{2014}".to_string(), |v| v.to_string()),
                                h.token_est.map_or("\u{2014}".to_string(), |v| v.to_string()),
                                h.retrieval_count,
                                chrono_format(h.created_at),
                            );
                        }
                    }
                }
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn chrono_format_negative_epoch() {
        assert_eq!(chrono_format(-1), "-1");
        assert_eq!(chrono_format(-999), "-999");
    }

    #[test]
    fn chrono_format_zero_epoch() {
        let result = chrono_format(0);
        assert!(result.contains("1970"), "epoch 0 should format as 1970-01-01");
    }

    #[test]
    fn looks_like_handle_valid() {
        assert!(looks_like_handle("b_1a2b3c4d"));
        assert!(looks_like_handle("g_abcdef"));
        assert!(!looks_like_handle("w_123456"));
        assert!(looks_like_handle("i_aabbcc"));
    }

    #[test]
    fn looks_like_handle_too_short() {
        assert!(!looks_like_handle("b_1a2b"));
    }

    #[test]
    fn looks_like_handle_wrong_prefix() {
        assert!(!looks_like_handle("x_1a2b3c4d"));
    }

    #[test]
    fn looks_like_handle_no_underscore() {
        assert!(!looks_like_handle("b1a2b3c4d"));
    }

    #[test]
    fn looks_like_handle_non_hex() {
        assert!(!looks_like_handle("b_1a2b3cXY"));
    }
}
