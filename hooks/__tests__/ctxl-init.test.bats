#!/usr/bin/env bats

# Tests for ctxl-init.sh — SessionStart hook that provisions ctxl session DB.
# Verifies fail-open behavior, binary resolution, and env file export.

setup() {
  load helpers

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

  # Create mock ctxl binary (release) under plugin-root layout
  mkdir -p target/release
  cat > target/release/ctxl <<'MOCK'
#!/usr/bin/env bash
echo "$@" >> "$MOCK_LOG"
cat >> "$MOCK_STDIN" 2>/dev/null || true
exit 0
MOCK
  chmod +x target/release/ctxl

  # ctxl-init.sh resolves PLUGIN_ROOT from CLAUDE_PLUGIN_ROOT
  export CLAUDE_PLUGIN_ROOT="$TEST_DIR"

  SCRIPT="$BATS_TEST_DIRNAME/../ctxl-init.sh"
}

teardown() {
  if [ -n "${TEST_DIR:-}" ] && [ -d "$TEST_DIR" ]; then
    # Kill any ctxl processes that may still be writing to TEST_DIR
    pkill -f "$TEST_DIR" 2>/dev/null || true
    sleep 0.1
    _assert_temp_path "$TEST_DIR" && rm -rf "$TEST_DIR" 2>/dev/null || true
  fi
}

# ── Fail-open: empty / missing input ──────────────────────────────────

@test "empty stdin exits 0" {
  cd "$TEST_DIR"
  run bash "$SCRIPT" < /dev/null
  [ "$status" -eq 0 ]
  # No ctxl invocation
  [ ! -s "$MOCK_LOG" ]
}

@test "missing session_id exits 0" {
  cd "$TEST_DIR"
  run bash -c 'echo "{\"foo\":\"bar\"}" | bash "'"$SCRIPT"'"'
  [ "$status" -eq 0 ]
  [ ! -s "$MOCK_LOG" ]
}

# ── Valid session ─────────────────────────────────────────────────────

@test "valid session invokes ctxl init" {
  cd "$TEST_DIR"
  echo '{"session_id":"test-abc-123"}' | bash "$SCRIPT"
  grep -q 'init --session-id test-abc-123' "$MOCK_LOG"
}

@test "exports CTXL_SESSION_ID to CLAUDE_ENV_FILE" {
  cd "$TEST_DIR"
  export CLAUDE_ENV_FILE="$TEST_DIR/env_out"
  echo '{"session_id":"sess-42"}' | bash "$SCRIPT"
  grep -qF 'export CTXL_SESSION_ID="sess-42"' "$CLAUDE_ENV_FILE"
}

# ── Binary resolution ────────────────────────────────────────────────

@test "missing binary exits 0" {
  cd "$TEST_DIR"
  rm -rf target
  run bash -c 'echo "{\"session_id\":\"test-1\"}" | bash "'"$SCRIPT"'"'
  [ "$status" -eq 0 ]
}

@test "release binary preferred over debug" {
  cd "$TEST_DIR"

  # Create separate mock binaries that identify themselves
  RELEASE_LOG="$TEST_DIR/release.log"
  DEBUG_LOG="$TEST_DIR/debug.log"
  export RELEASE_LOG DEBUG_LOG

  cat > target/release/ctxl <<'MOCK'
#!/usr/bin/env bash
echo "release: $@" >> "$RELEASE_LOG"
cat > /dev/null 2>/dev/null || true
exit 0
MOCK
  chmod +x target/release/ctxl

  mkdir -p target/debug
  cat > target/debug/ctxl <<'MOCK'
#!/usr/bin/env bash
echo "debug: $@" >> "$DEBUG_LOG"
cat > /dev/null 2>/dev/null || true
exit 0
MOCK
  chmod +x target/debug/ctxl

  touch "$RELEASE_LOG" "$DEBUG_LOG"
  echo '{"session_id":"pref-test"}' | bash "$SCRIPT"

  # Release was called, debug was not
  [ -s "$RELEASE_LOG" ]
  [ ! -s "$DEBUG_LOG" ]
}

# ── Background clean ─────────────────────────────────────────────────

@test "background clean spawned" {
  cd "$TEST_DIR"
  echo '{"session_id":"clean-test"}' | bash "$SCRIPT"
  # Give background process a moment to write
  sleep 0.2
  grep -q 'clean' "$MOCK_LOG"
}
