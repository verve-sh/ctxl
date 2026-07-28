#!/usr/bin/env bash
# ctxl-init.sh — SessionStart hook: provision ctxl session and export CTXL_SESSION_ID.
#
# Fail-open: any error exits 0. Never block session start.
set -euo pipefail
trap 'exit 0' ERR

# ── Parse session_id from stdin JSON ────────────────────────────────────────
INPUT=$(cat 2>/dev/null) || exit 0
SESSION_ID=$(echo "$INPUT" | grep -o '"session_id":"[^"]*"' 2>/dev/null | head -1 | cut -d'"' -f4) || exit 0
[ -z "$SESSION_ID" ] && exit 0

# ── Session ID validation ──────────────────────────────────────────────────
[[ "$SESSION_ID" =~ ^[a-zA-Z0-9_-]+$ ]] || exit 0

# ── Resolve ctxl binary ────────────────────────────────────────────────────
PLUGIN_ROOT="${CLAUDE_PLUGIN_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"
CTXL_BIN=""
if [ -x "$PLUGIN_ROOT/target/release/ctxl" ]; then
  CTXL_BIN="$PLUGIN_ROOT/target/release/ctxl"
elif [ -x "$PLUGIN_ROOT/target/debug/ctxl" ]; then
  CTXL_BIN="$PLUGIN_ROOT/target/debug/ctxl"
fi
if [ -z "$CTXL_BIN" ]; then
  if [ -d "$PLUGIN_ROOT/src" ]; then
    echo "[ctxl] First run — building binary (this may take a moment)..." >&2
    if (cd "$PLUGIN_ROOT" && cargo build --release --features all-languages 2>/dev/null); then
      if [ -x "$PLUGIN_ROOT/target/release/ctxl" ]; then
        CTXL_BIN="$PLUGIN_ROOT/target/release/ctxl"
        echo "[ctxl] Build complete." >&2
      fi
    fi
  fi
  [ -z "$CTXL_BIN" ] && exit 0
fi

# ── Staleness check — background rebuild if source is newer than binary ───
CTXL_SRC="$PLUGIN_ROOT/src"
if [ -d "$CTXL_SRC" ] && [ -n "$(find "$CTXL_SRC" -name '*.rs' -newer "$CTXL_BIN" -print -quit 2>/dev/null)" ]; then
  (cd "$PLUGIN_ROOT" && cargo build --release --features all-languages 2>/dev/null) &
  echo "[ctxl] source newer than binary — rebuilding in background" >&2
fi

# ── Debug / norecord sentinel files ───────────────────────────────────────
if [ "${CTXL_DEBUG:-}" = "1" ]; then
  (umask 077; printf '1\n' > "${TMPDIR:-/tmp}/ctxl-debug-${SESSION_ID}") 2>/dev/null || true
fi

if [ "${CTXL_RECORD:-1}" = "0" ]; then
  (umask 077; printf '1\n' > "${TMPDIR:-/tmp}/ctxl-norecord-${SESSION_ID}") 2>/dev/null || true
fi

# ── Resolve cache root (project-local by default) ────────────────────────
PLUGIN_JSON="$PLUGIN_ROOT/.claude-plugin/plugin.json"
CACHE_DIR=""
if [ -f "$PLUGIN_JSON" ] && command -v jq >/dev/null 2>&1; then
  CACHE_DIR=$(jq -r '.config.cacheDir // empty' "$PLUGIN_JSON" 2>/dev/null) || true
fi
if [ -z "$CACHE_DIR" ]; then
  CACHE_DIR="$PLUGIN_ROOT/cache"
fi
mkdir -p "$CACHE_DIR/ctxl" 2>/dev/null || true
export CTXL_CACHE_ROOT="$CACHE_DIR"

# ── Init session DB via Rust ──────────────────────────────────────────────
"$CTXL_BIN" hook session-start --session-id "$SESSION_ID" 2>/dev/null || exit 0

# ── Export env vars (MUST stay in shell — Rust can't modify parent env) ──
if [ -n "${CLAUDE_ENV_FILE:-}" ]; then
  echo "export CTXL_SESSION_ID=\"$SESSION_ID\"" >> "$CLAUDE_ENV_FILE"
  echo "export CTXL_BIN=\"$CTXL_BIN\"" >> "$CLAUDE_ENV_FILE"
  echo "export CTXL_CACHE_ROOT=\"$CACHE_DIR\"" >> "$CLAUDE_ENV_FILE"
  CTXL_DIR="$(dirname "$CTXL_BIN")"
  echo "export PATH=\"$CTXL_DIR:\$PATH\"" >> "$CLAUDE_ENV_FILE"
fi

exit 0
