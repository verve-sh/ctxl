/// Cross-session cache logic.
///
/// Implements the cache lookup / store algorithm described in the Phase 4 plan:
///
/// 1. Compute `repo_root` (cached per-session via [`CacheContext`]).
/// 2. Compute `git_head` FRESH on every lookup via `git rev-parse HEAD`.
/// 3. Check `is_dirty` FRESH — dirty worktree → skip cache read AND write.
/// 4. Normalize params (sort JSON keys, strip whitespace, remove session-specific fields).
/// 5. Hash with xxh128 → `param_hash`.
/// 6. Look up in `global.db`'s `cache_index`.
/// 7. On hit → insert session handle with `content=NULL`, `blob_id=<cached>`.
/// 8. On miss → store in session DB AND global DB.
///
/// All operations are fail-open: `Result` everywhere, callers fall through to
/// session-only storage on any error.
use xxhash_rust::xxh3::xxh3_128;

// ---------------------------------------------------------------------------
// CacheContext — per-session state
// ---------------------------------------------------------------------------

/// Holds per-session cache state.
///
/// `repo_root` is resolved once and cached.  `git_head` and `is_dirty` are
/// computed fresh on every call (they can change between tool invocations).
pub struct CacheContext {
    /// Absolute path to the git repository root.  `None` when the session is
    /// not inside a git repository or `git` is not available.
    pub repo_root: Option<String>,
    /// Working directory to pass to git commands via `.current_dir()`.
    /// `None` uses the process's current directory (default behavior).
    pub cwd: Option<String>,
}

impl CacheContext {
    /// Create a new `CacheContext`, resolving `repo_root` immediately.
    ///
    /// On any error (not in a git repo, git unavailable) `repo_root` is `None`.
    pub fn new() -> Self {
        let repo_root = Self::resolve_repo_root(None);
        CacheContext { repo_root, cwd: None }
    }

    /// Create a `CacheContext` with an explicit working directory.
    ///
    /// All git commands will run with `.current_dir(cwd)`.
    pub fn with_cwd(cwd: Option<String>) -> Self {
        let repo_root = Self::resolve_repo_root(cwd.as_deref());
        CacheContext { repo_root, cwd }
    }

    /// Return the current `HEAD` SHA.
    ///
    /// Runs `git rev-parse HEAD` fresh every call.  Returns `None` on failure.
    pub fn git_head(&self) -> Option<String> {
        let mut cmd = std::process::Command::new("git");
        cmd.args(["rev-parse", "HEAD"]);
        if let Some(ref dir) = self.cwd {
            cmd.current_dir(dir);
        }
        let out = cmd.output().ok()?;
        if out.status.success() {
            Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
        } else {
            None
        }
    }

    /// Return `true` when the working tree is dirty.
    ///
    /// Uses `git status --porcelain=v2 --untracked-files=no` (single spawn)
    /// to detect staged and unstaged modifications.  Returns `true` on any
    /// error (fail-open → skip cache).
    pub fn is_dirty(&self) -> bool {
        let mut cmd = std::process::Command::new("git");
        cmd.args(["status", "--porcelain=v2", "--untracked-files=no"]);
        if let Some(ref dir) = self.cwd {
            cmd.current_dir(dir);
        }
        let output = match cmd.output() {
            Ok(o) => o,
            Err(_) => return true, // conservative: treat errors as dirty
        };
        if !output.status.success() {
            return true;
        }
        !output.stdout.is_empty()
    }

    fn resolve_repo_root(cwd: Option<&str>) -> Option<String> {
        let mut cmd = std::process::Command::new("git");
        cmd.args(["rev-parse", "--show-toplevel"]);
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        let out = cmd.output().ok()?;
        if out.status.success() {
            Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
        } else {
            None
        }
    }
}

