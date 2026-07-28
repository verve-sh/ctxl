#!/usr/bin/env bash
# ctxl-hook.sh — Universal PostToolUse hook: pipes stdin through ctxl.
#
# Self-sufficient: resolves the binary and cache root itself instead of
# depending on CTXL_BIN/CTXL_CACHE_ROOT from CLAUDE_ENV_FILE. Hook
# subprocesses do NOT inherit CLAUDE_ENV_FILE exports (only Bash tool calls
# do) — relying on them left interception silently dead. Env vars, when
# present, still take precedence. Session id comes from the JSON payload
# (the binary peeks it) when CTXL_SESSION_ID is unset.
#
# Fail-open: any error exits 0. Hook errors must never block agent execution.
set -euo pipefail
trap 'exit 0' ERR

# ── Kill switches ─────────────────────────────────────────────────────────
[ "${CTXL_ENABLED:-1}" = "0" ] && exit 0
# Anchor to the project dir (hooks run with CWD = project dir, but
# CLAUDE_PROJECT_DIR is explicit and survives any cwd manipulation).
[ -f "${CLAUDE_PROJECT_DIR:-.}/.claude/ctxl.disabled" ] && exit 0

# ── Resolve plugin root (env override, else script location) ─────────────
PLUGIN_ROOT="${CLAUDE_PLUGIN_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"

# ── Resolve binary: env → plugin-local build → main-checkout build ───────
BIN="${CTXL_BIN:-}"
if [ -z "$BIN" ] || [ ! -x "$BIN" ]; then
  if [ -x "$PLUGIN_ROOT/target/release/ctxl" ]; then
    BIN="$PLUGIN_ROOT/target/release/ctxl"
  elif [ -x "$PLUGIN_ROOT/target/debug/ctxl" ]; then
    BIN="$PLUGIN_ROOT/target/debug/ctxl"
  else
    # Worktree: target/ is untracked and absent — borrow the main
    # checkout's binary via the shared git common dir.
    COMMON_DIR=$(git -C "$PLUGIN_ROOT" rev-parse --path-format=absolute --git-common-dir 2>/dev/null) || COMMON_DIR=""
    if [ -n "$COMMON_DIR" ]; then
      MAIN_PLUGIN="$(dirname "$COMMON_DIR")/.claude/plugins/ctxl"
      if [ -x "$MAIN_PLUGIN/target/release/ctxl" ]; then
        BIN="$MAIN_PLUGIN/target/release/ctxl"
      elif [ -x "$MAIN_PLUGIN/target/debug/ctxl" ]; then
        BIN="$MAIN_PLUGIN/target/debug/ctxl"
      fi
    fi
  fi
fi
[ -z "$BIN" ] && exit 0
[ ! -x "$BIN" ] && exit 0

# ── Resolve cache root: env → plugin.json cacheDir → plugin-local cache ──
# Mirrors ctxl-init.sh so hook writes land in the same DB retrieval reads.
if [ -z "${CTXL_CACHE_ROOT:-}" ]; then
  CACHE_DIR=""
  PLUGIN_JSON="$PLUGIN_ROOT/.claude-plugin/plugin.json"
  if [ -f "$PLUGIN_JSON" ] && command -v jq >/dev/null 2>&1; then
    CACHE_DIR=$(jq -r '.config.cacheDir // empty' "$PLUGIN_JSON" 2>/dev/null) || true
  fi
  [ -z "$CACHE_DIR" ] && CACHE_DIR="$PLUGIN_ROOT/cache"
  export CTXL_CACHE_ROOT="$CACHE_DIR"
fi

# ── Validate session ID (reject path traversal) ─────────────────────────
SID="${CTXL_SESSION_ID:-}"
case "$SID" in *..* | */* ) exit 0 ;; esac

# ── Debug mode ────────────────────────────────────────────────────────────
[ -n "$SID" ] && [ -f "${TMPDIR:-/tmp}/ctxl-debug-${SID}" ] && export CTXL_DEBUG=1

# ── Resolve timeout command ──────────────────────────────────────────────
TIMEOUT_CMD=""
command -v timeout >/dev/null 2>&1 && TIMEOUT_CMD="timeout 5"
[ -z "$TIMEOUT_CMD" ] && command -v gtimeout >/dev/null 2>&1 && TIMEOUT_CMD="gtimeout 5"

# ── Direct invocation ───────────────────────────────────────────────────
$TIMEOUT_CMD "$BIN" hook post-tool-use 2>/dev/null || exit 0
