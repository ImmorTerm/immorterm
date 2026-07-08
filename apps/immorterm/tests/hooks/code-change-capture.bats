#!/usr/bin/env bats
# code-change-capture.bats — Tests for immorterm-code-change-capture.sh
#
# This hook is an ASYNC PostToolUse hook that captures file diffs from
# Write/Edit/MultiEdit operations and POSTs them to the code-changes API.
# It also fires a background checkpoint POST.

load test_helper

HOOK_NAME="immorterm-code-change-capture.sh"

# Create a patched copy of the hook INSIDE the test project so that
# dirname "$0" resolves to $TEST_PROJECT_ROOT/.claude/hooks/ and
# _immorterm-env.sh + PROJECT_ROOT derivation work correctly.
_create_patched_hook() {
  PATCHED_HOOK="$TEST_PROJECT_ROOT/.claude/hooks/$HOOK_NAME"
  # Remove the symlink first — create_test_project symlinks to the real hook,
  # and > would follow the symlink and TRUNCATE the original file.
  rm -f "$PATCHED_HOOK"
  cp "$HOOKS_DIR/$HOOK_NAME" "$PATCHED_HOOK"
  chmod +x "$PATCHED_HOOK"
}

setup() {
  start_mock_server
  create_test_project
  _create_patched_hook

  # Create a temp file that the hook can read for after_hash computation
  TEST_FILE="$(mktemp)"
  echo "test file content" > "$TEST_FILE"
}

teardown() {
  stop_mock_server
  cleanup_test_project
  [ -f "${TEST_FILE:-}" ] && rm -f "$TEST_FILE"
}

# ── Helper ─────────────────────────────────────────────────────────────

_run_hook() {
  # Run the patched hook with stdin from build_hook_input
  "$@" | bash "$PATCHED_HOOK"
}

# ── Tests ──────────────────────────────────────────────────────────────

@test "code-change-capture: skips non-Write/Edit/MultiEdit tools" {
  local input
  input=$(build_hook_input \
    --session-id "test-sess-001" \
    --tool-name "Bash" \
    --tool-input '{"command": "ls"}' \
    --tool-response '{}')

  echo "$input" | bash "$PATCHED_HOOK"
  sleep 0.5

  # Should not POST to code-changes
  assert_mock_not_received POST /api/v1/code-changes/
}

@test "code-change-capture: skips when session_id is missing" {
  local input
  input=$(build_hook_input \
    --tool-name "Edit" \
    --tool-input "{\"file_path\": \"$TEST_FILE\", \"old_string\": \"old\", \"new_string\": \"new\"}" \
    --tool-response '{}')

  echo "$input" | bash "$PATCHED_HOOK"
  sleep 0.5

  assert_mock_not_received POST /api/v1/code-changes/
}

@test "code-change-capture: skips Read tool" {
  local input
  input=$(build_hook_input \
    --session-id "test-sess-001" \
    --tool-name "Read" \
    --tool-input "{\"file_path\": \"$TEST_FILE\"}" \
    --tool-response '{}')

  echo "$input" | bash "$PATCHED_HOOK"
  sleep 0.5

  assert_mock_not_received POST /api/v1/code-changes/
}

@test "code-change-capture: captures Edit with file_path and diffs" {
  local input
  input=$(build_hook_input \
    --session-id "test-sess-edit" \
    --tool-name "Edit" \
    --tool-input "{\"file_path\": \"$TEST_FILE\", \"old_string\": \"hello world\", \"new_string\": \"goodbye world\"}" \
    --tool-response '{}')

  echo "$input" | bash "$PATCHED_HOOK"
  sleep 1

  assert_mock_received POST /api/v1/code-changes/
  assert_mock_received_json POST /api/v1/code-changes/ '.session_id == "test-sess-edit"'
  assert_mock_received_json POST /api/v1/code-changes/ '.file_path != ""'
  assert_mock_received_json POST /api/v1/code-changes/ '.tool_name == "Edit"'
  assert_mock_received_json POST /api/v1/code-changes/ '.diff_content | contains("-hello world")'
  assert_mock_received_json POST /api/v1/code-changes/ '.diff_content | contains("+goodbye world")'
}

@test "code-change-capture: captures MultiEdit with file_path" {
  local input
  input=$(build_hook_input \
    --session-id "test-sess-multi" \
    --tool-name "MultiEdit" \
    --tool-input "{\"file_path\": \"$TEST_FILE\", \"edits\": [{\"old_string\": \"foo\", \"new_string\": \"bar\"}, {\"old_string\": \"baz\", \"new_string\": \"qux\"}]}" \
    --tool-response '{}')

  echo "$input" | bash "$PATCHED_HOOK"
  sleep 1

  assert_mock_received POST /api/v1/code-changes/
  assert_mock_received_json POST /api/v1/code-changes/ '.session_id == "test-sess-multi"'
  assert_mock_received_json POST /api/v1/code-changes/ '.tool_name == "MultiEdit"'
  assert_mock_received_json POST /api/v1/code-changes/ '.diff_content | contains("-foo")'
  assert_mock_received_json POST /api/v1/code-changes/ '.diff_content | contains("+bar")'
}