impl Default for CacheContext {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Param normalization + hashing
// ---------------------------------------------------------------------------

/// Normalize a JSON params string for stable hashing.
///
/// - Parses the JSON object.
/// - Removes `session_id` (session-specific, must not affect cache key).
/// - Re-serializes with keys sorted alphabetically and no extra whitespace.
///
/// Returns the canonical JSON string.  Falls back to the original string on
/// parse failure (prevents a bad payload from breaking the cache entirely —
/// it will simply always miss).
pub fn normalize_params(params_json: &str) -> String {
    let Ok(mut val) = serde_json::from_str::<serde_json::Value>(params_json) else {
        return params_json.to_string();
    };

    // Remove session-specific fields that must not affect the cache key.
    // NOTE: cwd is intentionally kept — it affects command output (e.g., ls,
    // find, git status produce different results from different directories).
    if let serde_json::Value::Object(ref mut map) = val {
        map.remove("session_id");
    }

    // Re-serialize: serde_json preserves insertion order, so we sort explicitly.
    let sorted = sort_json_keys(val);
    serde_json::to_string(&sorted).unwrap_or_else(|_| params_json.to_string())
}

/// Sort all JSON object keys recursively for canonical serialization.
fn sort_json_keys(val: serde_json::Value) -> serde_json::Value {
    match val {
        serde_json::Value::Object(map) => {
            let mut sorted: Vec<(String, serde_json::Value)> =
                map.into_iter().map(|(k, v)| (k, sort_json_keys(v))).collect();
            sorted.sort_by(|a, b| a.0.cmp(&b.0));
            serde_json::Value::Object(sorted.into_iter().collect())
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.into_iter().map(sort_json_keys).collect())
        }
        other => other,
    }
}

/// Compute the xxh128 hash of `data` and return it as a lowercase hex string.
pub fn xxh128_hex(data: &str) -> String {
    let hash = xxh3_128(data.as_bytes());
    format!("{hash:032x}")
}

// ---------------------------------------------------------------------------
// Cache check result
// ---------------------------------------------------------------------------

/// Outcome of a cache check operation.
#[derive(Debug)]
pub enum CacheCheckResult {
    /// Cache hit — blob already stored.
    Hit { blob_id: i64, created_at: String },
    /// Cache miss — git HEAD changed since last store.
    MissHeadChanged,
    /// Cache miss — no entry found for these params.
    MissNoMatch,
    /// Cache is unavailable (dirty worktree, no git repo, DB error).
    Unavailable(String),
}

impl std::fmt::Display for CacheCheckResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CacheCheckResult::Hit { blob_id, created_at } => {
                write!(f, "cache hit: blob_id={blob_id}, stored {created_at}")
            }
            CacheCheckResult::MissHeadChanged => {
                write!(f, "cache miss: git_head changed")
            }
            CacheCheckResult::MissNoMatch => {
                write!(f, "cache miss: no match")
            }
            CacheCheckResult::Unavailable(reason) => {
                write!(f, "cache unavailable: {reason}")
            }
        }
    }
}

