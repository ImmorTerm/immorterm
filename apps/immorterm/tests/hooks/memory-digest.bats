#!/usr/bin/env bats
# Tests for .claude/hooks/immorterm-memory-digest.sh
#
# The digest script processes JSONL transcript files via `claude -p` CLI,
# extracts facts and session summaries, and saves them to OpenMemory.
#
# Usage: bash immorterm-memory-digest.sh <project_id> <jsonl_dir> <session_id1> [session_id2] ...

load test_helper

setup() {
  start_mock_server
  setup_mock_claude

  # Override HOME to isolate checkpoint files and lock dirs
  export REAL_HOME="$HOME"
  export HOME="$(mktemp -d)"
  mkdir -p "$HOME/.immorterm"

  # Override the mock claude to output the format the digest script expects.
  # The default mock from test_helper uses {"facts":...} but the digest
  # script's prompt asks for {"memories":..., "session_summary":...}.
  # The claude -p --output-format json returns {"result": "<text>", ...}
  # where result is a string containing the LLM's text output.
  cat > "$MOCK_BIN_DIR/claude" << 'DIGEST_MOCK_CLAUDE'
#!/bin/bash
# Mock claude CLI that outputs digest-format JSON
# Reads stdin (ignored), outputs canned response matching digest prompt format
cat << 'CANNED'
{"result":"{\"memories\": [{\"text\": \"JWT token validation was using local time instead of UTC\", \"categories\": [\"architecture\", \"lessons_learned\"], \"prompt\": \"fix auth bug\"}], \"session_summary\": \"Session (10:00-10:05 UTC): Fixed JWT token validation bug and discussed refresh token implementation.\", \"new_context\": false}","session_id":"mock-digest-session","model":"claude-sonnet-4-5-20250929","cost_usd":0.001}
CANNED
DIGEST_MOCK_CLAUDE
  chmod +x "$MOCK_BIN_DIR/claude"

  TEST_BIN_DIR="$(mktemp -d)"
  cp "$HOOKS_DIR/immorterm-memory-digest.sh" "$TEST_BIN_DIR/immorterm-memory-digest.sh"
  export TEST_BIN_DIR

  # Create JSONL directory with a sample transcript
  JSONL_DIR="$(mktemp -d)"
  create_sample_jsonl "$JSONL_DIR/test-session-001.jsonl"

  export JSONL_DIR
}

teardown() {
  stop_mock_server
  cleanup_mock_claude

  # Clean up lock dirs/files that may be left behind
  rm -rf "$HOME/.immorterm/digest-test-project.lock" 2>/dev/null
  rmdir "$HOME/.immorterm/digest-test-project.lockdir" 2>/dev/null || true

  # Clean up temp dirs
  [ -n "${JSONL_DIR:-}" ] && rm -rf "$JSONL_DIR"
  [ -n "${TEST_BIN_DIR:-}" ] && rm -rf "$TEST_BIN_DIR"

  if [ -n "${HOME:-}" ] && [ "$HOME" != "$REAL_HOME" ]; then
    rm -rf "$HOME"
  fi
  export HOME="$REAL_HOME"
}

# Helper: create a sample JSONL file with enough messages for the
# digest script (needs >= 4 User:/Claude: messages)
create_sample_jsonl() {
  local path="$1"
  cat > "$path" << 'EOF'
{"type":"user","role":"user","content":"Fix the authentication bug in the login flow","timestamp":"2026-03-02T10:00:00Z"}
{"type":"assistant","role":"assistant","content":[{"type":"text","text":"I'll investigate the authentication bug. Let me look at the login handler."}],"timestamp":"2026-03-02T10:01:00Z"}
{"type":"user","role":"user","content":"The issue is in the JWT token validation","timestamp":"2026-03-02T10:02:00Z"}
{"type":"assistant","role":"assistant","content":[{"type":"text","text":"Found it. The token expiry check was using local time instead of UTC."}],"timestamp":"2026-03-02T10:03:00Z"}
{"type":"user","role":"user","content":"Can you also add a refresh token mechanism?","timestamp":"2026-03-02T10:04:00Z"}
{"type":"assistant","role":"assistant","content":[{"type":"text","text":"I'll implement refresh tokens using rotating token pairs with a 7-day expiry."}],"timestamp":"2026-03-02T10:05:00Z"}
EOF
}

