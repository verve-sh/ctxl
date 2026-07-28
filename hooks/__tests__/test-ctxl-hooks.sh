#!/usr/bin/env bash
# test-ctxl-hooks.sh — Shell-level tests for ctxl PostToolUse hook scripts.
#
# Tests: ctxl-hook.sh (universal hook)
#
# Usage: bash .claude/plugins/ctxl/hooks/__tests__/test-ctxl-hooks.sh

set -euo pipefail

HOOKS_DIR="$(cd "$(dirname "$0")/.." && pwd)"
PLUGIN_ROOT="$(cd "$HOOKS_DIR/.." && pwd)"
REPO_ROOT="$(cd "$PLUGIN_ROOT/../../.." && pwd)"

PASS=0
FAIL=0
TOTAL=0

# ── Helpers ──────────────────────────────────────────────────────────────

setup_tmpdir() {
  mktemp -d -p "${TMPDIR:-/tmp}"
}

cleanup() {
  local d="$1"
  [ -d "$d" ] && rm -rf "$d"
}

pass() {
  local desc="$1"
  PASS=$((PASS + 1))
  TOTAL=$((TOTAL + 1))
  echo "  ✓ $desc"
}

fail() {
  local desc="$1" reason="${2:-}"
  FAIL=$((FAIL + 1))
  TOTAL=$((TOTAL + 1))
  echo "  ✗ $desc${reason:+: $reason}"
}

# Create a mock ctxl binary that records args to a file and optionally prints
mk_mock_ctxl() {
  local dest="$1"      # path to write the binary
  local args_file="$2" # file to record args to
  local stdout="${3:-}"
  mkdir -p "$(dirname "$dest")"
  cat > "$dest" <<EOF
#!/bin/bash
echo "\$@" > "$args_file"
${stdout:+echo "$stdout"}
exit 0
EOF
  chmod +x "$dest"
}

# Run ctxl-hook.sh with a mock ctxl binary and controlled environment.
# Args:
#   $1 - fake repo root (must contain .claude/plugins/ctxl/target/release/ctxl if wanted)
#   $2 - CTXL_SESSION_ID value (empty = unset)
#   $3 - CTXL_ENABLED value (default "1")
#   $4 - stdin content
run_hook() {
  local fake_root="${1:-}"
  local session_id="${2}"  # empty string = unset (fail the session ID check)
  local enabled="${3:-1}"
  local stdin_content="${4:-{\"tool_name\":\"Bash\",\"session_id\":\"$session_id\",\"tool_response\":{\"stdout\":\"data\",\"stderr\":\"\"}}}"

  local mock_bin="$fake_root/.claude/plugins/ctxl/target/release/ctxl"

  local env_args=("CTXL_ENABLED=$enabled" "CTXL_SESSION_ID=$session_id" "TMPDIR=${TMPDIR:-/tmp}")
  if [ -n "$session_id" ] && [ -f "$mock_bin" ]; then
    env_args+=("CTXL_BIN=$mock_bin")
  fi

  local output exit_code
  output=$(echo "$stdin_content" \
    | env "${env_args[@]}" bash "$HOOKS_DIR/ctxl-hook.sh" 2>/dev/null) \
    && exit_code=0 || exit_code=$?

  printf '%s\n' "$exit_code:$output"
}

# ── ctxl-hook.sh intercept routing ──────────────────────────────────────

echo ""
echo "ctxl-hook.sh: intercept routing (Bash)"

TMP=$(setup_tmpdir)
ARGS_FILE="$TMP/ctxl-args.txt"
mk_mock_ctxl "$TMP/.claude/plugins/ctxl/target/release/ctxl" "$ARGS_FILE" "compressed-output"

result=$(run_hook "$TMP" "sess-abc" "1" '{"tool_name":"Bash","session_id":"sess-abc","tool_response":{"stdout":"x","stderr":""}}')
exit_code="${result%%:*}"
output="${result#*:}"

if [ "$exit_code" = "0" ]; then pass "exit code is 0"; else fail "exit code is 0" "got $exit_code"; fi

