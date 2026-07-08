#!/usr/bin/env bats
# Tests for .claude/hooks/immorterm-category-inject.sh — SubagentStart sync hook
#
# This hook:
#   - Maps subagent_type to memory categories (e.g., "frontend" -> frontend, "analyzer" -> debugging)
#   - Fetches relevant memories via POST /api/v1/memories/search
#   - Outputs hookSpecificOutput JSON with additionalContext field to stdout
#   - Exits cleanly when subagent_type is not recognized (falls through to wildcard case)
#   - Exits cleanly when IMMORTERM_MEMORY_URL is unreachable

load test_helper

HOOK_NAME="immorterm-category-inject.sh"

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
}

teardown() {
  stop_mock_server
  cleanup_test_project
}

@test "category-inject: known subagent_type produces hookSpecificOutput on stdout" {
  local input
  input=$(build_hook_input \
    --session-id "test-session-ci" \
    --subagent-type "frontend")

  run bash -c "echo '$input' | bash '$PATCHED_HOOK'"

  [ "$status" -eq 0 ]
  [[ "$output" == *"hookSpecificOutput"* ]]
}

@test "category-inject: output contains additionalContext field" {
  local input
  input=$(build_hook_input \
    --session-id "test-session-ci" \
    --subagent-type "backend")

  run bash -c "echo '$input' | bash '$PATCHED_HOOK'"

  [ "$status" -eq 0 ]
  # Validate the JSON structure
  echo "$output" | python3 -c "
import json, sys
data = json.load(sys.stdin)
assert 'hookSpecificOutput' in data, 'Missing hookSpecificOutput'
assert 'additionalContext' in data['hookSpecificOutput'], 'Missing additionalContext'
assert 'immorterm-memory' in data['hookSpecificOutput']['additionalContext'], 'Missing immorterm-memory tag'
"
}

@test "category-inject: search request is sent to OpenMemory" {
  local input
  input=$(build_hook_input \
    --session-id "test-session-ci" \
    --subagent-type "security")

  echo "$input" | bash "$PATCHED_HOOK"
  sleep 0.5

  # Should exit 0 regardless
  assert_mock_received POST /api/v1/memories/search
}

@test "category-inject: exits 0 when IMMORTERM_MEMORY_URL is unreachable" {
  # Create a hook pointing to a dead port
  local dead_hook
  dead_hook="$(mktemp)"
  cp "$HOOKS_DIR/$HOOK_NAME" "$dead_hook"
  chmod +x "$dead_hook"

  local input
  input=$(build_hook_input \
    --session-id "test-session-ci" \
    --subagent-type "frontend")

  # With no server, curl will fail, MEMORIES will be empty, hook exits 0 silently
  local saved_port="$IMMORTERM_MEMORY_PORT"
  export IMMORTERM_MEMORY_PORT=19999
  run bash -c "echo '$input' | bash '$dead_hook'"
  export IMMORTERM_MEMORY_PORT="$saved_port"

  [ "$status" -eq 0 ]

  rm -f "$dead_hook"
}
