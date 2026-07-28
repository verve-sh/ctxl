# ctxl

A safety net to prevent verbose tool output from flooding the context window.

When Bash or Grep output exceeds a size threshold, ctxl intercepts it, stores it in a per-session SQLite database, and replaces it with a two-line handle. The agent retrieves only what it needs via search or slice. Small outputs pass through unchanged.

```
$ grep -rn "async fn" src/         # 2,400 lines of output

[ctxl] Output captured (2400 lines, 89201 bytes) → b_a3f7c2
Run: ctxl show b_a3f7c2  ·  ctxl search b_a3f7c2 <query>
```

2,400 lines replaced by two. The agent searches the handle, pulls 10 matching lines, and moves on.

**124 sessions in a mid-size Rust/TypeScript monorepo:**

| Metric | Value |
|---|---|
| Bash/Grep calls recorded | 1,824 |
| Calls intercepted | 438 (24%) |
| Total output tokens | 4.0M |
| Tokens entering context | 1.5M |
| Net tokens saved | 2.5M (63%) |

17% of intercepted outputs were never retrieved — the agent didn't need the content at all.

## How it works

1. **Intercept** — PostToolUse hooks fire on Bash and Grep calls. Bash: >200 lines or >8 KB. Grep: >200 lines.
2. **Store** — content goes into a per-session SQLite database with FTS5 full-text indexing
3. **Replace** — the original output is swapped for a short handle (`b_a3f7c2`)
4. **Retrieve** — `ctxl show`, `ctxl search`, or `ctxl files` pulls exactly what's needed

Output below threshold passes through unchanged. All errors fail-open — ctxl never blocks the agent.

## What gets intercepted

| Tool | Threshold | Handle prefix |
|---|---|---|
| Bash | >200 lines OR >8,192 bytes | `b_` |
| Grep | >200 lines | `g_` |
| Read, Edit, Write, Glob | Never (recorded for audit only) | — |

Running `grep` via Bash produces a `b_` handle, not `g_`. Prefixes don't affect retrieval — all commands work on any handle.

## Install

**Requires:** Rust toolchain (`cargo`). The binary builds from source on first session start.

1. Copy into your project:

```bash
cp -r ctxl/ your-project/.claude/plugins/ctxl/
```

2. Install the agent rule:

```bash
mkdir -p your-project/.claude/rules
cp your-project/.claude/plugins/ctxl/rules/ctxl-handles.md your-project/.claude/rules/
```

3. Enable in `.claude/settings.json`:

```json
{ "enabledPlugins": { "ctxl@local-plugins": true } }
```

4. Gitignore build artifacts:

```bash
echo '.claude/plugins/ctxl/cache/' >> .gitignore
echo '.claude/plugins/ctxl/target/' >> .gitignore
```

The SessionStart hook handles the rest: builds the binary and creates the session database.

### Worktree users

Avoid a rebuild per worktree by symlinking the build directory:

```json
{ "worktree": { "symlinkDirectories": [".claude/plugins/ctxl/target"] } }
```

### Optional

Hide the audit skill from Claude's auto-invoke list:

```json
{ "skillOverrides": { "ctxl:audit": "name-only" } }
```

## Retrieval

```bash
ctxl show b_a3f7c2                         # first 80 lines
ctxl show b_a3f7c2 --head 200              # first 200 lines
ctxl show b_a3f7c2 --tail 20               # last 20 lines
ctxl show b_a3f7c2 --offset 100 --head 50  # lines 101–150
ctxl search b_a3f7c2 "async fn"            # FTS5 search within handle
ctxl search --all "TODO"                   # search all handles in session
ctxl files b_a3f7c2                        # file manifest with match counts
```

Truncated output includes a footer so the agent knows the full extent:

```
line 80
(showing 80 of 2400 lines)
```

### Skeleton compression

`ctxl show <handle> --compressed` uses tree-sitter to extract a structural skeleton — imports, type declarations, and function signatures are preserved; bodies become `// ... N lines` placeholders.

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

Supported: Rust, TypeScript, TSX, JavaScript, JSX, Python, Go, Java, C, C++, Ruby, Bash, CSS, JSON (14 grammars). Files without a supported grammar fall back to a head/tail preview.

## Piping arbitrary content

`ctxl index` reads from stdin:

```bash
curl -s https://api.example.com/data | ctxl index --hint "api response"
# [ctxl] Indexed (500 lines, 23401 bytes) → i_b29e4f
```

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

| Variable | Effect |
|---|---|
| `CTXL_ENABLED=0` | Disable interception entirely |
| `CTXL_RECORD=0` | Disable call recording (interception still runs) |
| `CTXL_DEBUG=1` | Enable debug logging |

Cache defaults to `.claude/plugins/ctxl/cache/` (project-local). To override, set `cacheDir` in `.claude/plugins/ctxl/.claude-plugin/plugin.json`.

## Architecture

```
hooks/
  ctxl-init.sh       SessionStart — build binary, create session DB, export env
  ctxl-hook.sh       PostToolUse  — route to intercept handler by tool name
src/
  main.rs            CLI entry + subcommand routing
  intercept.rs       Bash handler (byte + line threshold)
  intercept_grep.rs  Grep handler (line threshold)
  index.rs           stdin pipe (ctxl index)
  record.rs          Read/Edit/Write/Glob audit recording
  store.rs           Handle storage (raw + compressed)
  retrieve.rs        show / search / files / inspect
  db.rs              Per-session SQLite + migrations
  global_db.rs       Cross-session content-addressed blob store
  payload.rs         Generic PostToolUsePayload<T> envelope
  calls.rs           Call history queries
  doctor.rs          Diagnostic health checks
  compress/          Compression strategies (tree-sitter, SimHash, JSON, diff, ANSI)
rules/
  ctxl-handles.md    Agent reference (copy to .claude/rules/ at install)
```

## Design principles

- **Fail-open** — every error exits 0. ctxl never blocks the agent.
- **Session isolation** — one SQLite DB per session. No cross-session interference.
- **Threshold, not heuristic** — deterministic interception based on byte count and line count.
- **Content-addressed dedup** — identical outputs across sessions share storage via `global.db`.

## Development

```bash
cargo check --tests                    # type check
cargo test                             # unit + integration tests
cargo test --features all-languages    # full suite with tree-sitter grammars
cargo clippy                           # lint
cargo fmt --check                      # format check
```

## License

Apache 2.0 — see [LICENSE](LICENSE).