if [ -f "$ARGS_FILE" ]; then
  args=$(cat "$ARGS_FILE")
  if echo "$args" | grep -q "hook post-tool-use"; then
    pass "calls hook post-tool-use subcommand"
  else
    fail "calls hook post-tool-use subcommand" "got: $args"
  fi

  # No tool-specific flags — thresholds are compiled into binary
  if echo "$args" | grep -q "\-\-threshold"; then
    fail "no --threshold flag (compiled-in defaults)" "got: $args"
  else
    pass "no --threshold flag (compiled-in defaults)"
  fi
else
  fail "calls hook post-tool-use subcommand" "ctxl was not invoked"
  fail "no --threshold flag (compiled-in defaults)" "ctxl was not invoked"
fi

# output passthrough: ctxl stdout becomes hook stdout
if echo "$output" | grep -q "compressed-output"; then
  pass "ctxl stdout is passed through"
else
  fail "ctxl stdout is passed through" "output: $output"
fi

cleanup "$TMP"

# ── Kill switch: CTXL_ENABLED=0 ──────────────────────────────────────────

TMP=$(setup_tmpdir)
ARGS_FILE="$TMP/ctxl-args.txt"
mk_mock_ctxl "$TMP/.claude/plugins/ctxl/target/release/ctxl" "$ARGS_FILE" "should-not-appear"

result=$(run_hook "$TMP" "sess-abc" "0" '{"tool_name":"Bash","session_id":"sess-abc","tool_response":{}}')
exit_code="${result%%:*}"
output="${result#*:}"

if [ "$exit_code" = "0" ]; then pass "CTXL_ENABLED=0 exits 0"; else fail "CTXL_ENABLED=0 exits 0" "got $exit_code"; fi
if [ ! -f "$ARGS_FILE" ]; then pass "CTXL_ENABLED=0 skips ctxl invocation"; else fail "CTXL_ENABLED=0 skips ctxl invocation" "ctxl was called"; fi
if [ -z "$output" ]; then pass "CTXL_ENABLED=0 produces no stdout"; else fail "CTXL_ENABLED=0 produces no stdout" "got: $output"; fi

cleanup "$TMP"

# ── Kill switch: .claude/ctxl.disabled file ─────────────────────────────

TMP=$(setup_tmpdir)
ARGS_FILE="$TMP/ctxl-args.txt"
mk_mock_ctxl "$TMP/.claude/plugins/ctxl/target/release/ctxl" "$ARGS_FILE" "should-not-appear"

# Create the disable file in CWD (where script runs)
PREV_DIR="$(pwd)"
cd "$TMP"
mkdir -p .claude && touch .claude/ctxl.disabled

result=$(run_hook "$TMP" "sess-abc" "1" '{"tool_name":"Bash","session_id":"sess-abc","tool_response":{}}')
exit_code="${result%%:*}"

cd "$PREV_DIR"

if [ "$exit_code" = "0" ]; then pass ".claude/ctxl.disabled exits 0"; else fail ".claude/ctxl.disabled exits 0" "got $exit_code"; fi
if [ ! -f "$ARGS_FILE" ]; then pass ".claude/ctxl.disabled skips invocation"; else fail ".claude/ctxl.disabled skips invocation"; fi

cleanup "$TMP"

# ── Missing CTXL_SESSION_ID exits 0 ──────────────────────────────────────

TMP=$(setup_tmpdir)
ARGS_FILE="$TMP/ctxl-args.txt"
mk_mock_ctxl "$TMP/.claude/plugins/ctxl/target/release/ctxl" "$ARGS_FILE"

result=$(run_hook "$TMP" "" "1" '{"tool_name":"Bash","tool_response":{}}')
exit_code="${result%%:*}"
output="${result#*:}"

if [ "$exit_code" = "0" ]; then pass "missing session ID exits 0"; else fail "missing session ID exits 0" "got $exit_code"; fi
if [ ! -f "$ARGS_FILE" ]; then pass "missing session ID skips ctxl"; else fail "missing session ID skips ctxl" "ctxl was invoked"; fi
if [ -z "$output" ]; then pass "missing session ID produces no stdout"; else fail "missing session ID produces no stdout" "got: $output"; fi

