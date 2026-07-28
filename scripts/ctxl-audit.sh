#!/usr/bin/env bash
# ctxl-audit.sh — Post-session audit of ctxl value metrics.
#
# Usage: ctxl-audit.sh [session-id]
#   If session-id omitted, uses most recent session in cache dir.
#
# Outputs a structured report with:
#   1. Interception stats (count, bytes, estimated tokens saved)
#   2. Recording stats (non-intercepted tool calls)
#   3. Retrieval analysis (how many handles were accessed back)
#   4. Net value assessment
#
# Requires: sqlite3, jq (optional, for JSON output)
set -euo pipefail

CACHE_DIR="${CTXL_CACHE_ROOT:-}"
if [ -z "$CACHE_DIR" ]; then
  SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
  PLUGIN_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
  if [ -d "$PLUGIN_ROOT/cache/ctxl" ]; then
    CACHE_DIR="$PLUGIN_ROOT/cache"
  else
    case "$(uname -s)" in
      Darwin) CACHE_DIR="$HOME/Library/Caches" ;;
      *)      CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}" ;;
    esac
  fi
fi

# ── Resolve session ──────────────────────────────────────────────────
SESSION_ID="${1:-}"
if [ -z "$SESSION_ID" ]; then
  SESSION_ID="${CTXL_SESSION_ID:-}"
fi
if [ -z "$SESSION_ID" ]; then
  if [ "$(uname -s)" = "Darwin" ]; then
    SESSION_ID=$(find "$CACHE_DIR" -maxdepth 1 -mindepth 1 -type d \
      -exec stat -f '%m %N' {} \; 2>/dev/null \
      | sort -rn | head -1 | sed 's|^[0-9]* ||' | xargs basename)
  else
    SESSION_ID=$(find "$CACHE_DIR" -maxdepth 1 -mindepth 1 -type d \
      -exec stat -c '%Y %n' {} \; 2>/dev/null \
      | sort -rn | head -1 | sed 's|^[0-9]* ||' | xargs basename)
  fi
  if [ -z "$SESSION_ID" ]; then
    echo "ERROR: No ctxl sessions found in $CACHE_DIR" >&2
    exit 1
  fi
fi

DB="$CACHE_DIR/ctxl/$SESSION_ID/store.db"
if [ ! -f "$DB" ]; then
  echo "ERROR: Database not found: $DB" >&2
  exit 1
fi

echo "═══════════════════════════════════════════════════════════════"
echo "  ctxl Session Audit Report"
echo "  Session: $SESSION_ID"
echo "  Database: $DB"
echo "═══════════════════════════════════════════════════════════════"
echo ""

# ── 1. Interception Stats ────────────────────────────────────────────
echo "── 1. Interception Stats ──────────────────────────────────────"

HANDLE_COUNT=$(sqlite3 "$DB" "SELECT COUNT(*) FROM handles;")
TOTAL_BYTES=$(sqlite3 "$DB" "SELECT COALESCE(SUM(LENGTH(content)), 0) FROM handles;")
TOTAL_LINES=$(sqlite3 "$DB" "SELECT COALESCE(SUM(line_count), 0) FROM handles;")
TOTAL_TOKEN_EST=$(sqlite3 "$DB" "SELECT COALESCE(SUM(token_est), 0) FROM handles;")

echo "  Handles created:     $HANDLE_COUNT"
echo "  Total bytes stored:  $TOTAL_BYTES"
echo "  Total lines stored:  $TOTAL_LINES"
echo "  Est. tokens saved:   $TOTAL_TOKEN_EST"
echo ""

if [ "$HANDLE_COUNT" -gt 0 ]; then
  echo "  Per-handle breakdown:"
  sqlite3 -column -header "$DB" "
    SELECT
      id AS handle,
      tool,
      line_count AS lines,
      LENGTH(content) AS bytes,
      token_est AS est_tokens,
      datetime(created_at, 'unixepoch', 'localtime') AS created
    FROM handles
    ORDER BY created_at;
  "
  echo ""
fi

# ── 2. Recording Stats ──────────────────────────────────────────────
echo "── 2. Recording Stats (calls table) ─────────────────────────"

CALL_COUNT=$(sqlite3 "$DB" "SELECT COUNT(*) FROM calls;")
INTERCEPTED_COUNT=$(sqlite3 "$DB" "SELECT COUNT(*) FROM calls WHERE intercepted = 1;")
PASSTHROUGH_COUNT=$(sqlite3 "$DB" "SELECT COUNT(*) FROM calls WHERE intercepted = 0;")

echo "  Total recorded calls:  $CALL_COUNT"
echo "  Intercepted:           $INTERCEPTED_COUNT"
echo "  Pass-through:          $PASSTHROUGH_COUNT"
echo ""

if [ "$CALL_COUNT" -gt 0 ]; then
  echo "  By tool:"
  sqlite3 -column -header "$DB" "
    SELECT
      tool,
      COUNT(*) AS count,
      SUM(CASE WHEN intercepted THEN 1 ELSE 0 END) AS intercepted,
      COALESCE(SUM(token_est), 0) AS total_token_est
    FROM calls
    GROUP BY tool
    ORDER BY count DESC;
  "
  echo ""