@test "code-change-capture: truncates large diffs to ~50KB" {
  # Create a large old_string (>50KB)
  local large_string
  large_string=$(python3 -c "print('x' * 60000)")
  local input
  input=$(build_hook_input \
    --session-id "test-sess-trunc" \
    --tool-name "Edit" \
    --tool-input "{\"file_path\": \"$TEST_FILE\", \"old_string\": \"$large_string\", \"new_string\": \"small\"}" \
    --tool-response '{}')

  echo "$input" | bash "$PATCHED_HOOK"
  sleep 1

  assert_mock_received POST /api/v1/code-changes/
  assert_mock_received_json POST /api/v1/code-changes/ '.diff_content | contains("truncated")'
}

@test "code-change-capture: handles HTTP 500 gracefully" {
  stop_mock_server
  start_mock_server "error-500"
  _create_patched_hook

  local input
  input=$(build_hook_input \
    --session-id "test-sess-500" \
    --tool-name "Edit" \
    --tool-input "{\"file_path\": \"$TEST_FILE\", \"old_string\": \"a\", \"new_string\": \"b\"}" \
    --tool-response '{}')

  # Hook should not crash on HTTP 500
  run bash -c "echo '$input' | bash '$PATCHED_HOOK'"
  [ "$status" -eq 0 ]
}

@test "code-change-capture: retries on HTTP 000 (connection refused)" {
  # Stop mock server so curl gets connection refused (HTTP 000)
  stop_mock_server

  local input
  input=$(build_hook_input \
    --session-id "test-sess-retry" \
    --tool-name "Edit" \
    --tool-input "{\"file_path\": \"$TEST_FILE\", \"old_string\": \"x\", \"new_string\": \"y\"}" \
    --tool-response '{}')

  # Hook retries 3 times then exits cleanly — should not hang
  # Use a short timeout to prevent test from blocking
  run timeout 20 bash -c "echo '$input' | bash '$PATCHED_HOOK'"
  [ "$status" -eq 0 ]
}

@test "code-change-capture: file checkpoint POST fires in background" {
  # Create a git repo with the test file so the checkpoint logic can read it
  local git_dir
  git_dir="$(mktemp -d)"
  local git_file="$git_dir/test-file.txt"
  echo "original content" > "$git_file"
  cd "$git_dir"
  git init -q
  git config user.email "test@test.com"
  git config user.name "Test"
  git add test-file.txt
  git commit -q -m "init"

  # Now modify the file (simulating what Edit does)
  echo "modified content" > "$git_file"

  local input
  input=$(build_hook_input \
    --session-id "test-sess-cp" \
    --tool-name "Edit" \
    --tool-input "{\"file_path\": \"$git_file\", \"old_string\": \"original content\", \"new_string\": \"modified content\"}" \
    --tool-response '{}')

  echo "$input" | bash "$PATCHED_HOOK"
  # Wait for background checkpoint process
  sleep 2

  # Checkpoint POST should have been made
  assert_mock_received POST /api/v1/file-checkpoints/

  # Cleanup
  rm -rf "$git_dir"
}

@test "code-change-capture: exits cleanly when IMMORTERM_MEMORY_URL is unreachable" {
  # Point to a port that nothing listens on
  local dead_hook
  dead_hook="$(mktemp)"
  cp "$HOOKS_DIR/$HOOK_NAME" "$dead_hook"
  chmod +x "$dead_hook"

  local input
  input=$(build_hook_input \
    --session-id "test-sess-dead" \
    --tool-name "Edit" \
    --tool-input "{\"file_path\": \"$TEST_FILE\", \"old_string\": \"a\", \"new_string\": \"b\"}" \
    --tool-response '{}')

  local saved_port="$IMMORTERM_MEMORY_PORT"
  export IMMORTERM_MEMORY_PORT=1
  run timeout 20 bash -c "echo '$input' | bash '$dead_hook'"
  export IMMORTERM_MEMORY_PORT="$saved_port"
  [ "$status" -eq 0 ]

  rm -f "$dead_hook"
}

@test "code-change-capture: skips when tool_response has error" {
  local input
  input=$(build_hook_input \
    --session-id "test-sess-err" \
    --tool-name "Edit" \
    --tool-input "{\"file_path\": \"$TEST_FILE\", \"old_string\": \"a\", \"new_string\": \"b\"}" \
    --tool-response '{"error": "File not found"}')

  echo "$input" | bash "$PATCHED_HOOK"
  sleep 0.5

  assert_mock_not_received POST /api/v1/code-changes/
}

@test "code-change-capture: JSON body contains required fields" {
  local input
  input=$(build_hook_input \
    --session-id "test-sess-fields" \
    --tool-name "Edit" \
    --tool-input "{\"file_path\": \"$TEST_FILE\", \"old_string\": \"alpha\", \"new_string\": \"beta\"}" \
    --tool-response '{}')

  echo "$input" | bash "$PATCHED_HOOK"
  sleep 1

  assert_mock_received POST /api/v1/code-changes/
  # Verify all required fields are present
  assert_mock_received_json POST /api/v1/code-changes/ '.id != null'
  assert_mock_received_json POST /api/v1/code-changes/ '.session_id == "test-sess-fields"'
  assert_mock_received_json POST /api/v1/code-changes/ '.file_path != null'
  assert_mock_received_json POST /api/v1/code-changes/ '.tool_name == "Edit"'
  assert_mock_received_json POST /api/v1/code-changes/ '.file_action == "modified"'
  assert_mock_received_json POST /api/v1/code-changes/ '.diff_content != null'
  assert_mock_received_json POST /api/v1/code-changes/ '.lines_added >= 0'
  assert_mock_received_json POST /api/v1/code-changes/ '.lines_removed >= 0'
  assert_mock_received_json POST /api/v1/code-changes/ '.timestamp != null'
}
