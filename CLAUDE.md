# ctxl — Context Ledger

Standalone Rust CLI that intercepts large tool output via PostToolUse hooks, stores in per-session SQLite, returns handles for bounded retrieval. No Tauri dependency.

## Commands

```bash
cd .claude/plugins/ctxl && cargo check --tests   # type check
cd .claude/plugins/ctxl && cargo test             # full suite
cd .claude/plugins/ctxl && cargo clippy           # lint
cd .claude/plugins/ctxl && cargo fmt --check      # format
```

## Architecture

```
src/
├── main.rs             — CLI + router (intercept subcommand dispatch)
├── index.rs            — `ctxl index` stdin pipe: detect, convert, store, hint
├── intercept.rs        — Bash handler
├── intercept_grep.rs   — Grep handler
├── record.rs           — recording tier (Read/Edit/Write/Glob)
├── payload.rs          — generic PostToolUsePayload<T> envelope
├── store.rs            — handle storage (raw + compressed)
├── retrieve.rs         — CLI retrieval (show/search/files/inspect)
├── doctor.rs           — diagnostic checks (DB, hooks, session health)
├── db.rs               — SQLite session DB + global.db, migrations
├── calls.rs            — call history queries (calls/last/cache-check)
├── compress/           — compression strategies
│   ├── code.rs         — tree-sitter skeleton extraction
│   ├── diff.rs         — entity-level diff attribution
│   ├── grep_dedup.rs   — SimHash dedup
│   ├── grep_preview.rs — mode-aware preview
│   ├── json.rs         — JSON flattening
│   ├── passthrough.rs  — identity (no compression)
│   └── ansi.rs         — ANSI escape stripping
├── clean.rs            — session TTL cleanup
├── debug.rs            — debug utilities
└── error.rs            — CtxlError enum
```

## Key Design Decisions

- **Generic envelope:** `PostToolUsePayload<T>` in `payload.rs` is the single deserialization struct. Router parses once, dispatches typed payloads.
- **Fail-open everywhere:** Deserialization errors, DB errors, handler panics — all log to stderr and exit 0. Never block the agent.
- **Session isolation:** SQLite DB per `session_id` at `{CTXL_CACHE_ROOT}/ctxl/{SESSION_ID}/store.db`. Cache root defaults to plugin-local `cache/` (configurable via `plugin.json`).
- **Handle prefixes:** `b_` = Bash, `g_` = Grep, `i_` = ctxl index.
- **record.rs is separate:** Different payload shape. Not behind the generic envelope.
- **Guard bypass:** PreToolUse guards allow heavy-output commands through (gh read ops, curl/wget, `grep -r`/`rg`) — ctxl intercepts large output post-execution. Disable interception: `CTXL_ENABLED=0` or `.claude/ctxl.disabled` file.

## Gotchas

- `cargo check` skips `#[cfg(test)]`. Use `cargo check --tests`.
- Serde renames on response structs: `isImage`, `noOutputExpected`, `exitCode` (Bash), `numFiles`, `numMatches` (Grep) use `#[serde(rename)]`.
- Handler `run()` takes owned payload — `PostToolUsePayload<T>` is consumed by move.
- `record.rs` is NOT behind the generic envelope — has its own inline deserialization.
- Non-zero exit codes are surfaced in handle messages as "Command failed (exit N)" with a `ctxl search <handle> "error"` suggestion. The `updatedToolOutput` schema cannot propagate exit codes separately — they are embedded in the stdout text.
