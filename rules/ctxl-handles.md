# ctxl Context Ledger

Large tool outputs are automatically captured and replaced with compact handles.

## Recognizing Handles

When a tool produces large output, you'll see:

```
[ctxl] Output captured (892 lines, 30000 bytes) → b_69e7a5
Run: ctxl show b_69e7a5
```

The output is stored — not lost. The handle (`b_69e7a5`) is your key to retrieve it.

### Handle Prefixes

| Prefix | Tool | Threshold |
|---|---|---|
| `b_` | Bash | >8192 bytes (after ANSI stripping) OR >200 lines |
| `g_` | Grep (native tool) | >200 lines |
| `i_` | `ctxl index` (stdin pipe) | Always stored |

**Prefix determines the intercepting tool, not the content.** Running `grep`/`rg` via Bash produces `b_` handles — the Bash tool intercepted it. The `g_` prefix only appears from the native Grep tool. **The prefix doesn't matter for retrieval** — all commands (`show`, `search`, `files`, `--file`, `--glob`) work identically on `b_`, `g_`, and `i_` handles. When Bash-intercepted output is grep-like, the summary line will include a file count and suggest `ctxl files` automatically.

## Retrieval Commands

| Command | Use | Example |
|---|---|---|
| `ctxl show <handle>` | First 80 lines of captured output | `ctxl show b_69e7a5` |
| `ctxl show <handle> --head 200` | More lines from the start | `ctxl show b_69e7a5 --head 200` |
| `ctxl show <handle> --tail 20` | Last N lines | `ctxl show b_69e7a5 --tail 20` |
| `ctxl show <handle> --offset 100 --head 50` | Windowed slice (lines 100–149) | `ctxl show b_69e7a5 --offset 100 --head 50` |
| `ctxl search <handle> <query>` | FTS5 search within captured output | `ctxl search b_69e7a5 "fn main"` |
| `ctxl search --all <query>` | FTS5 search across all session handles | `ctxl search --all "Connection API"` |
| `ctxl search --global <query>` | FTS5 search across all sessions in this project (global.db) | `ctxl search --global "AppError"` |
| `ctxl search --global --repo <path>` | Scope global search to a specific repo root | `ctxl search --global --repo /path/to/repo "query"` |
| `ctxl search --limit <n>` | Cap result count (default 20) | `ctxl search b_69e7a5 "error" --limit 5` |
| `ctxl files <handle>` | File manifest with match counts | `ctxl files b_69e7a5` |
| `ctxl inspect <handle>` | Metadata without content (JSON or `--human`) | `ctxl inspect b_69e7a5 --human` |
| `ctxl index [--hint <q>]` | Pipe stdin → store + FTS5 index | `curl -sL <url> \| ctxl index --hint "API"` |
| `ctxl index --content-type <type>` | Override auto-detection: `html`, `json`, `text` | `echo '{}' \| ctxl index --content-type json` |
| `ctxl index --source <url>` | Store provenance URL as metadata (no fetch) | `curl -sL <url> \| ctxl index --source <url>` |
| `ctxl show <handle> --file <path>` | Matches from a specific file path (exact or suffix) | `ctxl show b_69e7a5 --file src/lib.rs` |
| `ctxl show <handle> --glob "*.rs"` | Matches from files matching glob | `ctxl show b_69e7a5 --glob "*.rs"` |
| `ctxl show <handle> --exclude "*.test.*"` | Exclude files by glob (repeatable) | `ctxl show b_69e7a5 --exclude "*.test.*"` |
| `ctxl show <handle> --compressed` | Structural/compressed view (skeleton, entity attribution) | `ctxl show b_69e7a5 --compressed` |
| `ctxl show <handle> --raw` | Explicit raw content (default; clarity when compressed is available) | `ctxl show b_69e7a5 --raw` |

**Argument order:** `ctxl search` accepts positional `<handle> <query>` (handle-first) or named flags `--handle <id> --query <text>` (order-independent). Reversed positionals produce an error with hint.

