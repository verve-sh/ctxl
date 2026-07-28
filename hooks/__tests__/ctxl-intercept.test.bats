#!/usr/bin/env bats

# Tests for ctxl-hook.sh — universal PostToolUse hook: pipes output through ctxl intercept.
# Verifies kill switches, fail-open behavior, binary resolution, and correct argument passing.

setup() {
  load helpers

  # Clean any stale ctxl marker files from prior tests
  rm -f "${TMPDIR:-/tmp}"/ctxl-debug-* "${TMPDIR:-/tmp}"/ctxl-norecord-* 2>/dev/null || true

  TEST_DIR=$(_safe_mktemp_d)
  MOCK_LOG="$TEST_DIR/mock.log"
  MOCK_STDIN="$TEST_DIR/mock.stdin"
  touch "$MOCK_LOG" "$MOCK_STDIN"

  # Export for the mock binary to use
  export MOCK_LOG MOCK_STDIN

  # Prevent CI workspace git state from leaking
  unset GIT_DIR GIT_WORK_TREE GIT_CEILING_DIRECTORIES 2>/dev/null || true

  # Create mock git repo
  cd "$TEST_DIR"
  git init -b main --quiet
  git config user.email "test@test.com"
  git config user.name "Test"
  echo "init" > README.md
  git add README.md
  git commit -m "init" --quiet

  # Create mock ctxl binary at a path matching the hook's validation regex
  # ctxl-hook.sh validates: /.*/\.claude/plugins/ctxl/target/(release|debug)/ctxl
  MOCK_PLUGIN="$TEST_DIR/.claude/plugins/ctxl"
  mkdir -p "$MOCK_PLUGIN/target/release"
  cat > "$MOCK_PLUGIN/target/release/ctxl" <<'MOCK'
#!/usr/bin/env bash
echo "$@" >> "$MOCK_LOG"
cat >> "$MOCK_STDIN" 2>/dev/null || true
exit 0
MOCK
  chmod +x "$MOCK_PLUGIN/target/release/ctxl"

  export CLAUDE_PLUGIN_ROOT="$MOCK_PLUGIN"

  SCRIPT="$BATS_TEST_DIRNAME/../ctxl-hook.sh"
}

teardown() {
  # Clean up ctxl marker files created by tests
  rm -f "${TMPDIR:-/tmp}"/ctxl-debug-* "${TMPDIR:-/tmp}"/ctxl-norecord-* 2>/dev/null || true
  if [ -n "${TEST_DIR:-}" ] && [ -d "$TEST_DIR" ]; then
    _assert_temp_path "$TEST_DIR" && rm -rf "$TEST_DIR"
  fi
}

# ── Kill switches ────────────────────────────────────────────────────

@test "CTXL_ENABLED=0 exits 0" {
  cd "$TEST_DIR"
  run bash -c 'CTXL_ENABLED=0 CTXL_SESSION_ID=test-1 CTXL_BIN="'"$MOCK_PLUGIN/target/release/ctxl"'" bash "'"$SCRIPT"'"'
  [ "$status" -eq 0 ]
  [ ! -s "$MOCK_LOG" ]
}

@test "disable file exits 0" {
  cd "$TEST_DIR"
  mkdir -p .claude
  touch .claude/ctxl.disabled
  export CTXL_BIN="$MOCK_PLUGIN/target/release/ctxl"
  export CTXL_SESSION_ID="test-1"
  run bash "$SCRIPT"
  [ "$status" -eq 0 ]
  [ ! -s "$MOCK_LOG" ]
}

# ── Missing session ID ──────────────────────────────────────────────

@test "missing CTXL_SESSION_ID exits 0" {
  cd "$TEST_DIR"
  unset CTXL_SESSION_ID 2>/dev/null || true
  run bash "$SCRIPT"
  [ "$status" -eq 0 ]
  [ ! -s "$MOCK_LOG" ]
}

# ── Valid invocation ─────────────────────────────────────────────────

@test "pipes stdin to ctxl hook post-tool-use" {
  cd "$TEST_DIR"
  export CTXL_SESSION_ID="sess-42"
  export CTXL_BIN="$MOCK_PLUGIN/target/release/ctxl"
  echo '{"tool_name":"Bash","session_id":"sess-42","tool_response":"data"}' | bash "$SCRIPT"
  grep -q 'hook post-tool-use' "$MOCK_LOG"
  grep -qF 'tool_name' "$MOCK_STDIN"
}

@test "session-id extracted from JSON payload" {
  cd "$TEST_DIR"
  export CTXL_SESSION_ID="sess-42"
  export CTXL_BIN="$MOCK_PLUGIN/target/release/ctxl"
  echo '{"tool_name":"Bash","session_id":"sess-42","tool_response":"data"}' | bash "$SCRIPT"
  # hook post-tool-use reads session_id from JSON, not CLI args
  grep -q 'hook post-tool-use' "$MOCK_LOG"
  grep -qF '"session_id":"sess-42"' "$MOCK_STDIN"
}

# ── Binary resolution ────────────────────────────────────────────────

@test "missing CTXL_BIN exits 0" {
  cd "$TEST_DIR"
  unset CTXL_BIN 2>/dev/null || true
  export CTXL_SESSION_ID="test-1"
  run bash -c 'echo "data" | bash "'"$SCRIPT"'"'
  [ "$status" -eq 0 ]
  [ ! -s "$MOCK_LOG" ]
}

@test "non-executable CTXL_BIN exits 0" {
  cd "$TEST_DIR"
  export CTXL_BIN="/tmp/not-a-ctxl-binary"
  export CTXL_SESSION_ID="test-1"
  run bash -c 'echo "data" | bash "'"$SCRIPT"'"'
  [ "$status" -eq 0 ]
  [ ! -s "$MOCK_LOG" ]
}

@test "ctxl failure exits 0" {
  cd "$TEST_DIR"

  # Replace mock with one that fails
  cat > "$MOCK_PLUGIN/target/release/ctxl" <<'MOCK'
#!/usr/bin/env bash
exit 1
MOCK
  chmod +x "$MOCK_PLUGIN/target/release/ctxl"

  export CTXL_BIN="$MOCK_PLUGIN/target/release/ctxl"
  export CTXL_SESSION_ID="test-1"
  run bash -c 'echo "data" | bash "'"$SCRIPT"'"'
  [ "$status" -eq 0 ]
}

@test "session ID with path traversal rejected" {
  cd "$TEST_DIR"
  export CTXL_SESSION_ID="../etc/passwd"
  run bash -c 'echo "data" | bash "'"$SCRIPT"'"'
  [ "$status" -eq 0 ]
  [ ! -s "$MOCK_LOG" ]
}

@test "debug mode sets CTXL_DEBUG" {
  cd "$TEST_DIR"
  export CTXL_SESSION_ID="sess-debug"
  export CTXL_BIN="$MOCK_PLUGIN/target/release/ctxl"
  touch "${TMPDIR:-/tmp}/ctxl-debug-sess-debug"

  # Replace mock to check CTXL_DEBUG env
  cat > "$MOCK_PLUGIN/target/release/ctxl" <<'MOCK'
#!/usr/bin/env bash
echo "CTXL_DEBUG=$CTXL_DEBUG" >> "$MOCK_LOG"
cat > /dev/null 2>/dev/null || true
exit 0
MOCK
  chmod +x "$MOCK_PLUGIN/target/release/ctxl"

  echo '{"tool_name":"Bash","session_id":"sess-debug","tool_response":"x"}' | bash "$SCRIPT"
  grep -q 'CTXL_DEBUG=1' "$MOCK_LOG"
}