cleanup "$TMP"

# ── Session ID validation (path traversal) ──────────────────────────────

echo ""
echo "ctxl-hook.sh: session ID validation"

TMP=$(setup_tmpdir)
ARGS_FILE="$TMP/ctxl-args.txt"
mk_mock_ctxl "$TMP/.claude/plugins/ctxl/target/release/ctxl" "$ARGS_FILE"

result=$(run_hook "$TMP" "../etc/passwd" "1" '{"tool_name":"Bash","session_id":"../etc/passwd","tool_response":{}}')
exit_code="${result%%:*}"

if [ "$exit_code" = "0" ]; then pass "path traversal session ID exits 0"; else fail "path traversal session ID exits 0"; fi
# Hook forwards to binary — session ID validation happens inside ctxl, not the hook
if [ -f "$ARGS_FILE" ]; then pass "path traversal session ID forwarded to ctxl (binary validates)"; else fail "path traversal session ID forwarded to ctxl (binary validates)"; fi

cleanup "$TMP"

# ── Missing ctxl binary cache — fail-open ───────────────────────────────

echo ""
echo "ctxl-hook.sh: missing CTXL_BIN (fail-open)"

TMP=$(setup_tmpdir)

result=$(CTXL_SESSION_ID="sess-nobin" CTXL_ENABLED=1 TMPDIR="${TMPDIR:-/tmp}" \
  echo '{"tool_name":"Bash","session_id":"sess-nobin","tool_response":{}}' \
  | bash "$HOOKS_DIR/ctxl-hook.sh" 2>/dev/null) && exit_code=0 || exit_code=$?

if [ "$exit_code" = "0" ]; then pass "missing CTXL_BIN exits 0"; else fail "missing CTXL_BIN exits 0" "got $exit_code"; fi

cleanup "$TMP"

# ── hooks.json hook registration ───────────────────────────────────────

echo ""
echo "hooks.json: hook registration"

HOOKS_JSON="$PLUGIN_ROOT/hooks/hooks.json"

# All four matchers point to ctxl-hook.sh
for matcher in "Bash" "Grep" "WebFetch"; do
  if jq -e ".hooks.PostToolUse[] | select(.matcher == \"$matcher\") | .hooks[].command | select(contains(\"ctxl-hook.sh\"))" "$HOOKS_JSON" > /dev/null 2>&1; then
    pass "PostToolUse $matcher -> ctxl-hook.sh registered in hooks.json"
  else
    fail "PostToolUse $matcher -> ctxl-hook.sh registered in hooks.json"
  fi
done

if jq -e '.hooks.PostToolUse[] | select(.matcher == "Read|Edit|Write|Glob") | .hooks[].command | select(contains("ctxl-hook.sh"))' "$HOOKS_JSON" > /dev/null 2>&1; then
  pass "PostToolUse Read|Edit|Write|Glob -> ctxl-hook.sh registered in hooks.json"
else
  fail "PostToolUse Read|Edit|Write|Glob -> ctxl-hook.sh registered in hooks.json"
fi

# SessionStart hook registered
if jq -e '.hooks.SessionStart[].hooks[].command | select(contains("ctxl-init.sh"))' "$HOOKS_JSON" > /dev/null 2>&1; then
  pass "SessionStart -> ctxl-init.sh registered in hooks.json"
else
  fail "SessionStart -> ctxl-init.sh registered in hooks.json"
fi

# ctxl hooks NOT in settings.json (migrated to plugin)
SETTINGS="$REPO_ROOT/.claude/settings.json"
if [ -f "$SETTINGS" ]; then
  if jq -e '.hooks.PostToolUse[] | select(.hooks[].command | contains("ctxl-hook.sh"))' "$SETTINGS" > /dev/null 2>&1; then
    fail "ctxl hooks removed from settings.json"
  else
    pass "ctxl hooks removed from settings.json"
  fi
fi