fi

# ── 3. Retrieval Analysis ───────────────────────────────────────────
echo "── 3. Retrieval Analysis ────────────────────────────────────"

# Also check handles table — if compressed_body is populated, compression was applied
COMPRESSED_COUNT=$(sqlite3 "$DB" "
  SELECT COUNT(*) FROM handles WHERE compressed_body IS NOT NULL;
")

RETRIEVAL_COUNT=$(sqlite3 "$DB" "
  SELECT COUNT(*) FROM calls WHERE tool LIKE 'ctxl-%';
")
RETRIEVAL_TOKENS=$(sqlite3 "$DB" "
  SELECT COALESCE(SUM(token_est), 0) FROM calls WHERE tool LIKE 'ctxl-%';
")

# Handle-level retrieval stats (from retrieval_count column)
USED_HANDLES=$(sqlite3 "$DB" "SELECT COUNT(*) FROM handles WHERE retrieval_count > 0;")
UNUSED_HANDLES=$(sqlite3 "$DB" "SELECT COUNT(*) FROM handles WHERE retrieval_count = 0;")
TOTAL_RETRIEVALS=$(sqlite3 "$DB" "SELECT COALESCE(SUM(retrieval_count), 0) FROM handles;")

echo "  Handles created:       $HANDLE_COUNT"
echo "  Compressed handles:    $COMPRESSED_COUNT"
echo "  Handles retrieved:     $USED_HANDLES"
echo "  Handles never used:    $UNUSED_HANDLES"
echo "  Total retrievals:      $TOTAL_RETRIEVALS"
echo "  Retrieval calls:       $RETRIEVAL_COUNT"
echo "  Retrieval tokens:      $RETRIEVAL_TOKENS"
echo ""

if [ "$RETRIEVAL_COUNT" -gt 0 ]; then
  echo "  Per-command breakdown:"
  sqlite3 -column -header "$DB" "
    SELECT
      tool,
      COUNT(*) AS count,
      COALESCE(SUM(line_count), 0) AS lines,
      COALESCE(SUM(token_est), 0) AS est_tokens
    FROM calls
    WHERE tool LIKE 'ctxl-%'
    GROUP BY tool
    ORDER BY count DESC;
  "
  echo ""
fi

# ── 4. Net Value Assessment ─────────────────────────────────────────
echo "── 4. Net Value Assessment ──────────────────────────────────"

TOKENS_SAVED=$TOTAL_TOKEN_EST
NET_SAVINGS=$((TOKENS_SAVED - RETRIEVAL_TOKENS))

echo "  Tokens saved (interception):   $TOKENS_SAVED"
echo "  Tokens spent (retrieval):      $RETRIEVAL_TOKENS"
echo "  Net token savings:             $NET_SAVINGS"
echo ""

if [ "$TOKENS_SAVED" -gt 0 ]; then
  if [ "$NET_SAVINGS" -gt 0 ]; then
    echo "  NET POSITIVE — saved $NET_SAVINGS tokens after retrieval overhead."
  else
    echo "  NET NEGATIVE — retrieval cost exceeded savings. Review retrieval"
    echo "  patterns: are agents retrieving full handles unnecessarily?"
  fi
else
  echo "  No tokens saved — nothing exceeded interception threshold."
fi
echo ""

# ── 5. Verdict ──────────────────────────────────────────────────────
echo "── 5. Verdict ───────────────────────────────────────────────"
if [ "$HANDLE_COUNT" -eq 0 ]; then
  echo "  NO DATA — no outputs exceeded interception threshold."
  echo "  Either the session had small outputs, or hooks didn't fire."
else
  RETRIEVAL_RATE=$((USED_HANDLES * 100 / HANDLE_COUNT))
  echo "  Handle retrieval rate:  ${RETRIEVAL_RATE}% ($USED_HANDLES of $HANDLE_COUNT)"
  if [ "$USED_HANDLES" -eq 0 ]; then
    echo "  HANDLES UNUSED — $HANDLE_COUNT handles created, none retrieved."
    echo "  Threshold may be too aggressive (intercepting outputs the agent"
    echo "  doesn't need to reference)."
  elif [ "$TOKENS_SAVED" -gt 5000 ]; then
    echo "  ACTIVE — saved ~$TOKENS_SAVED tokens across $HANDLE_COUNT intercepts."
    echo "  Review session transcript for retrieval behavior."
  else
    echo "  MARGINAL — saved ~$TOKENS_SAVED tokens across $HANDLE_COUNT intercepts."
    echo "  Small total savings. Consider lowering threshold or review"
    echo "  whether the intercepted outputs were genuinely useful to compress."
  fi
fi
echo ""
echo "═══════════════════════════════════════════════════════════════"
echo "  Manual follow-up: review session transcript for agent behavior"
echo "  around handle messages. Did the agent:"
echo "    - Recognize the handle format?"
echo "    - Retrieve when it needed content?"
echo "    - Proceed correctly when it didn't need content?"
echo "    - Hallucinate about content it didn't have?"
echo "═══════════════════════════════════════════════════════════════"