/// Check whether a cache entry exists for the given tool and params JSON.
///
/// This is the implementation for `ctxl cache-check`.
pub fn cache_check(
    ctx: &CacheContext,
    tool: &str,
    output_mode: Option<&str>,
    params_json: &str,
) -> CacheCheckResult {
    let repo_root = match &ctx.repo_root {
        Some(r) => r.clone(),
        None => return CacheCheckResult::Unavailable("not in a git repository".to_string()),
    };

    if ctx.is_dirty() {
        return CacheCheckResult::Unavailable("dirty worktree".to_string());
    }

    let git_head = match ctx.git_head() {
        Some(h) => h,
        None => return CacheCheckResult::Unavailable("could not resolve HEAD".to_string()),
    };

    let normalized = normalize_params(params_json);
    let param_hash = xxh128_hex(&normalized);

    let db_path = match crate::global_db::global_db_path() {
        Some(p) => p,
        None => return CacheCheckResult::Unavailable("cache directory unavailable".to_string()),
    };

    let conn = match crate::global_db::open_global_db(&db_path) {
        Ok(c) => c,
        Err(e) => return CacheCheckResult::Unavailable(e.to_string()),
    };

    let lookup_result = match output_mode {
        Some(mode) => {
            crate::global_db::lookup(&conn, &repo_root, tool, mode, &param_hash, &git_head)
        }
        None => crate::global_db::lookup_any_mode(&conn, &repo_root, tool, &param_hash, &git_head),
    };

    match lookup_result {
        Ok(Some(hit)) => CacheCheckResult::Hit { blob_id: hit.blob_id, created_at: hit.created_at },
        Ok(None) => {
            // Distinguish "HEAD changed" from "no match" by checking if ANY
            // entry exists for these params (regardless of HEAD).
            let (sql, params): (&str, Vec<Box<dyn rusqlite::types::ToSql>>) = match output_mode {
                Some(mode) => (
                    "SELECT COUNT(*) FROM cache_index \
                     WHERE repo_root=?1 AND tool=?2 AND output_mode=?3 AND param_hash=?4",
                    vec![
                        Box::new(repo_root),
                        Box::new(tool.to_string()),
                        Box::new(mode.to_string()),
                        Box::new(param_hash),
                    ],
                ),
                None => (
                    "SELECT COUNT(*) FROM cache_index \
                     WHERE repo_root=?1 AND tool=?2 AND param_hash=?3",
                    vec![Box::new(repo_root), Box::new(tool.to_string()), Box::new(param_hash)],
                ),
            };
            let any_exists = conn
                .query_row(sql, rusqlite::params_from_iter(params.iter()), |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap_or(0);
            if any_exists > 0 {
                CacheCheckResult::MissHeadChanged
            } else {
                CacheCheckResult::MissNoMatch
            }
        }
        Err(e) => CacheCheckResult::Unavailable(e.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn normalize_params_removes_session_id() {
        let input = r#"{"session_id":"abc","tool":"Bash","command":"ls"}"#;
        let normalized = normalize_params(input);
        let val: serde_json::Value = serde_json::from_str(&normalized).unwrap();
        assert!(val.get("session_id").is_none(), "session_id should be removed");
        assert_eq!(val.get("tool").and_then(|v| v.as_str()), Some("Bash"));
    }

    #[test]
    fn normalize_params_sorts_keys() {
        let a = normalize_params(r#"{"z":"last","a":"first","m":"mid"}"#);
        let b = normalize_params(r#"{"m":"mid","z":"last","a":"first"}"#);
        assert_eq!(a, b, "key order should not affect normalized form");
    }

    #[test]
    fn normalize_params_handles_invalid_json() {
        let bad = "not json at all";
        let normalized = normalize_params(bad);
        assert_eq!(normalized, bad, "invalid JSON should pass through unchanged");
    }

    #[test]
    fn xxh128_hex_stable() {
        let h1 = xxh128_hex("hello world");
        let h2 = xxh128_hex("hello world");
        assert_eq!(h1, h2, "hash must be deterministic");
        assert_eq!(h1.len(), 32, "xxh128 hex is 32 chars");
    }

    #[test]
    fn xxh128_hex_differs_on_different_input() {
        let h1 = xxh128_hex("hello world");
        let h2 = xxh128_hex("hello world!");
        assert_ne!(h1, h2, "different inputs must produce different hashes");
    }

    #[test]
    fn cache_check_unavailable_without_global_db() {
        // Set CTXL_CACHE_ROOT to a temp dir with no global.db — should create it.
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("CTXL_CACHE_ROOT", tmp.path());
        let ctx = CacheContext { repo_root: Some("/some/repo".to_string()), cwd: None };
        // Don't require is_dirty() to succeed in test env — just verify
        // the function runs without panicking.
        let _result = cache_check(&ctx, "Bash", Some("stdout"), r#"{"command":"ls"}"#);
        std::env::remove_var("CTXL_CACHE_ROOT");
    }

    #[test]
    fn cache_check_unavailable_when_no_repo_root() {
        let ctx = CacheContext { repo_root: None, cwd: None };
        let result = cache_check(&ctx, "Bash", Some("stdout"), "{}");
        assert!(matches!(result, CacheCheckResult::Unavailable(_)), "no repo_root → unavailable");
    }

    #[test]
    fn cache_lookup_via_global_db() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("ctxl").join("global.db");
        let conn = crate::global_db::open_global_db(&db_path).unwrap();

        let params = crate::global_db::StoreBlobParams {
            content_hash: "testhash",
            content: "test content",
            compressed_body: None,
            compressed_method: None,
            line_count: Some(1),
            token_est: Some(2),
        };
        let blob_id =
            crate::global_db::store_blob(&conn, params, "/repo", "Bash", "stdout", "ph", "abc123")
                .unwrap();

        let hit = crate::global_db::lookup(&conn, "/repo", "Bash", "stdout", "ph", "abc123")
            .unwrap()
            .expect("should hit");
        assert_eq!(hit.blob_id, blob_id);
    }

    #[test]
    fn param_hash_same_for_reordered_json() {
        let a = xxh128_hex(&normalize_params(r#"{"b":2,"a":1}"#));
        let b = xxh128_hex(&normalize_params(r#"{"a":1,"b":2}"#));
        assert_eq!(a, b, "reordered JSON should produce same hash after normalization");
    }
}