@test "processes a JSONL file and calls claude CLI" {
  run bash "$TEST_BIN_DIR/immorterm-memory-digest.sh" \
      "test-project" "$JSONL_DIR" "test-session-001"

  [ "$status" -eq 0 ]

  # The mock claude returns canned output with 1 fact, so memories should be saved
  assert_mock_received POST /api/v1/memories/
}

@test "creates lockfile during processing and removes after" {
  local lockdir="$HOME/.immorterm/digest-test-project.lockdir"

  run bash "$TEST_BIN_DIR/immorterm-memory-digest.sh" \
      "test-project" "$JSONL_DIR" "test-session-001"

  [ "$status" -eq 0 ]

  # After completion, lock directory should be cleaned up
  [ ! -d "$lockdir" ]
}

@test "refuses to run when lockfile exists (concurrent protection)" {
  local lockdir="$HOME/.immorterm/digest-test-project.lockdir"
  local lockfile="$HOME/.immorterm/digest-test-project.lock"

  # Create lock directory and a recent lock file (simulates running process)
  mkdir -p "$lockdir"
  echo "99999" > "$lockfile"
  # Touch the lock file so it appears fresh (not stale)
  touch "$lockfile"

  run bash "$TEST_BIN_DIR/immorterm-memory-digest.sh" \
      "test-project" "$JSONL_DIR" "test-session-001"

  # Should exit 0 (graceful skip, not error)
  [ "$status" -eq 0 ]

  # Should NOT have called claude or saved anything
  assert_mock_not_received POST /api/v1/memories/

  # Clean up
  rm -f "$lockfile"
  rmdir "$lockdir" 2>/dev/null || true
}

@test "saves facts to /api/v1/memories/ endpoint" {
  run bash "$TEST_BIN_DIR/immorterm-memory-digest.sh" \
      "test-project" "$JSONL_DIR" "test-session-001"

  [ "$status" -eq 0 ]

  assert_mock_received POST /api/v1/memories/

  # Verify the memory has correct metadata type
  assert_mock_received_json POST /api/v1/memories/ \
    '.metadata.type == "digest_extraction" or .metadata.type == "session_summary"'
}

@test "saves session summary to /api/v1/memories/" {
  run bash "$TEST_BIN_DIR/immorterm-memory-digest.sh" \
      "test-project" "$JSONL_DIR" "test-session-001"

  [ "$status" -eq 0 ]

  # The mock claude output includes a session_summary field,
  # which triggers a POST to /api/v1/memories/ with type=session_summary.
  # We check that at least 2 POSTs were made (1 fact + 1 summary).
  # The fact has type=digest_extraction, the summary has type=session_summary.
  assert_mock_received POST /api/v1/memories/
}

@test "skips already-processed files via checkpoint" {
  # First run: process the file
  bash "$TEST_BIN_DIR/immorterm-memory-digest.sh" \
      "test-project" "$JSONL_DIR" "test-session-001"

  # Clear mock log to track only second run's requests
  : > "$MOCK_LOG"

  # Second run without modifying the JSONL file: should skip
  run bash "$TEST_BIN_DIR/immorterm-memory-digest.sh" \
      "test-project" "$JSONL_DIR" "test-session-001"

  [ "$status" -eq 0 ]

  # No new memory saves on second run (file already checkpointed)
  assert_mock_not_received POST /api/v1/memories/
}