**FTS5 tokenization:** `ctxl search` uses FTS5 phrase matching. Underscore-joined identifiers like `grep_dedup` are single tokens — searching for `"grep"` alone won't match. Use `"grep*"` for prefix matching, or `"grep_dedup"` for the exact token. The CLI suggests this when search returns zero results.

## Session Introspection

| Command | Use | Example |
|---|---|---|
| `ctxl calls` | Full call history for the session | `ctxl calls` |
| `ctxl calls --last 5` | Last N calls | `ctxl calls --last 5` |
| `ctxl calls --tool Bash` | Calls filtered by tool name | `ctxl calls --tool Bash` |
| `ctxl calls --intercepted` | Only calls that produced handles | `ctxl calls --intercepted` |
| `ctxl last` | Shorthand for `calls --last 1` | `ctxl last` |
| `ctxl last --intercepted` | Most recent intercepted call | `ctxl last --intercepted` |
| `ctxl doctor` | Diagnostic report (DB status, hook chain, session health) | `ctxl doctor` |
| `ctxl doctor --json` | Machine-readable diagnostics | `ctxl doctor --json` |
| `ctxl cache-check` | Check global cache for a tool+params combination | `ctxl cache-check --tool Bash --params '{"command":"npm test"}'` |

## When to Retrieve

- **Need specific content** (function signature, error message, config value): use `ctxl search <handle> "<specific text>"` — targeted, low token cost
- **Need overview** (structure, file list, test summary): use `ctxl show <handle>` — first 80 lines usually enough
- **Don't need the content** (command confirmed success, output was incidental): skip retrieval — the handle saved you tokens

## File-Level Navigation

For any handle with file-structured content (grep output, test output, logs):

1. **Start with `ctxl files <handle>`** — see which files have matches and how many
2. **Narrow with `--file` or `--glob`** — `ctxl show <handle> --file src/main.rs` for one file, `ctxl show <handle> --glob "*.rs"` for a pattern. Both relative and absolute paths work for `--file`.
3. **Use `ctxl search` for FTS5 across all files** — `ctxl search <handle> "error"` when you need a specific term

## Decision Framework

The handle message tells you the size. Use that to decide:

| Situation | Action | Why |
|---|---|---|
| Build/test passed, just confirming | Don't retrieve | Success confirmation was in the message |
| Build failed, need error details | `ctxl search <handle> "error"` | Targeted extraction beats full dump |
| Exploring codebase, need file contents | `ctxl show <handle>` | Overview first, then search if needed |
| Need to reference specific lines later | `ctxl show <handle> --head N` | Get the section you need |
| Grep returned many files | `ctxl files <handle>` | See file manifest first, then narrow |

## What Gets Captured

- Bash outputs >8192 bytes (after ANSI stripping) OR >200 lines
- Grep outputs >200 lines
- `ctxl index` stdin input (always stored — explicit indexing, no threshold)
- The handle prefix indicates the tool: `b_` = Bash, `g_` = Grep, `i_` = ctxl index

## What Doesn't Get Captured

- Outputs below threshold — pass through normally
- Read/Edit/Write/Glob tool calls — recorded (for audit) but output passes through unchanged
- Image outputs — always pass through

## Configuration

- `CTXL_ENABLED=0` or `.claude/ctxl.disabled` file — disables interception entirely
- `CTXL_RECORD=0` — disables audit recording; interception still runs
- `CTXL_BIN` env var — set by session-start hook; used by guard bypass detection
- `CTXL_CACHE_ROOT` env var — set by session-start hook; points to project-local cache directory
- All errors are non-fatal — log to stderr and exit 0 (fail-open)

## Session Lifecycle

- **SessionStart hook** provisions the per-session SQLite DB under the project-local cache (`CTXL_CACHE_ROOT`) and runs background cleanup of stale sessions (>7d)
- **Global cache** (`global.db`) persists across sessions within this project for cross-session search (`--global`) and cache-check
- **Cleanup:** `ctxl clean` prunes sessions older than `--ttl` (default 7d); `--global` cleans global cache (30d)
