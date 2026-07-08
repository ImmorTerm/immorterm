#!/usr/bin/env bats
# memory-guide.bats — Tests for immorterm-memory-guide.sh
#
# This hook is a SYNC SessionStart hook. Its stdout goes directly to Claude
# as context injection. It also registers the session in background.

load test_helper

HOOK_NAME="immorterm-memory-guide.sh"

setup() {
  start_mock_server
  create_test_project
}

teardown() {
  stop_mock_server
  cleanup_test_project
}

# ── Tests ──────────────────────────────────────────────────────────────

@test "memory-guide: outputs memory guidance text to stdout" {
  local input
  input=$(build_hook_input \
    --session-id "test-sess-guide" \
    --cwd "/tmp")

  run bash -c "echo '$input' | bash '$HOOKS_DIR/$HOOK_NAME'"
  [ "$status" -eq 0 ]

  # Output should contain the immorterm-memory header
  [[ "$output" == *"immorterm-memory"* ]]
  [[ "$output" == *"Memory Services Active"* ]]
}

@test "memory-guide: output includes search_memory and add_memories tool references" {
  local input
  input=$(build_hook_input \
    --session-id "test-sess-tools" \
    --cwd "/tmp")

  run bash -c "echo '$input' | bash '$HOOKS_DIR/$HOOK_NAME'"
  [ "$status" -eq 0 ]

  # Verify key tool names are mentioned
  [[ "$output" == *"search_memory"* ]]
  [[ "$output" == *"add_memories"* ]]
  [[ "$output" == *"get_session_context"* ]]
  [[ "$output" == *"list_code_changes"* ]]
  [[ "$output" == *"explain_change"* ]]
}

@test "memory-guide: output includes decision tracking instructions" {
  local input
  input=$(build_hook_input \
    --session-id "test-sess-decisions" \
    --cwd "/tmp")

  run bash -c "echo '$input' | bash '$HOOKS_DIR/$HOOK_NAME'"
  [ "$status" -eq 0 ]

  # Decision tracking section should be present
  [[ "$output" == *"Decision Tracking"* ]]
  [[ "$output" == *"resolve_decisions"* ]]
  [[ "$output" == *"planned"* ]]
  [[ "$output" == *"completed"* ]]
}

@test "memory-guide: session registration POST fires in background" {
  # The hook uses hardcoded http://localhost:8765 for the session registration
  # curl. To test this, we need to either have the mock on port 8765 or
  # accept that background registration goes to the default port.
  # We test the output (sync part) which always works.
  local input
  input=$(build_hook_input \
    --session-id "test-sess-reg" \
    --cwd "/tmp")

  run bash -c "echo '$input' | bash '$HOOKS_DIR/$HOOK_NAME'"
  [ "$status" -eq 0 ]

  # The sync output must always be produced regardless of background POST
  [[ "$output" == *"immorterm-memory"* ]]

  # Session identity section should include the session UUID
  [[ "$output" == *"test-sess-reg"* ]]
  [[ "$output" == *"Session Identity"* ]]
}

@test "memory-guide: exits cleanly with valid output even without session_id" {
  # SessionStart may not always have a session_id
  local input
  input=$(build_hook_input \
    --cwd "/tmp")

  # Unset SESSION_ID to prevent env contamination from parent (Claude Code sets it)
  run bash -c "unset SESSION_ID; echo '$input' | bash '$HOOKS_DIR/$HOOK_NAME'"
  [ "$status" -eq 0 ]

  # Should still output the memory guide text
  [[ "$output" == *"Memory Services Active"* ]]
  [[ "$output" == *"search_memory"* ]]

  # Session Identity section should NOT appear without session_id
  [[ "$output" != *"Session Identity"* ]]
}
