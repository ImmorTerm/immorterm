#!/usr/bin/env bats
# Tests for .claude/hooks/immorterm-bg-memory-save.sh
#
# This hook is called via Bash with run_in_background: true.
# It takes positional arguments: <category> <text>
# POSTs to /api/v1/memories/ with metadata.

load test_helper

HOOK_NAME="immorterm-bg-memory-save.sh"

setup() {
  start_mock_server
  create_test_project

  PATCHED_HOOK="$TEST_PROJECT_ROOT/.claude/hooks/$HOOK_NAME"
}

teardown() {
  stop_mock_server
  cleanup_test_project
}

@test "saves memory with correct category in payload" {
  run bash "$PATCHED_HOOK" \
    "architecture" "We chose PostgreSQL because of its JSONB support"

  [ "$status" -eq 0 ]

  assert_mock_received POST /api/v1/memories/

  # Verify category is set correctly in metadata
  assert_mock_received_json POST /api/v1/memories/ \
    '.metadata.category == "architecture"'

  # Verify categories array includes the category
  assert_mock_received_json POST /api/v1/memories/ \
    '.metadata.categories[0] == "architecture"'

  # Verify the text is preserved
  assert_mock_received_json POST /api/v1/memories/ \
    '.text == "We chose PostgreSQL because of its JSONB support"'

  # Verify type is history_ref
  assert_mock_received_json POST /api/v1/memories/ \
    '.metadata.type == "history_ref"'
}

@test "detects PLANNED: prefix and adds status metadata" {
  run bash "$PATCHED_HOOK" \
    "decisions" "PLANNED: Migrate from SQLite to PostgreSQL"

  [ "$status" -eq 0 ]

  assert_mock_received POST /api/v1/memories/

  # Verify status is set to "planned"
  assert_mock_received_json POST /api/v1/memories/ \
    '.metadata.status == "planned"'

  # Verify the full text (with prefix) is preserved
  assert_mock_received_json POST /api/v1/memories/ \
    '.text == "PLANNED: Migrate from SQLite to PostgreSQL"'
}

@test "exits 0 when no arguments provided" {
  # No args at all — should fail with usage error (exit 1 from bash :? expansion)
  run bash "$PATCHED_HOOK"

  # The :? expansion triggers exit 1
  [ "$status" -ne 0 ]

  # No request should have been made
  assert_mock_not_received POST /api/v1/memories/
}