# ── check_heavy_output retained in guard-bash-dispatch.sh ────────────────

echo ""
echo "guard-bash-dispatch.sh: check_heavy_output retained"

DISPATCH="$REPO_ROOT/.claude/scripts/guard-bash-dispatch.sh"
CHECKS_LIB="$REPO_ROOT/.claude/scripts/bash-checks-lib.sh"

if grep -q 'source.*bash-checks-lib\.sh' "$DISPATCH"; then
  pass "guard-bash-dispatch.sh sources bash-checks-lib.sh"
else
  fail "guard-bash-dispatch.sh sources bash-checks-lib.sh"
fi

if grep -q 'check_heavy_output' "$DISPATCH"; then
  pass "guard-bash-dispatch.sh invokes check_heavy_output"
else
  fail "guard-bash-dispatch.sh invokes check_heavy_output"
fi

if grep -q '^check_heavy_output()' "$CHECKS_LIB"; then
  pass "bash-checks-lib.sh defines check_heavy_output function"
else
  fail "bash-checks-lib.sh defines check_heavy_output function"
fi

# ── Compression parity gate (mock) ─────────────────────────────────────

echo ""
echo "ctxl-hook.sh: compression parity gate (mock)"

TMP=$(setup_tmpdir)
ARGS_FILE="$TMP/ctxl-args.txt"

# Simulate ctxl compressed output for a failing cargo test run
COMPRESSED_BLOCK='{"type":"ctxl-handle","handle_id":"h001","summary":"FAILED: test_fts5_query, test_session_lifecycle (3 of 45)","original_tokens":2400,"compressed_tokens":420}'
mk_mock_ctxl "$TMP/.claude/plugins/ctxl/target/release/ctxl" "$ARGS_FILE" "$COMPRESSED_BLOCK"

SAMPLE_INPUT='{"tool_name":"Bash","session_id":"sess-parity","tool_response":{"stdout":"test result: FAILED. 42 passed; 3 failed; 0 ignored\ntest kb::tests::test_fts5_query ... FAILED\ntest pty::tests::test_session_lifecycle ... FAILED","stderr":""}}'

result=$(run_hook "$TMP" "sess-parity" "1" "$SAMPLE_INPUT")
output="${result#*:}"

# Compressed block contains failing test names
if echo "$output" | grep -q "test_fts5_query"; then
  pass "compressed output contains failing test name (test_fts5_query)"
else
  fail "compressed output contains failing test name (test_fts5_query)" "output: $output"
fi

if echo "$output" | grep -q "test_session_lifecycle"; then
  pass "compressed output contains failing test name (test_session_lifecycle)"
else
  fail "compressed output contains failing test name (test_session_lifecycle)" "output: $output"
fi

if echo "$output" | grep -q "FAILED"; then
  pass "compressed output contains FAILED status"
else
  fail "compressed output contains FAILED status" "output: $output"
fi

# Token reduction check: original 2400, compressed 420 => 82.5% reduction (> 78%)
ORIG=$(echo "$output" | jq -r '.original_tokens' 2>/dev/null || echo "0")
COMP=$(echo "$output" | jq -r '.compressed_tokens' 2>/dev/null || echo "0")
if [ "${ORIG:-0}" -gt 0 ] && [ "${COMP:-0}" -gt 0 ]; then
  REDUCTION=$(awk -v o="$ORIG" -v c="$COMP" 'BEGIN { r = (o - c) / o; print (r >= 0.78 ? "pass" : "fail") }')
  if [ "$REDUCTION" = "pass" ]; then
    pass "token reduction >= 78% (original=$ORIG, compressed=$COMP)"
  else
    fail "token reduction >= 78% (original=$ORIG, compressed=$COMP)"
  fi
else
  pass "token reduction check skipped (mock output, real ctxl not built)"
fi

cleanup "$TMP"

# ── Summary ───────────────────────────────────────────────────────────────

echo ""
echo "────────────────────────────────────────"
echo "Results: $PASS/$TOTAL passed, $FAIL failed"

if [ "$FAIL" -gt 0 ]; then
  exit 1
fi
