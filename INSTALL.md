# ctxl — Installation

## Prerequisites

- Rust toolchain (`cargo`) — binary is built from source on first session start

## Setup

1. Copy `.claude/plugins/ctxl/` into your project

2. Add to your project's `.claude/settings.json`:

### Required

    "enabledPlugins": { "ctxl@local-plugins": true }

3. Add to `.gitignore`:

    .claude/plugins/ctxl/cache/

4. **If `.claude/` is tracked in git** (`git ls-files .claude/ | head -1` returns output):

   Worktrees get the plugin directory from git checkout, but not the compiled
   binary (it's in `target/`, which is gitignored). Without the symlink below,
   each worktree triggers a ~30s background rebuild on first session.

   Add to `.claude/settings.json`:

       "worktree": { "symlinkDirectories": [".claude/plugins/ctxl/target"] }

### Recommended

    "skillOverrides": { "ctxl:audit": "name-only" }

## How It Works

The plugin's SessionStart hook (`ctxl-init.sh`) handles everything else:
- Finds or builds the ctxl binary
- Creates a per-session SQLite database
- Exports `CTXL_BIN`, `CTXL_SESSION_ID`, `CTXL_CACHE_ROOT` to the session
- Copies `rules/ctxl-handles.md` to `.claude/rules/ctxl-handles.md` on first run (agents need this always-loaded reference)

PostToolUse hooks intercept large Bash/Grep output and record Read/Edit/Write/Glob calls.

## Configuration

Cache defaults to `.claude/plugins/ctxl/cache/` (project-local).
Override at install time:

    claude plugin install ctxl --config cacheDir=/custom/path

Or set via `/plugin` → ctxl → Configure after install.

## Environment Variables (user-configurable)

- `CTXL_ENABLED=0` — disable interception entirely
- `CTXL_RECORD=0` — disable call recording
- `CTXL_DEBUG=1` — enable debug logging
