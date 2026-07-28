# ctxl

Context Ledger for Claude Code. Intercepts large tool output, stores it in per-session SQLite, and returns compact handles for bounded retrieval.

```
$ grep -rn "async fn" src/         # 2,400 lines of output

[ctxl] Output captured (2400 lines, 89201 bytes) → b_a3f7c2
Run: ctxl show b_a3f7c2  ·  ctxl search b_a3f7c2 <query>
```

Without ctxl, that output floods the context window. With ctxl, the agent gets a handle and retrieves only what it needs.

## Why

Claude Code tools like Bash and Grep can return thousands of lines. Every line consumes context tokens — even when the agent only needs a few. ctxl sits between the tool and the agent:

1. **Intercept** — PostToolUse hooks capture output exceeding thresholds
2. **Store** — content goes into a per-session SQLite database with FTS5 indexing
3. **Replace** — the agent sees a short handle instead of the full output
4. **Retrieve** — the agent runs `ctxl show` or `ctxl search` to get exactly what it needs

Output below threshold passes through unchanged. The agent never sees a difference in behavior. All errors fail-open — ctxl never blocks the agent.

## What gets intercepted

| Tool | Threshold | Handle prefix |
|---|---|---|
| Bash | >200 lines OR >8,192 bytes | `b_` |
| Grep | >200 lines | `g_` |
| Read, Edit, Write, Glob | Never (recorded for audit only) | — |

Running `grep` via the Bash tool produces `b_` handles (Bash intercepted it). The `g_` prefix only appears from the native Grep tool. Prefixes don't affect retrieval — all commands work on any handle.

## Installation

### Prerequisites

- Rust toolchain (`cargo`) — the binary is built from source on first session start

### Setup

```bash
# 1. Copy into your project
cp -r ctxl/ your-project/.claude/plugins/ctxl/

# 2. Enable the plugin — add to .claude/settings.json
#    "enabledPlugins": { "ctxl@local-plugins": true }

# 3. Add to .gitignore
echo '.claude/plugins/ctxl/cache/' >> .gitignore
echo '.claude/plugins/ctxl/target/' >> .gitignore
```

That's it. The SessionStart hook builds the binary, creates the session database, and installs the agent reference into `.claude/rules/` — all on first run.

### Worktree users

If `.claude/` is tracked in git, worktrees get the plugin source but not the compiled binary (`target/` is gitignored). Avoid a rebuild per worktree by symlinking:

```json
{
  "worktree": { "symlinkDirectories": [".claude/plugins/ctxl/target"] }
}
```

### Optional

Hide the audit skill from Claude's auto-invoke list:

```json
{
  "skillOverrides": { "ctxl:audit": "name-only" }
}
```

## Retrieval

Once a handle exists, retrieve content with the `ctxl` CLI (available on `PATH` after the first session):

```bash
ctxl show b_a3f7c2                    # first 80 lines
ctxl show b_a3f7c2 --head 200         # first 200 lines
ctxl show b_a3f7c2 --tail 20          # last 20 lines
ctxl show b_a3f7c2 --offset 100 --head 50   # lines 101-150
ctxl search b_a3f7c2 "async fn"       # FTS5 search within handle
ctxl search --all "TODO"              # search all handles in session
ctxl files b_a3f7c2                   # file manifest with match counts
ctxl show b_a3f7c2 --compressed       # structural skeleton (see below)
```

Truncated output includes a footer so the agent knows the full extent:

```
line 78
line 79
line 80
(showing 80 of 2400 lines)
```

### Skeleton compression

`--compressed` uses tree-sitter to extract a structural skeleton: imports, type declarations, and function signatures are preserved; function bodies are replaced with `// ... N lines` placeholders.

```
use std::collections::HashMap;

struct Config {
    limit: usize,
    factor: f64,
}

fn process(data: &[u8], config: &Config) -> Result<Output> { // ... 47 lines
}

fn validate(input: &str) -> bool { // ... 12 lines
}
```

Supported languages: Rust, TypeScript, TSX, JavaScript, JSX, Python, Go, Java, C, C++, Ruby, Bash, CSS, JSON. Language grammars are compiled via feature flags — all are included when built with `--features all-languages` (the default).

Files without a supported grammar fall back to a head/tail preview.

## Piping arbitrary content

`ctxl index` reads from stdin, stores it, and returns a handle:

```bash
curl -s https://api.example.com/data | ctxl index --hint "api response"
# [ctxl] Indexed (500 lines, 23401 bytes) → i_b29e4f
```

The `--hint` helps with later retrieval context. Index handles use the `i_` prefix.

## Session introspection

```bash
ctxl calls                    # call history for current session
ctxl calls --last 5           # last 5 calls
ctxl calls --intercepted      # only intercepted calls
ctxl last                     # most recent call
ctxl inspect b_a3f7c2         # handle metadata (size, tool, timestamp)
ctxl doctor                   # diagnostic health check
```

## Configuration

Cache defaults to `.claude/plugins/ctxl/cache/` (project-local). Override at install time:

```bash
claude plugin install ctxl --config cacheDir=/custom/path
```

| Environment variable | Effect |
|---|---|
| `CTXL_ENABLED=0` | Disable interception entirely |
| `CTXL_RECORD=0` | Disable call recording (interception still runs) |
| `CTXL_DEBUG=1` | Enable debug logging |

## Architecture

```
hooks/
  ctxl-init.sh       SessionStart — build binary, create session DB, export env
  ctxl-hook.sh       PostToolUse  — route to intercept handler by tool name
src/
  main.rs            CLI entry + subcommand routing
  intercept.rs       Bash handler (byte + line threshold)
  intercept_grep.rs  Grep handler (line threshold)
  record.rs          Read/Edit/Write/Glob audit recording
  store.rs           Handle storage (raw + compressed)
  retrieve.rs        show / search / files / inspect
  db.rs              Per-session SQLite + migrations
  global_db.rs       Cross-session content-addressed blob store
  compress/
    code.rs          tree-sitter skeleton extraction
    diff.rs          Entity-level diff attribution
    grep_dedup.rs    SimHash dedup for grep output
    json.rs          JSON structure flattening
    passthrough.rs   Identity (no compression)
    ansi.rs          ANSI escape stripping
rules/
  ctxl-handles.md    Agent reference (auto-installed to .claude/rules/)
```

## Design principles

- **Fail-open** — every error exits 0. ctxl never blocks the agent.
- **Session isolation** — one SQLite DB per session. No cross-session interference.
- **Threshold, not heuristic** — deterministic interception based on byte count and line count, not content analysis.
- **Content-addressed dedup** — identical outputs across sessions share storage via `global.db`.

## Development

```bash
cargo check --tests                    # type check (includes #[cfg(test)])
cargo test                             # unit + integration tests (no grammars)
cargo test --features all-languages    # full suite with tree-sitter grammars
cargo clippy                           # lint
cargo fmt --check                      # format check
```

## License

Apache 2.0 — see [LICENSE](LICENSE).