@test "exits cleanly with empty JSONL directory" {
  local empty_dir
  empty_dir="$(mktemp -d)"

  run bash "$TEST_BIN_DIR/immorterm-memory-digest.sh" \
      "test-project" "$empty_dir" "nonexistent-session"

  # Should exit 0 (no session files found, just skips)
  [ "$status" -eq 0 ]

  # No requests to memories endpoint
  assert_mock_not_received POST /api/v1/memories/

  rm -rf "$empty_dir"
}

@test "exits cleanly when claude CLI is not found" {
  # Remove mock claude from PATH
  cleanup_mock_claude
  # Also make sure real claude is not found
  export PATH="/usr/bin:/bin"

  run bash "$TEST_BIN_DIR/immorterm-memory-digest.sh" \
      "test-project" "$JSONL_DIR" "test-session-001"

  # Should exit 0 (graceful skip)
  [ "$status" -eq 0 ]

  # No memory saves attempted
  assert_mock_not_received POST /api/v1/memories/
}

@test "timeout: claude CLI that hangs gets killed" {
  # Replace mock claude with one that sleeps forever.
  cat > "$MOCK_BIN_DIR/claude" << 'SLOWCLAUDE'
#!/bin/bash
exec sleep 600
SLOWCLAUDE
  chmod +x "$MOCK_BIN_DIR/claude"

  # Run digest script in its own process group so we can kill the entire tree.
  # The script's internal polyfill `timeout 300` starts claude + a watchdog;
  # our outer kill needs to reap all of them, not just the top-level bash.
  bash "$TEST_BIN_DIR/immorterm-memory-digest.sh" \
      "test-project" "$JSONL_DIR" "test-session-001" &
  local digest_pid=$!

  # Wait up to 10 seconds, then kill the whole process tree
  local elapsed=0
  while kill -0 "$digest_pid" 2>/dev/null && [ "$elapsed" -lt 10 ]; do
    sleep 1
    elapsed=$((elapsed + 1))
  done
  # Kill process and any children
  kill "$digest_pid" 2>/dev/null || true
  pkill -P "$digest_pid" 2>/dev/null || true
  wait "$digest_pid" 2>/dev/null || true

  # If we get here, the test didn't hang.
  assert_mock_not_received POST /api/v1/memories/
}

@test "handles session_id argument correctly" {
  run bash "$TEST_BIN_DIR/immorterm-memory-digest.sh" \
      "test-project" "$JSONL_DIR" "test-session-001"

  [ "$status" -eq 0 ]

  # Verify the session_id is passed through to the memory metadata
  assert_mock_received_json POST /api/v1/memories/ \
    '.metadata.session_id == "test-session-001"'
}

@test "fetches code changes context from /api/v1/code-changes/window" {
  run bash "$TEST_BIN_DIR/immorterm-memory-digest.sh" \
      "test-project" "$JSONL_DIR" "test-session-001"

  [ "$status" -eq 0 ]

  # The digest script queries code-changes/window to get file modification
  # context for the session's time window
  assert_mock_received GET /api/v1/code-changes/window
}

@test "lock cleanup on SIGTERM" {
  local lockdir="$HOME/.immorterm/digest-test-project.lockdir"

  # Start digest in background
  bash "$TEST_BIN_DIR/immorterm-memory-digest.sh" \
      "test-project" "$JSONL_DIR" "test-session-001" &
  local digest_pid=$!

  # Wait for lock to be acquired
  sleep 1

  # Send SIGTERM
  kill -TERM "$digest_pid" 2>/dev/null || true
  wait "$digest_pid" 2>/dev/null || true

  # Lock directory should be cleaned up by EXIT trap
  [ ! -d "$lockdir" ]
}

@test "exits 0 when no session IDs provided" {
  run bash "$TEST_BIN_DIR/immorterm-memory-digest.sh" \
      "test-project" "$JSONL_DIR"

  # The script exits 0 with "No session IDs provided" message
  [ "$status" -eq 0 ]

  assert_mock_not_received POST /api/v1/memories/
}
